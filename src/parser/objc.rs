// parser::objc — tree-sitter-based Objective-C source parser.
//
// Handles ``.m`` (Objective-C) and ``.mm`` (Objective-C++). The grammar
// covers ``@interface`` / ``@implementation`` / ``@protocol`` / method
// declarations and definitions, as well as C constructs embedded inside.
//
// Grammar reference: https://github.com/jiyee/tree-sitter-objc

use tree_sitter::{Node, Parser};

use super::{
    node_field_text, node_text, qual, ExtractedNode, ExtractedRef, ParseResult, LABEL_CALL_SITE,
    LABEL_FUNCTION, LABEL_IMPORT, LABEL_METHOD, LABEL_STRUCT, LABEL_TRAIT,
};

const TS_CLASS_INTERFACE: &str = "class_interface";
const TS_CLASS_IMPL: &str = "class_implementation";
const TS_CATEGORY_INTERFACE: &str = "category_interface";
const TS_CATEGORY_IMPL: &str = "category_implementation";
const TS_PROTOCOL_DECL: &str = "protocol_declaration";
const TS_METHOD_DECL: &str = "method_declaration";
const TS_METHOD_DEF: &str = "method_definition";
const TS_FUNCTION_DEF: &str = "function_definition";
const TS_IMPORT: &str = "preproc_include";
const TS_MODULE_IMPORT: &str = "module_import";
const TS_CALL: &str = "call_expression";
const TS_MSG_EXPR: &str = "message_expression";

pub fn parse_objc_file(source: &str, file_path: &str) -> Result<ParseResult, String> {
    let lang: tree_sitter::Language = tree_sitter_objc::LANGUAGE.into();
    let mut parser = Parser::new();
    parser
        .set_language(&lang)
        .map_err(|e| format!("failed to set Objective-C language: {e}"))?;
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
    extract_top(&mut ctx, tree.root_node(), file_path);
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

fn find_name(source: &str, node: Node) -> String {
    // tree-sitter-objc uses ``name`` field or an inline ``identifier``.
    let n = node_field_text(source, node, "name");
    if !n.is_empty() {
        return n;
    }
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        let k = c.kind();
        if k == "identifier" || k == "class_name" || k == "type_identifier" {
            return node_text(source, c);
        }
    }
    String::new()
}

fn extract_top(ctx: &mut Ctx, parent: Node, scope: &str) {
    let mut cursor = parent.walk();
    for child in parent.children(&mut cursor) {
        match child.kind() {
            TS_CLASS_INTERFACE | TS_CLASS_IMPL => extract_class(ctx, child, scope, false),
            TS_CATEGORY_INTERFACE | TS_CATEGORY_IMPL => extract_class(ctx, child, scope, true),
            TS_PROTOCOL_DECL => extract_protocol(ctx, child, scope),
            TS_FUNCTION_DEF => extract_function(ctx, child, scope, None),
            TS_IMPORT => extract_import(ctx, child, scope),
            TS_MODULE_IMPORT => extract_module_import(ctx, child, scope),
            _ => {
                if child.named_child_count() > 0 {
                    extract_top(ctx, child, scope);
                }
            }
        }
    }
}

fn extract_class(ctx: &mut Ctx, node: Node, scope: &str, is_category: bool) {
    let name = find_name(ctx.source, node);
    if name.is_empty() {
        return;
    }
    let qn = qual(scope, &name);
    let mut props = Vec::new();
    if is_category {
        props.push(("is_category".to_string(), "true".to_string()));
    }
    ctx.nodes.push(ExtractedNode {
        label: LABEL_STRUCT.to_string(),
        name: name.clone(),
        qualified_name: qn.clone(),
        start_line: node.start_position().row as u64 + 1,
        end_line: node.end_position().row as u64 + 1,
        visibility: "public".to_string(),
        properties: props,
    });
    ctx.refs.push(ExtractedRef {
        kind: "Defines".to_string(),
        from_qualified_name: scope.to_string(),
        to_qualified_name: qn.clone(),
    });
    // Walk all children for method declarations / definitions.
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            TS_METHOD_DECL | TS_METHOD_DEF => {
                extract_method(ctx, child, &qn);
            }
            _ => {
                if child.named_child_count() > 0 {
                    // Dive into compound groupings (``class_body``, etc.).
                    let mut inner = child.walk();
                    for gc in child.children(&mut inner) {
                        if gc.kind() == TS_METHOD_DECL || gc.kind() == TS_METHOD_DEF {
                            extract_method(ctx, gc, &qn);
                        }
                    }
                }
            }
        }
    }
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
        visibility: "public".to_string(),
        properties: Vec::new(),
    });
    ctx.refs.push(ExtractedRef {
        kind: "Defines".to_string(),
        from_qualified_name: scope.to_string(),
        to_qualified_name: qn.clone(),
    });
}

fn method_selector(source: &str, node: Node) -> String {
    // Build the ObjC method selector from keyword_selector children.
    let mut parts: Vec<String> = Vec::new();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "unary_selector" | "selector" => {
                return node_text(source, child);
            }
            "keyword_declarator" => {
                let kw = node_field_text(source, child, "keyword");
                if !kw.is_empty() {
                    parts.push(format!("{kw}:"));
                }
            }
            _ => {}
        }
    }
    if parts.is_empty() {
        // Fallback: the method's ``selector`` field if present.
        let fs = node_field_text(source, node, "selector");
        if !fs.is_empty() {
            return fs;
        }
    }
    parts.join("")
}

