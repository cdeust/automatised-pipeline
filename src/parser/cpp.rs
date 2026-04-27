// parser::cpp — tree-sitter-based C++ source parser.
//
// Builds on the C grammar's node shapes but adds class, namespace,
// template declarations, and member functions. Uses the tree-sitter-cpp
// grammar directly (not the C grammar).
//
// Grammar reference: https://github.com/tree-sitter/tree-sitter-cpp

use tree_sitter::{Node, Parser};

use super::{
    node_field_text, node_text, qual, ExtractedNode, ExtractedRef, ParseResult, LABEL_CALL_SITE,
    LABEL_CONSTANT, LABEL_ENUM, LABEL_FUNCTION, LABEL_IMPORT, LABEL_METHOD, LABEL_STRUCT,
    LABEL_TRAIT,
};

const TS_NAMESPACE: &str = "namespace_definition";
const TS_CLASS: &str = "class_specifier";
const TS_STRUCT: &str = "struct_specifier";
const TS_UNION: &str = "union_specifier";
const TS_ENUM: &str = "enum_specifier";
const TS_TEMPLATE: &str = "template_declaration";
const TS_FUNCTION_DEF: &str = "function_definition";
const TS_FIELD_DECL: &str = "field_declaration";
const TS_TYPEDEF: &str = "type_definition";
const TS_INCLUDE: &str = "preproc_include";
const TS_USING: &str = "using_declaration";
const TS_CALL: &str = "call_expression";

pub fn parse_cpp_file(source: &str, file_path: &str) -> Result<ParseResult, String> {
    let lang: tree_sitter::Language = tree_sitter_cpp::LANGUAGE.into();
    let mut parser = Parser::new();
    parser
        .set_language(&lang)
        .map_err(|e| format!("failed to set C++ language: {e}"))?;
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

fn find_identifier(source: &str, node: Node) -> String {
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        let k = n.kind();
        if k == "identifier" || k == "type_identifier" || k == "field_identifier" {
            return node_text(source, n);
        }
        let mut cursor = n.walk();
        for c in n.children(&mut cursor) {
            stack.push(c);
        }
    }
    String::new()
}

fn extract_children(ctx: &mut Ctx, parent: Node, scope: &str, enclosing_type: Option<&str>) {
    let mut cursor = parent.walk();
    for child in parent.children(&mut cursor) {
        match child.kind() {
            TS_NAMESPACE => extract_namespace(ctx, child, scope),
            TS_CLASS => extract_class_like(ctx, child, scope, LABEL_STRUCT, /*is_class=*/ true),
            TS_STRUCT | TS_UNION => {
                extract_class_like(ctx, child, scope, LABEL_STRUCT, false)
            }
            TS_ENUM => extract_enum(ctx, child, scope),
            TS_TEMPLATE => {
                // Templates wrap a class/function; descend.
                extract_children(ctx, child, scope, enclosing_type);
            }
            TS_FUNCTION_DEF => extract_function(ctx, child, scope, enclosing_type),
            TS_FIELD_DECL => {
                if enclosing_type.is_some() {
                    // Inside a class body this may be a member function
                    // declaration OR a field; check for a function_declarator.
                    if has_function_declarator(child) {
                        extract_member_proto(ctx, child, scope);
                    } else {
                        extract_member_field(ctx, child, scope);
                    }
                }
            }
            TS_TYPEDEF => extract_typedef(ctx, child, scope),
            TS_INCLUDE => extract_include(ctx, child, scope),
            TS_USING => extract_using(ctx, child, scope),
            _ => {
                if child.named_child_count() > 0 {
                    extract_children(ctx, child, scope, enclosing_type);
                }
            }
        }
    }
}

fn has_function_declarator(node: Node) -> bool {
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if n.kind() == "function_declarator" {
            return true;
        }
        let mut cursor = n.walk();
        for c in n.children(&mut cursor) {
            stack.push(c);
        }
    }
    false
}

fn extract_namespace(ctx: &mut Ctx, node: Node, scope: &str) {
    let name = node_field_text(ctx.source, node, "name");
    let name = if name.is_empty() {
        find_identifier(ctx.source, node)
    } else {
        name
    };
    if name.is_empty() {
        // Anonymous namespace — keep scope unchanged but still recurse.
        if let Some(body) = node.child_by_field_name("body") {
            extract_children(ctx, body, scope, None);
        }
        return;
    }
    let qn = qual(scope, &name);
    ctx.nodes.push(ExtractedNode {
        label: LABEL_STRUCT.to_string(),
        name: name.clone(),
        qualified_name: qn.clone(),
        start_line: node.start_position().row as u64 + 1,
        end_line: node.end_position().row as u64 + 1,
        visibility: "public".to_string(),
        properties: vec![("is_namespace".to_string(), "true".to_string())],
    });
    ctx.refs.push(ExtractedRef {
        kind: "Defines".to_string(),
        from_qualified_name: scope.to_string(),
        to_qualified_name: qn.clone(),
    });
    if let Some(body) = node.child_by_field_name("body") {
        extract_children(ctx, body, &qn, None);
    }
}

