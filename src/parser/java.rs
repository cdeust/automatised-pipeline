// parser::java — tree-sitter-based Java source parser for code-intelligence graph.
//
// Parses a single `.java` file and extracts typed symbols matching the
// graph_store schema. Shares the ParseResult/ExtractedNode/ExtractedRef
// contract with parser::rust, parser::python, and parser::typescript.
//
// Grammar reference: https://github.com/tree-sitter/tree-sitter-java

use tree_sitter::{Node, Parser};

use super::{
    node_field_text, node_text, qual, ExtractedNode, ExtractedRef, ParseResult, LABEL_CALL_SITE,
    LABEL_CONSTANT, LABEL_ENUM, LABEL_FUNCTION, LABEL_IMPORT, LABEL_METHOD, LABEL_STRUCT,
    LABEL_TRAIT,
};

// Tree-sitter node type constants — from
// https://github.com/tree-sitter/tree-sitter-java/blob/master/src/node-types.json
const TS_CLASS: &str = "class_declaration";
const TS_INTERFACE: &str = "interface_declaration";
const TS_ENUM: &str = "enum_declaration";
const TS_RECORD: &str = "record_declaration";
const TS_ANNOTATION: &str = "annotation_type_declaration";
const TS_METHOD: &str = "method_declaration";
const TS_CONSTRUCTOR: &str = "constructor_declaration";
const TS_FIELD: &str = "field_declaration";
const TS_IMPORT: &str = "import_declaration";
const TS_PACKAGE: &str = "package_declaration";
const TS_CALL: &str = "method_invocation";
const TS_OBJECT_CREATION: &str = "object_creation_expression";

pub fn parse_java_file(source: &str, file_path: &str) -> Result<ParseResult, String> {
    let lang: tree_sitter::Language = tree_sitter_java::LANGUAGE.into();
    let mut parser = Parser::new();
    parser
        .set_language(&lang)
        .map_err(|e| format!("failed to set Java language: {e}"))?;
    parser.set_timeout_micros(super::PARSE_TIMEOUT_MICROS);
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| "parse_timeout_or_none: tree-sitter returned None".to_string())?;

    let mut ctx = Ctx {
        source,
        file_path,
        nodes: Vec::new(),
        refs: Vec::new(),
        package: String::new(),
        next_seq: 0,
    };
    // Java files carry a ``package X.Y;`` at the top that seeds the scope
    // for every qualified name below; we capture it then prefix-qualify.
    if let Some(pkg) = find_package(tree.root_node(), source) {
        ctx.package = pkg;
    }
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
    package: String,
    // See the note in parser::kotlin — a per-file sequence disambiguates
    // overloaded methods and multiple call sites at the same position
    // so the graph store's primary-key uniqueness holds.
    next_seq: u64,
}

fn find_package(root: Node, source: &str) -> Option<String> {
    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        if child.kind() == TS_PACKAGE {
            let mut inner = child.walk();
            for n in child.children(&mut inner) {
                if n.kind() == "scoped_identifier" || n.kind() == "identifier" {
                    return Some(node_text(source, n).trim().to_string());
                }
            }
        }
    }
    None
}

fn visibility_from_modifiers(source: &str, node: Node) -> String {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "modifiers" {
            let t = node_text(source, child);
            if t.contains("public") {
                return "public".to_string();
            }
            if t.contains("private") {
                return "private".to_string();
            }
            if t.contains("protected") {
                return "protected".to_string();
            }
        }
    }
    // Java default is package-private.
    "package".to_string()
}

fn extract_children(ctx: &mut Ctx, parent: Node, scope: &str, enclosing_type: Option<&str>) {
    let mut cursor = parent.walk();
    for child in parent.children(&mut cursor) {
        match child.kind() {
            TS_CLASS | TS_RECORD => extract_class_like(ctx, child, scope, LABEL_STRUCT),
            TS_INTERFACE | TS_ANNOTATION => {
                extract_class_like(ctx, child, scope, LABEL_TRAIT)
            }
            TS_ENUM => extract_class_like(ctx, child, scope, LABEL_ENUM),
            TS_METHOD | TS_CONSTRUCTOR => {
                extract_method(ctx, child, scope, enclosing_type)
            }
            TS_FIELD => extract_field(ctx, child, scope),
            TS_IMPORT => extract_import(ctx, child, scope),
            // ``class_body`` / ``interface_body`` / ``enum_body`` wrap members.
            _ if child.kind().ends_with("_body") => {
                extract_children(ctx, child, scope, enclosing_type);
            }
            _ => {}
        }
    }
}