fn extract_method(ctx: &mut Ctx, node: Node, scope: &str) {
    let sel = method_selector(ctx.source, node);
    if sel.is_empty() {
        return;
    }
    let seq = {
        ctx.next_seq += 1;
        ctx.next_seq
    };
    let qn = format!("{}::{}#{}", scope, sel, seq);
    ctx.nodes.push(ExtractedNode {
        label: LABEL_METHOD.to_string(),
        name: sel.clone(),
        qualified_name: qn.clone(),
        start_line: node.start_position().row as u64 + 1,
        end_line: node.end_position().row as u64 + 1,
        visibility: "public".to_string(),
        properties: vec![("receiver_type".to_string(), scope.to_string())],
    });
    ctx.refs.push(ExtractedRef {
        kind: "HasMethod".to_string(),
        from_qualified_name: scope.to_string(),
        to_qualified_name: qn.clone(),
    });
    if let Some(body) = node
        .child_by_field_name("body")
        .or_else(|| node_child_of_kind(node, "compound_statement"))
    {
        extract_calls(ctx, body, &qn);
    }
}

fn extract_function(ctx: &mut Ctx, node: Node, scope: &str, enclosing: Option<&str>) {
    let decl = node.child_by_field_name("declarator");
    let name = decl
        .map(|d| node_field_text(ctx.source, d, "declarator"))
        .unwrap_or_default();
    let name = if name.is_empty() {
        find_name(ctx.source, node)
    } else {
        name
    };
    if name.is_empty() {
        return;
    }
    let seq = {
        ctx.next_seq += 1;
        ctx.next_seq
    };
    let qn = format!("{}::{}#{}", scope, name, seq);
    let label = if enclosing.is_some() {
        LABEL_METHOD
    } else {
        LABEL_FUNCTION
    };
    ctx.nodes.push(ExtractedNode {
        label: label.to_string(),
        name: name.clone(),
        qualified_name: qn.clone(),
        start_line: node.start_position().row as u64 + 1,
        end_line: node.end_position().row as u64 + 1,
        visibility: "public".to_string(),
        properties: Vec::new(),
    });
    ctx.refs.push(ExtractedRef {
        kind: "Defines".to_string(),
        from_qualified_name: scope.to_string(),
        to_qualified_name: qn.clone(),
    });
    if let Some(body) = node
        .child_by_field_name("body")
        .or_else(|| node_child_of_kind(node, "compound_statement"))
    {
        extract_calls(ctx, body, &qn);
    }
}

fn node_child_of_kind<'a>(node: Node<'a>, kind: &str) -> Option<Node<'a>> {
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        if c.kind() == kind {
            return Some(c);
        }
    }
    None
}

fn extract_import(ctx: &mut Ctx, node: Node, scope: &str) {
    let text = node_text(ctx.source, node);
    let cleaned = text
        .trim()
        .trim_start_matches("#import")
        .trim_start_matches("#include")
        .trim()
        .trim_matches('<')
        .trim_matches('>')
        .trim_matches('"')
        .trim()
        .to_string();
    if cleaned.is_empty() {
        return;
    }
    let name = cleaned.rsplit('/').next().unwrap_or(&cleaned).to_string();
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

fn extract_module_import(ctx: &mut Ctx, node: Node, scope: &str) {
    let text = node_text(ctx.source, node);
    let cleaned = text
        .trim()
        .trim_start_matches("@import")
        .trim_end_matches(';')
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
        match n.kind() {
            TS_CALL => {
                let callee = node_field_text(ctx.source, n, "function");
                let callee = callee
                    .rsplit('.')
                    .next()
                    .unwrap_or("")
                    .trim_end_matches('(')
                    .to_string();
                emit_call(ctx, n, caller_qn, &callee);
            }
            TS_MSG_EXPR => {
                // ``[receiver selector:arg]`` — emit the selector as callee.
                let sel = node_field_text(ctx.source, n, "selector");
                let sel = if sel.is_empty() {
                    // Best-effort: first keyword from subtree. The children
                    // iterator borrows the cursor, so capture the match into
                    // a local before leaving the scope.
                    let mut inner = n.walk();
                    let mut picked: Option<Node> = None;
                    for c in n.children(&mut inner) {
                        if c.kind() == "keyword_selector" || c.kind() == "unary_selector" {
                            picked = Some(c);
                            break;
                        }
                    }
                    picked.map(|c| node_text(ctx.source, c)).unwrap_or_default()
                } else {
                    sel
                };
                if !sel.is_empty() {
                    emit_call(ctx, n, caller_qn, &sel);
                }
            }
            _ => {}
        }
        let mut cursor = n.walk();
        for c in n.children(&mut cursor) {
            stack.push(c);
        }
    }
}

fn emit_call(ctx: &mut Ctx, n: Node, caller_qn: &str, callee: &str) {
    if callee.is_empty()
        || !callee
            .chars()
            .next()
            .map_or(false, |c| c.is_alphabetic() || c == '_')
    {
        return;
    }
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
        name: callee.to_string(),
        qualified_name: site_qn,
        start_line: n.start_position().row as u64 + 1,
        end_line: n.end_position().row as u64 + 1,
        visibility: "public".to_string(),
        properties: vec![("callee_name".to_string(), callee.to_string())],
    });
    ctx.refs.push(ExtractedRef {
        kind: "Calls".to_string(),
        from_qualified_name: caller_qn.to_string(),
        to_qualified_name: callee.to_string(),
    });
}