fn extract_class_like(ctx: &mut Ctx, node: Node, scope: &str, label: &str, is_class: bool) {
    let name = node_field_text(ctx.source, node, "name");
    let name = if name.is_empty() {
        find_identifier(ctx.source, node)
    } else {
        name
    };
    if name.is_empty() {
        return;
    }
    let qn = qual(scope, &name);
    let mut props = Vec::new();
    if is_class {
        props.push(("is_class".to_string(), "true".to_string()));
    }
    ctx.nodes.push(ExtractedNode {
        label: label.to_string(),
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
    // Base-class list.
    if let Some(bases) = node.child_by_field_name("bases") {
        let mut cursor = bases.walk();
        for child in bases.children(&mut cursor) {
            if child.kind() == "base_class_clause" {
                let t = node_text(ctx.source, child).trim().to_string();
                if !t.is_empty() {
                    ctx.refs.push(ExtractedRef {
                        kind: "Extends".to_string(),
                        from_qualified_name: qn.clone(),
                        to_qualified_name: t,
                    });
                }
            }
        }
    }
    if let Some(body) = node.child_by_field_name("body") {
        extract_children(ctx, body, &qn, Some(&qn));
    }
}

fn extract_enum(ctx: &mut Ctx, node: Node, scope: &str) {
    let name = node_field_text(ctx.source, node, "name");
    let name = if name.is_empty() {
        find_identifier(ctx.source, node)
    } else {
        name
    };
    if name.is_empty() {
        return;
    }
    let qn = qual(scope, &name);
    ctx.nodes.push(ExtractedNode {
        label: LABEL_ENUM.to_string(),
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
        to_qualified_name: qn,
    });
}

fn extract_function(ctx: &mut Ctx, node: Node, scope: &str, enclosing_type: Option<&str>) {
    let declarator = node.child_by_field_name("declarator");
    let name = declarator
        .map(|d| find_identifier(ctx.source, d))
        .unwrap_or_default();
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
        visibility: "public".to_string(),
        properties: props,
    });
    let edge = if enclosing_type.is_some() {
        "HasMethod"
    } else {
        "Defines"
    };
    ctx.refs.push(ExtractedRef {
        kind: edge.to_string(),
        from_qualified_name: scope.to_string(),
        to_qualified_name: qn.clone(),
    });
    if let Some(body) = node.child_by_field_name("body") {
        extract_calls(ctx, body, &qn);
    }
}

fn extract_member_proto(ctx: &mut Ctx, node: Node, scope: &str) {
    let name = find_identifier(ctx.source, node);
    if name.is_empty() {
        return;
    }
    let seq = {
        ctx.next_seq += 1;
        ctx.next_seq
    };
    let qn = format!("{}::{}#{}", scope, name, seq);
    ctx.nodes.push(ExtractedNode {
        label: LABEL_METHOD.to_string(),
        name: name.clone(),
        qualified_name: qn.clone(),
        start_line: node.start_position().row as u64 + 1,
        end_line: node.end_position().row as u64 + 1,
        visibility: "public".to_string(),
        properties: vec![
            ("is_prototype".to_string(), "true".to_string()),
            ("receiver_type".to_string(), scope.to_string()),
        ],
    });
    ctx.refs.push(ExtractedRef {
        kind: "HasMethod".to_string(),
        from_qualified_name: scope.to_string(),
        to_qualified_name: qn,
    });
}

fn extract_member_field(ctx: &mut Ctx, node: Node, scope: &str) {
    let name = find_identifier(ctx.source, node);
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
        visibility: "public".to_string(),
        properties: Vec::new(),
    });
    ctx.refs.push(ExtractedRef {
        kind: "Defines".to_string(),
        from_qualified_name: scope.to_string(),
        to_qualified_name: qn,
    });
}

fn extract_typedef(ctx: &mut Ctx, node: Node, scope: &str) {
    let name = find_identifier(ctx.source, node);
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
        visibility: "public".to_string(),
        properties: vec![("typedef".to_string(), "true".to_string())],
    });
    ctx.refs.push(ExtractedRef {
        kind: "Defines".to_string(),
        from_qualified_name: scope.to_string(),
        to_qualified_name: qn,
    });
}

fn extract_include(ctx: &mut Ctx, node: Node, scope: &str) {
    let text = node_text(ctx.source, node);
    let cleaned = text
        .trim()
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
    let qn = qual(scope, &format!("include:{cleaned}"));
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
    // Cross-reference in the graph palette: C++ headers show up as
    // "includes" in the UI, but we map to the same LABEL_TRAIT when a
    // module is semantically a concept — keep simple for now.
    let _ = LABEL_TRAIT;
}

fn extract_using(ctx: &mut Ctx, node: Node, scope: &str) {
    let text = node_text(ctx.source, node);
    let cleaned = text
        .trim()
        .trim_start_matches("using")
        .trim()
        .trim_end_matches(';')
        .trim()
        .to_string();
    if cleaned.is_empty() {
        return;
    }
    let qn = qual(scope, &format!("using:{cleaned}"));
    ctx.nodes.push(ExtractedNode {
        label: LABEL_IMPORT.to_string(),
        name: cleaned.clone(),
        qualified_name: qn,
        start_line: node.start_position().row as u64 + 1,
        end_line: node.end_position().row as u64 + 1,
        visibility: "public".to_string(),
        properties: vec![("using".to_string(), "true".to_string())],
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
        if n.kind() == TS_CALL {
            let callee = node_field_text(ctx.source, n, "function");
            let tail = callee
                .rsplit(|c: char| c == '.' || c == '>' || c == ':')
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
