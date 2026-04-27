// parser::kotlin — tree-sitter-based Kotlin source parser for code-intelligence graph.
//
// Parses ``.kt`` / ``.kts`` files using tree-sitter-kotlin-ng and emits
// the same ParseResult shape as the sister parsers.
//
// Grammar reference: https://github.com/fwcd/tree-sitter-kotlin
// (the -ng fork we depend on is an actively maintained descendant.)

use tree_sitter::{Node, Parser};

use super::{
    node_field_text, node_text, qual, ExtractedNode, ExtractedRef, ParseResult, LABEL_CALL_SITE,
    LABEL_CONSTANT, LABEL_ENUM, LABEL_FUNCTION, LABEL_IMPORT, LABEL_METHOD, LABEL_STRUCT,
    LABEL_TRAIT,
};

const TS_PACKAGE_HEADER: &str = "package_header";
const TS_IMPORT_HEADER: &str = "import_header";
const TS_CLASS_DECL: &str = "class_declaration";
const TS_OBJECT_DECL: &str = "object_declaration";
const TS_FUNCTION_DECL: &str = "function_declaration";
const TS_PROPERTY_DECL: &str = "property_declaration";
const TS_CLASS_BODY: &str = "class_body";
const TS_OBJECT_BODY: &str = "object_body";
const TS_ENUM_ENTRY: &str = "enum_entry";
const TS_CALL_EXPR: &str = "call_expression";

pub fn parse_kotlin_file(source: &str, file_path: &str) -> Result<ParseResult, String> {
    let lang: tree_sitter::Language = tree_sitter_kotlin_ng::LANGUAGE.into();
    let mut parser = Parser::new();
    parser
        .set_language(&lang)
        .map_err(|e| format!("failed to set Kotlin language: {e}"))?;
    parser.set_timeout_micros(super::PARSE_TIMEOUT_MICROS);
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| "parse_timeout_or_none: tree-sitter returned None".to_string())?;

    let mut ctx = Ctx {
        source,
        file_path,
        nodes: Vec::new(),
        refs: Vec::new(),
        next_seq: 0,
    };
    extract_children(&mut ctx, tree.root_node(), file_path, None);
    Ok(ParseResult {
        nodes: ctx.nodes,
        refs: ctx.refs,
    })
}

struct Ctx<'a> {
    source: &'a str,
    #[allow(dead_code)]
    file_path: &'a str,
    nodes: Vec<ExtractedNode>,
    refs: Vec<ExtractedRef>,
    // Per-file monotonic counter used as the last segment of call-site
    // and overloaded-method qualified_names. Two overloaded Kotlin funs
    // ``fun toDomain(x: A)`` and ``fun toDomain(x: B)`` would otherwise
    // collide on the LadybugDB primary key; the counter disambiguates.
    next_seq: u64,
}

// Kotlin declaration kinds the grammar exposes as ``class_declaration``
// are distinguished by a leading modifier: ``interface``, ``enum class``,
// ``data class``, etc. We walk the node text once to classify.
fn classify_class(source: &str, node: Node) -> &'static str {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        let kind = child.kind();
        if kind == "interface" {
            return LABEL_TRAIT;
        }
        if kind == "enum" {
            return LABEL_ENUM;
        }
        // ``modifiers`` node contains ``annotation``, ``class_modifier`` etc.
        if kind == "modifiers" {
            let text = node_text(source, child);
            if text.contains("enum") {
                return LABEL_ENUM;
            }
        }
    }
    LABEL_STRUCT
}

fn visibility_modifier(source: &str, node: Node) -> String {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "modifiers" {
            let t = node_text(source, child);
            for v in ["public", "private", "protected", "internal"] {
                if t.split_whitespace().any(|w| w == v) {
                    return v.to_string();
                }
            }
        }
    }
    // Kotlin default is ``public``.
    "public".to_string()
}