fn extract_class_like(ctx: &mut Ctx, node: Node, scope: &str, label: &str) {
    let name = node_field_text(ctx.source, node, "name");
    if name.is_empty() {
        return;
    }
    let qn = qual(scope, &name);
    let visibility = visibility_from_modifiers(ctx.source, node);
    ctx.nodes.push(ExtractedNode {
        label: label.to_string(),
        name: name.clone(),
        qualified_name: qn.clone(),
        start_line: node.start_position().row as u64 + 1,
        end_line: node.end_position().row as u64 + 1,
        visibility,
        properties: Vec::new(),
    });
    ctx.refs.push(ExtractedRef {
        kind: "Defines".to_string(),
        from_qualified_name: scope.to_string(),
        to_qualified_name: qn.clone(),
    });
    // Inheritance / interface implementation.
    if let Some(supers) = node.child_by_field_name("superclass") {
        let name = node_text(ctx.source, supers)
            .trim_start_matches("extends")
            .trim()
            .to_string();
        if !name.is_empty() {
            ctx.refs.push(ExtractedRef {
                kind: "Extends".to_string(),
                from_qualified_name: qn.clone(),
                to_qualified_name: name,
            });
        }
    }
    if let Some(ifaces) = node.child_by_field_name("interfaces") {
        let mut cursor = ifaces.walk();
        for child in ifaces.children(&mut cursor) {
            let k = child.kind();
            if k == "type_identifier" || k == "scoped_type_identifier" {
                let name = node_text(ctx.source, child);
                if !name.is_empty() {
                    ctx.refs.push(ExtractedRef {
                        kind: "Implements".to_string(),
                        from_qualified_name: qn.clone(),
                        to_qualified_name: name,
                    });
                }
            }
        }
    }
    if let Some(body) = node.child_by_field_name("body") {
        extract_children(ctx, body, &qn, Some(&qn));
    }
}

fn extract_method(ctx: &mut Ctx, node: Node, scope: &str, enclosing_type: Option<&str>) {
    let name = node_field_text(ctx.source, node, "name");
    if name.is_empty() {
        return;
    }
    let seq = {
        ctx.next_seq += 1;
        ctx.next_seq
    };
    let qn = format!("{}::{}#{}", scope, name, seq);
    let visibility = visibility_from_modifiers(ctx.source, node);
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
        visibility,
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
    }
}

fn extract_field(ctx: &mut Ctx, node: Node, scope: &str) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "variable_declarator" {
            let name = node_field_text(ctx.source, child, "name");
            if name.is_empty() {
                continue;
            }
            let qn = qual(scope, &name);
            ctx.nodes.push(ExtractedNode {
                label: LABEL_CONSTANT.to_string(),
                name: name.clone(),
                qualified_name: qn.clone(),
                start_line: child.start_position().row as u64 + 1,
                end_line: child.end_position().row as u64 + 1,
                visibility: visibility_from_modifiers(ctx.source, node),
                properties: Vec::new(),
            });
            ctx.refs.push(ExtractedRef {
                kind: "Defines".to_string(),
                from_qualified_name: scope.to_string(),
                to_qualified_name: qn,
            });
        }
    }
}

fn extract_import(ctx: &mut Ctx, node: Node, scope: &str) {
    let text = node_text(ctx.source, node);
    // ``import a.b.C;`` or ``import static a.b.C.method;``
    let cleaned = text
        .trim()
        .trim_start_matches("import")
        .trim()
        .trim_start_matches("static")
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
        .to_string();
    let qn = qual(scope, &format!("import:{cleaned}"));
    ctx.nodes.push(ExtractedNode {
        label: LABEL_IMPORT.to_string(),
        name,
        qualified_name: qn.clone(),
        start_line: node.start_position().row as u64 + 1,
        end_line: node.end_position().row as u64 + 1,
        visibility: "package".to_string(),
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
        if n.kind() == TS_CALL || n.kind() == TS_OBJECT_CREATION {
            let callee = node_field_text(ctx.source, n, "name");
            let callee = if callee.is_empty() {
                // ``new X()`` uses the ``type`` field for the class name.
                node_field_text(ctx.source, n, "type")
            } else {
                callee
            };
            if !callee.is_empty() {
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
                    name: callee.clone(),
                    qualified_name: site_qn.clone(),
                    start_line: n.start_position().row as u64 + 1,
                    end_line: n.end_position().row as u64 + 1,
                    visibility: "package".to_string(),
                    properties: vec![("callee_name".to_string(), callee.clone())],
                });
                ctx.refs.push(ExtractedRef {
                    kind: "Calls".to_string(),
                    from_qualified_name: caller_qn.to_string(),
                    to_qualified_name: callee,
                });
            }
        }
        let mut cursor = n.walk();
        for c in n.children(&mut cursor) {
            stack.push(c);
        }
    }
}
