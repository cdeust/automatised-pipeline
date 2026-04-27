// parser::swift — tree-sitter-based Swift source parser for code-intelligence graph.
//
// Parses ``.swift`` files and extracts class/struct/enum/protocol/extension
// declarations plus functions, methods, properties, imports, and calls.
//
// Grammar reference: https://github.com/alex-pinkus/tree-sitter-swift

use tree_sitter::{Node, Parser};

use super::{
    node_field_text, node_text, qual, ExtractedNode, ExtractedRef, ParseResult, LABEL_CALL_SITE,
    LABEL_CONSTANT, LABEL_ENUM, LABEL_FUNCTION, LABEL_IMPORT, LABEL_METHOD, LABEL_STRUCT,
    LABEL_TRAIT,
};

const TS_CLASS_DECL: &str = "class_declaration";
const TS_PROTOCOL_DECL: &str = "protocol_declaration";
const TS_FUNCTION_DECL: &str = "function_declaration";
const TS_PROPERTY_DECL: &str = "property_declaration";
const TS_IMPORT_DECL: &str = "import_declaration";
const TS_TYPEALIAS_DECL: &str = "typealias_declaration";
const TS_CALL_EXPR: &str = "call_expression";

pub fn parse_swift_file(source: &str, file_path: &str) -> Result<ParseResult, String> {
    let lang: tree_sitter::Language = tree_sitter_swift::LANGUAGE.into();
    let mut parser = Parser::new();
    parser
        .set_language(&lang)
        .map_err(|e| format!("failed to set Swift language: {e}"))?;
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
    next_seq: u64,
}

// ``class_declaration`` is the Swift grammar's umbrella for class, struct,
// actor, extension, and enum. Disambiguate by the leading keyword in the
// node's raw text — tree-sitter-swift doesn't expose a dedicated child
// field for the kind of aggregate.
fn classify_class(source: &str, node: Node) -> (&'static str, bool) {
    let head = node_text(source, node);
    let head = head.trim_start();
    if head.starts_with("struct") || head.starts_with("public struct") || head.starts_with("internal struct") || head.starts_with("fileprivate struct") || head.starts_with("private struct") {
        return (LABEL_STRUCT, false);
    }
    if head.starts_with("enum") || head.contains(" enum ") {
        return (LABEL_ENUM, false);
    }
    if head.starts_with("extension") {
        return (LABEL_STRUCT, true);
    }
    if head.starts_with("actor") {
        return (LABEL_STRUCT, false);
    }
    (LABEL_STRUCT, false)
}

fn visibility_modifier(source: &str, node: Node) -> String {
    let text = node_text(source, node);
    let head = text.lines().next().unwrap_or("");
    for v in ["public", "private", "internal", "fileprivate", "open"] {
        if head.split_whitespace().any(|w| w == v) {
            return v.to_string();
        }
    }
    "internal".to_string()
}

fn extract_children(ctx: &mut Ctx, parent: Node, scope: &str, enclosing_type: Option<&str>) {
    let mut cursor = parent.walk();
    for child in parent.children(&mut cursor) {
        match child.kind() {
            TS_CLASS_DECL => extract_class_like(ctx, child, scope),
            TS_PROTOCOL_DECL => extract_protocol(ctx, child, scope),
            TS_FUNCTION_DECL => extract_function(ctx, child, scope, enclosing_type),
            TS_PROPERTY_DECL => extract_property(ctx, child, scope),
            TS_IMPORT_DECL => extract_import(ctx, child, scope),
            TS_TYPEALIAS_DECL => extract_typealias(ctx, child, scope),
            _ => {
                // Swift top-level is a sequence of statements — recurse into
                // compound groupings so we pick up nested decls inside
                // guards / do-blocks / computed-property bodies.
                if child.named_child_count() > 0 {
                    extract_children(ctx, child, scope, enclosing_type);
                }
            }
        }
    }
}