fn extract_children(ctx: &mut Ctx, parent: Node, scope: &str, enclosing_type: Option<&str>) {
    let mut cursor = parent.walk();
    for child in parent.children(&mut cursor) {
        match child.kind() {
            TS_CLASS_DECL | TS_OBJECT_DECL => extract_class_like(ctx, child, scope),
            TS_FUNCTION_DECL => extract_function(ctx, child, scope, enclosing_type),
            TS_PROPERTY_DECL => extract_property(ctx, child, scope),
            TS_IMPORT_HEADER => extract_import(ctx, child, scope),
            TS_PACKAGE_HEADER => {}
            TS_ENUM_ENTRY => {
                // Enum variant — emit as a constant of the enum type.
                let name = node_field_text(ctx.source, child, "simple_identifier");
                let name = if name.is_empty() {
                    first_identifier(ctx.source, child)
                } else {
                    name
                };
                if !name.is_empty() {
                    let qn = qual(scope, &name);
                    ctx.nodes.push(ExtractedNode {
                        label: LABEL_CONSTANT.to_string(),
                        name: name.clone(),
                        qualified_name: qn.clone(),
                        start_line: child.start_position().row as u64 + 1,
                        end_line: child.end_position().row as u64 + 1,
                        visibility: "public".to_string(),
                        properties: vec![("enum_entry".to_string(), "true".to_string())],
                    });
                    ctx.refs.push(ExtractedRef {
                        kind: "Defines".to_string(),
                        from_qualified_name: scope.to_string(),
                        to_qualified_name: qn,
                    });
                }
            }
            TS_CLASS_BODY | TS_OBJECT_BODY => {
                extract_children(ctx, child, scope, enclosing_type);
            }
            _ => {}
        }
    }
}

fn first_identifier(source: &str, node: Node) -> String {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "simple_identifier" || child.kind() == "identifier" {
            return node_text(source, child);
        }
    }
    String::new()
}

fn extract_class_like(ctx: &mut Ctx, node: Node, scope: &str) {
    let name = node_field_text(ctx.source, node, "name");
    let name = if name.is_empty() {
        first_identifier(ctx.source, node)
    } else {
        name
    };
    if name.is_empty() {
        return;
    }
    let label = classify_class(ctx.source, node);
    let qn = qual(scope, &name);
    ctx.nodes.push(ExtractedNode {
        label: label.to_string(),
        name: name.clone(),
        qualified_name: qn.clone(),
        start_line: node.start_position().row as u64 + 1,
        end_line: node.end_position().row as u64 + 1,
        visibility: visibility_modifier(ctx.source, node),
        properties: Vec::new(),
    });
    ctx.refs.push(ExtractedRef {
        kind: "Defines".to_string(),
        from_qualified_name: scope.to_string(),
        to_qualified_name: qn.clone(),
    });
    // Supertype list — ``: Parent, Interface`` after the class name.
    if let Some(supers) = node.child_by_field_name("delegation_specifiers") {
        let text = node_text(ctx.source, supers);
        for piece in text
            .split(',')
            .map(|s| s.trim().trim_end_matches("()"))
            .filter(|s| !s.is_empty())
        {
            ctx.refs.push(ExtractedRef {
                kind: "Extends".to_string(),
                from_qualified_name: qn.clone(),
                to_qualified_name: piece.to_string(),
            });
        }
    }
    // Body — recurse with this type as the enclosing scope.
    // Fall back from the ``body`` field to a child of the expected kind
    // via a manual walk (the cursor-borrow pattern can't be closed over).
    let body = node.child_by_field_name("body").or_else(|| {
        let mut cursor = node.walk();
        let mut found = None;
        for c in node.children(&mut cursor) {
            if c.kind() == TS_CLASS_BODY || c.kind() == TS_OBJECT_BODY {
                found = Some(c);
                break;
            }
        }
        found
    });
    if let Some(body) = body {
        extract_children(ctx, body, &qn, Some(&qn));
    }
}

fn extract_function(ctx: &mut Ctx, node: Node, scope: &str, enclosing_type: Option<&str>) {
    let name = node_field_text(ctx.source, node, "name");
    let name = if name.is_empty() {
        first_identifier(ctx.source, node)
    } else {
        name
    };
    if name.is_empty() {
        return;
    }
    // Disambiguate overloaded functions by appending a per-file sequence
    // number — the primary key must be unique across all Function and
    // Method nodes in the graph, and LadybugDB rejects duplicates at
    // insert time.
    let seq = {
        ctx.next_seq += 1;
        ctx.next_seq
    };
    let qn = format!("{}::{}#{}", scope, name, seq);
    let label = if enclosing_type.is_some() {
        LABEL_METHOD
    } else {
        LABEL_FUNCTION
    };
    let mut props = Vec::new();
    if let Some(rec) = enclosing_type {
        props.push(("receiver_type".to_string(), rec.to_string()));
    }
    // Kotlin extension functions declare a receiver: ``fun String.foo()``.
    if let Some(recv) = node.child_by_field_name("receiver") {
        props.push((
            "extension_receiver".to_string(),
            node_text(ctx.source, recv),
        ));
    }
    ctx.nodes.push(ExtractedNode {
        label: label.to_string(),
        name: name.clone(),
        qualified_name: qn.clone(),
        start_line: node.start_position().row as u64 + 1,
        end_line: node.end_position().row as u64 + 1,
        visibility: visibility_modifier(ctx.source, node),
        properties: props,
    });
    let edge_kind = if enclosing_type.is_some() {
        "HasMethod"
    } else {
        "Defines"
    };
    ctx.refs.push(ExtractedRef {
        kind: edge_kind.to_string(),
        from_qualified_name: scope.to_string(),
        to_qualified_name: qn.clone(),
    });
    if let Some(body) = node.child_by_field_name("body") {
        extract_calls(ctx, body, &qn);
    } else {
        // Expression-bodied function: ``fun f() = x.bar()``.
        extract_calls(ctx, node, &qn);
    }
}

