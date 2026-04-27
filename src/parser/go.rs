// parser::go — tree-sitter-based Go source parser for code-intelligence graph.
//
// Handles ``.go``. Extracts package, import, function, method (with receiver),
// struct, interface, type alias, const, var, and calls.
//
// Grammar reference: https://github.com/tree-sitter/tree-sitter-go

use tree_sitter::{Node, Parser};

use super::{
    node_field_text, node_text, qual, ExtractedNode, ExtractedRef, ParseResult, LABEL_CALL_SITE,
    LABEL_CONSTANT, LABEL_FUNCTION, LABEL_IMPORT, LABEL_METHOD, LABEL_STRUCT, LABEL_TRAIT,
    LABEL_TYPE_ALIAS,
};

const TS_PACKAGE_CLAUSE: &str = "package_clause";
const TS_IMPORT_DECL: &str = "import_declaration";
const TS_TYPE_DECL: &str = "type_declaration";
const TS_FUNCTION_DECL: &str = "function_declaration";
const TS_METHOD_DECL: &str = "method_declaration";
const TS_CONST_DECL: &str = "const_declaration";
const TS_VAR_DECL: &str = "var_declaration";
const TS_CALL_EXPR: &str = "call_expression";

pub fn parse_go_file(source: &str, file_path: &str) -> Result<ParseResult, String> {
    let lang: tree_sitter::Language = tree_sitter_go::LANGUAGE.into();
    let mut parser = Parser::new();
    parser
        .set_language(&lang)
        .map_err(|e| format!("failed to set Go language: {e}"))?;
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

fn go_visibility(name: &str) -> String {
    // Exported iff first letter is uppercase; idiomatic Go convention.
    match name.chars().next() {
        Some(c) if c.is_uppercase() => "public".to_string(),
        _ => "package".to_string(),
    }
}

fn extract_top(ctx: &mut Ctx, parent: Node, scope: &str) {
    let mut cursor = parent.walk();
    for child in parent.children(&mut cursor) {
        match child.kind() {
            TS_PACKAGE_CLAUSE => {}
            TS_IMPORT_DECL => extract_imports(ctx, child, scope),
            TS_TYPE_DECL => extract_types(ctx, child, scope),
            TS_FUNCTION_DECL => extract_function(ctx, child, scope),
            TS_METHOD_DECL => extract_method(ctx, child, scope),
            TS_CONST_DECL | TS_VAR_DECL => extract_value_decl(ctx, child, scope),
            _ => {}
        }
    }
}

fn extract_imports(ctx: &mut Ctx, node: Node, scope: &str) {
    // Two shapes: single ``import "x"`` or ``import ( "a"; "b" )``.
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if n.kind() == "import_spec" {
            let path = node_field_text(ctx.source, n, "path");
            let cleaned = path.trim_matches('"').to_string();
            if cleaned.is_empty() {
                continue;
            }
            let name = cleaned.rsplit('/').next().unwrap_or(&cleaned).to_string();
            let qn = qual(scope, &format!("import:{cleaned}"));
            ctx.nodes.push(ExtractedNode {
                label: LABEL_IMPORT.to_string(),
                name,
                qualified_name: qn,
                start_line: n.start_position().row as u64 + 1,
                end_line: n.end_position().row as u64 + 1,
                visibility: "public".to_string(),
                properties: vec![("target".to_string(), cleaned.clone())],
            });
            ctx.refs.push(ExtractedRef {
                kind: "Imports".to_string(),
                from_qualified_name: scope.to_string(),
                to_qualified_name: cleaned,
            });
        }
        let mut cursor = n.walk();
        for c in n.children(&mut cursor) {
            stack.push(c);
        }
    }
}

fn extract_types(ctx: &mut Ctx, node: Node, scope: &str) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "type_spec" => extract_type_spec(ctx, child, scope),
            "type_alias" => extract_type_spec(ctx, child, scope),
            _ => {}
        }
    }
}