fn find_name(source: &str, node: Node) -> String {
    // Try the canonical field first.
    let n = node_field_text(source, node, "name");
    if !n.is_empty() {
        return n;
    }
    // Fall back: first ``type_identifier`` or ``simple_identifier``.
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        let k = c.kind();
        if k == "type_identifier" || k == "simple_identifier" || k == "identifier" {
            let t = node_text(source, c);
            if !t.is_empty() {
                return t;
            }
        }
    }
    String::new()
}

fn extract_class_like(ctx: &mut Ctx, node: Node, scope: &str) {
    let (label, is_extension) = classify_class(ctx.source, node);
    let name = find_name(ctx.source, node);
    if name.is_empty() {
        return;
    }
    let qn = qual(scope, &name);
    // Extensions don't create a new type — we still emit a node so the
    // methods inside have a parent, but mark it so downstream tooling
    // can merge.
    let mut props = Vec::new();
    if is_extension {
        props.push(("is_extension".to_string(), "true".to_string()));
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
    ctx.refs.push(ExtractedRef {
        kind: "Defines".to_string(),
        from_qualified_name: scope.to_string(),
        to_qualified_name: qn.clone(),
    });
    if let Some(body) = node
        .child_by_field_name("body")
        .or_else(|| find_block_child(node))
    {
        extract_children(ctx, body, &qn, Some(&qn));
    }
}

fn find_block_child(node: Node) -> Option<Node> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        let k = child.kind();
        if k.ends_with("_body") || k == "class_body" || k == "enum_class_body" {
            return Some(child);
        }
    }
    None
}

fn extract_protocol(ctx: &mut Ctx, node: Node, scope: &str) {
    let name = find_name(ctx.source, node);
    if name.is_empty() {
        return;
    }
    let qn = qual(scope, &name);
    ctx.nodes.push(ExtractedNode {
        label: LABEL_TRAIT.to_string(),
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
    if let Some(body) = node
        .child_by_field_name("body")
        .or_else(|| find_block_child(node))
    {
        extract_children(ctx, body, &qn, Some(&qn));
    }
}

fn extract_function(ctx: &mut Ctx, node: Node, scope: &str, enclosing_type: Option<&str>) {
    let name = find_name(ctx.source, node);
    if name.is_empty() {
        return;
    }
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
    if let Some(body) = node
        .child_by_field_name("body")
        .or_else(|| find_block_child(node))
    {
        extract_calls(ctx, body, &qn);
    }
}

fn extract_property(ctx: &mut Ctx, node: Node, scope: &str) {
    let name = find_name(ctx.source, node);
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

fn extract_typealias(ctx: &mut Ctx, node: Node, scope: &str) {
    let name = find_name(ctx.source, node);
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
        properties: vec![("typealias".to_string(), "true".to_string())],
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
        .to_string();
    if cleaned.is_empty() {
        return;
    }
    let qn = qual(scope, &format!("import:{cleaned}"));
    ctx.nodes.push(ExtractedNode {
        label: LABEL_IMPORT.to_string(),
        name: cleaned.clone(),
        qualified_name: qn,
        start_line: node.start_position().row as u64 + 1,
        end_line: node.end_position().row as u64 + 1,
        visibility: "internal".to_string(),
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
            let callee_text = n
                .named_child(0)
                .map(|c| node_text(ctx.source, c))
                .unwrap_or_default();
            let tail = callee_text
                .rsplit('.')
                .next()
                .unwrap_or("")
                .trim_end_matches('(')
                .trim()
                .to_string();
            if !tail.is_empty()
                && tail.chars().next().map_or(false, |c| c.is_alphabetic() || c == '_')
            {
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
                    name: tail.clone(),
                    qualified_name: site_qn.clone(),
                    start_line: n.start_position().row as u64 + 1,
                    end_line: n.end_position().row as u64 + 1,
                    visibility: "internal".to_string(),
                    properties: vec![("callee_name".to_string(), tail.clone())],
                });
                ctx.refs.push(ExtractedRef {
                    kind: "Calls".to_string(),
                    from_qualified_name: caller_qn.to_string(),
                    to_qualified_name: tail,
                });
            }
        }
        let mut cursor = n.walk();
        for c in n.children(&mut cursor) {
            stack.push(c);
        }
    }
}