fn extract_property(ctx: &mut Ctx, node: Node, scope: &str) {
    let name = first_identifier(ctx.source, node);
    if name.is_empty() {
        return;
    }
    let qn = qual(scope, &name);
    ctx.nodes.push(ExtractedNode {
        label: LABEL_CONSTANT.to_string(),
        name: name.clone(),
        qualified_name: qn.clone(),
        start_line: node.start_position().row as u64 + 1,
        end_line: node.end_position().row as u64 + 1,
        visibility: visibility_modifier(ctx.source, node),
        properties: Vec::new(),
    });
    ctx.refs.push(ExtractedRef {
        kind: "Defines".to_string(),
        from_qualified_name: scope.to_string(),
        to_qualified_name: qn,
    });
}

fn extract_import(ctx: &mut Ctx, node: Node, scope: &str) {
    let text = node_text(ctx.source, node);
    let cleaned = text
        .trim()
        .trim_start_matches("import")
        .trim()
        .trim_end_matches(';')
        .trim()
        .to_string();
    if cleaned.is_empty() {
        return;
    }
    let name = cleaned
        .rsplit('.')
        .next()
        .unwrap_or("")
        .trim_end_matches('*')
        .to_string();
    let qn = qual(scope, &format!("import:{cleaned}"));
    ctx.nodes.push(ExtractedNode {
        label: LABEL_IMPORT.to_string(),
        name,
        qualified_name: qn,
        start_line: node.start_position().row as u64 + 1,
        end_line: node.end_position().row as u64 + 1,
        visibility: "public".to_string(),
        properties: vec![("target".to_string(), cleaned.clone())],
    });
    ctx.refs.push(ExtractedRef {
        kind: "Imports".to_string(),
        from_qualified_name: scope.to_string(),
        to_qualified_name: cleaned,
    });
}

fn extract_calls(ctx: &mut Ctx, root: Node, caller_qn: &str) {
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if n.kind() == TS_CALL_EXPR {
            // In tree-sitter-kotlin-ng the callee is the first named child.
            let callee = if let Some(c) = n.child_by_field_name("callee") {
                node_text(ctx.source, c)
            } else {
                let mut cursor = n.walk();
                let first = n.children(&mut cursor).next();
                first
                    .map(|c| node_text(ctx.source, c))
                    .unwrap_or_default()
            };
            // Keep only the tail identifier to match the file::name convention
            // used by the rest of the graph.
            let callee_tail = callee
                .rsplit('.')
                .next()
                .unwrap_or("")
                .trim_end_matches('(')
                .to_string();
            if !callee_tail.is_empty() && callee_tail.chars().next().map_or(false, |c| c.is_alphabetic() || c == '_') {
                let seq = {
                    ctx.next_seq += 1;
                    ctx.next_seq
                };
                let site_qn = format!(
                    "{}::call@{}:{}#{}",
                    caller_qn,
                    n.start_position().row + 1,
                    n.start_position().column + 1,
                    seq,
                );
                ctx.nodes.push(ExtractedNode {
                    label: LABEL_CALL_SITE.to_string(),
                    name: callee_tail.clone(),
                    qualified_name: site_qn.clone(),
                    start_line: n.start_position().row as u64 + 1,
                    end_line: n.end_position().row as u64 + 1,
                    visibility: "public".to_string(),
                    properties: vec![("callee_name".to_string(), callee_tail.clone())],
                });
                ctx.refs.push(ExtractedRef {
                    kind: "Calls".to_string(),
                    from_qualified_name: caller_qn.to_string(),
                    to_qualified_name: callee_tail,
                });
            }
        }
        let mut cursor = n.walk();
        for c in n.children(&mut cursor) {
            stack.push(c);
        }
    }
}