fn extract_type_spec(ctx: &mut Ctx, node: Node, scope: &str) {
    let name = node_field_text(ctx.source, node, "name");
    if name.is_empty() {
        return;
    }
    let qn = qual(scope, &name);
    // Classify by looking at the ``type`` field — struct/interface/other.
    let mut label = LABEL_TYPE_ALIAS;
    if let Some(ty) = node.child_by_field_name("type") {
        match ty.kind() {
            "struct_type" => label = LABEL_STRUCT,
            "interface_type" => label = LABEL_TRAIT,
            _ => {}
        }
    }
    ctx.nodes.push(ExtractedNode {
        label: label.to_string(),
        name: name.clone(),
        qualified_name: qn.clone(),
        start_line: node.start_position().row as u64 + 1,
        end_line: node.end_position().row as u64 + 1,
        visibility: go_visibility(&name),
        properties: Vec::new(),
    });
    ctx.refs.push(ExtractedRef {
        kind: "Defines".to_string(),
        from_qualified_name: scope.to_string(),
        to_qualified_name: qn,
    });
}

fn extract_function(ctx: &mut Ctx, node: Node, scope: &str) {
    let name = node_field_text(ctx.source, node, "name");
    if name.is_empty() {
        return;
    }
    let seq = {
        ctx.next_seq += 1;
        ctx.next_seq
    };
    let qn = format!("{}::{}#{}", scope, name, seq);
    ctx.nodes.push(ExtractedNode {
        label: LABEL_FUNCTION.to_string(),
        name: name.clone(),
        qualified_name: qn.clone(),
        start_line: node.start_position().row as u64 + 1,
        end_line: node.end_position().row as u64 + 1,
        visibility: go_visibility(&name),
        properties: Vec::new(),
    });
    ctx.refs.push(ExtractedRef {
        kind: "Defines".to_string(),
        from_qualified_name: scope.to_string(),
        to_qualified_name: qn.clone(),
    });
    if let Some(body) = node.child_by_field_name("body") {
        extract_calls(ctx, body, &qn);
    }
}

fn extract_method(ctx: &mut Ctx, node: Node, scope: &str) {
    let name = node_field_text(ctx.source, node, "name");
    if name.is_empty() {
        return;
    }
    // Receiver type — strip ``(*T)`` or ``(T)`` to ``T``.
    let recv = node_field_text(ctx.source, node, "receiver");
    let recv_type = recv
        .trim()
        .trim_start_matches('(')
        .trim_end_matches(')')
        .split_whitespace()
        .last()
        .unwrap_or("")
        .trim_start_matches('*')
        .to_string();
    let scope_qn = if recv_type.is_empty() {
        scope.to_string()
    } else {
        qual(scope, &recv_type)
    };
    let seq = {
        ctx.next_seq += 1;
        ctx.next_seq
    };
    let qn = format!("{}::{}#{}", scope_qn, name, seq);
    ctx.nodes.push(ExtractedNode {
        label: LABEL_METHOD.to_string(),
        name: name.clone(),
        qualified_name: qn.clone(),
        start_line: node.start_position().row as u64 + 1,
        end_line: node.end_position().row as u64 + 1,
        visibility: go_visibility(&name),
        properties: vec![("receiver_type".to_string(), recv_type.clone())],
    });
    ctx.refs.push(ExtractedRef {
        kind: "HasMethod".to_string(),
        from_qualified_name: scope_qn.clone(),
        to_qualified_name: qn.clone(),
    });
    if let Some(body) = node.child_by_field_name("body") {
        extract_calls(ctx, body, &qn);
    }
}

fn extract_value_decl(ctx: &mut Ctx, node: Node, scope: &str) {
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if n.kind() == "const_spec" || n.kind() == "var_spec" {
            let mut cursor = n.walk();
            for child in n.children(&mut cursor) {
                if child.kind() == "identifier" {
                    let name = node_text(ctx.source, child);
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
                        visibility: go_visibility(&name),
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
        let mut cursor = n.walk();
        for c in n.children(&mut cursor) {
            stack.push(c);
        }
    }
}

fn extract_calls(ctx: &mut Ctx, root: Node, caller_qn: &str) {
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if n.kind() == TS_CALL_EXPR {
            let callee = node_field_text(ctx.source, n, "function");
            let tail = callee
                .rsplit('.')
                .next()
                .unwrap_or("")
                .trim_end_matches('(')
                .trim()
                .to_string();
            if !tail.is_empty()
                && tail
                    .chars()
                    .next()
                    .map_or(false, |c| c.is_alphabetic() || c == '_')
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
                    visibility: "public".to_string(),
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
