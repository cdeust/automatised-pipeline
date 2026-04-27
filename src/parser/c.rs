// parser::c — tree-sitter-based C source parser for code-intelligence graph.
//
// Handles ``.c`` and ``.h``. C++ uses a separate parser (``parser::cpp``).
// Focuses on the constructs the graph_store schema can represent:
// struct, enum, typedef, function, #include, call.
//
// Grammar reference: https://github.com/tree-sitter/tree-sitter-c

use tree_sitter::{Node, Parser};

use super::{
    node_field_text, node_text, qual, ExtractedNode, ExtractedRef, ParseResult, LABEL_CALL_SITE,
    LABEL_CONSTANT, LABEL_ENUM, LABEL_FUNCTION, LABEL_IMPORT, LABEL_STRUCT,
};

const TS_STRUCT: &str = "struct_specifier";
const TS_UNION: &str = "union_specifier";
const TS_ENUM: &str = "enum_specifier";
const TS_TYPEDEF: &str = "type_definition";
const TS_FUNCTION_DEF: &str = "function_definition";
const TS_FUNCTION_DECL: &str = "declaration";
const TS_INCLUDE: &str = "preproc_include";
const TS_CALL: &str = "call_expression";

pub fn parse_c_file(source: &str, file_path: &str) -> Result<ParseResult, String> {
    let lang: tree_sitter::Language = tree_sitter_c::LANGUAGE.into();
    let mut parser = Parser::new();
    parser
        .set_language(&lang)
        .map_err(|e| format!("failed to set C language: {e}"))?;
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

pub(crate) struct Ctx<'a> {
    pub source: &'a str,
    #[allow(dead_code)]
    pub file_path: &'a str,
    pub nodes: Vec<ExtractedNode>,
    pub refs: Vec<ExtractedRef>,
    pub next_seq: u64,
}

pub(crate) fn extract_top(ctx: &mut Ctx, parent: Node, scope: &str) {
    let mut cursor = parent.walk();
    for child in parent.children(&mut cursor) {
        match child.kind() {
            TS_STRUCT | TS_UNION => extract_struct(ctx, child, scope, LABEL_STRUCT),
            TS_ENUM => extract_enum(ctx, child, scope),
            TS_TYPEDEF => extract_typedef(ctx, child, scope),
            TS_FUNCTION_DEF => extract_function(ctx, child, scope),
            TS_INCLUDE => extract_include(ctx, child, scope),
            TS_FUNCTION_DECL => {
                // Function prototypes / forward declarations. The grammar
                // also uses ``declaration`` for globals — we only emit
                // function declarations.
                if is_function_prototype(child) {
                    extract_function_prototype(ctx, child, scope);
                }
            }
            _ => {
                if child.named_child_count() > 0 {
                    extract_top(ctx, child, scope);
                }
            }
        }
    }
}

fn is_function_prototype(node: Node) -> bool {
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        if c.kind() == "function_declarator" {
            return true;
        }
        if c.kind() == "init_declarator" {
            // ``int f(void) = ...;`` rare, but still has a function_declarator inside.
            let mut ic = c.walk();
            for gc in c.children(&mut ic) {
                if gc.kind() == "function_declarator" {
                    return true;
                }
            }
        }
    }
    false
}

fn find_identifier(source: &str, node: Node) -> String {
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if n.kind() == "identifier" || n.kind() == "type_identifier" {
            return node_text(source, n);
        }
        let mut cursor = n.walk();
        for c in n.children(&mut cursor) {
            stack.push(c);
        }
    }
    String::new()
}

fn extract_struct(ctx: &mut Ctx, node: Node, scope: &str, label: &str) {
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
        to_qualified_name: qn,
    });
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
        to_qualified_name: qn.clone(),
    });
    // Enum entries as constants.
    if let Some(body) = node.child_by_field_name("body") {
        let mut cursor = body.walk();
        for child in body.children(&mut cursor) {
            if child.kind() == "enumerator" {
                let en = find_identifier(ctx.source, child);
                if en.is_empty() {
                    continue;
                }
                let eqn = qual(&qn, &en);
                ctx.nodes.push(ExtractedNode {
                    label: LABEL_CONSTANT.to_string(),
                    name: en.clone(),
                    qualified_name: eqn.clone(),
                    start_line: child.start_position().row as u64 + 1,
                    end_line: child.end_position().row as u64 + 1,
                    visibility: "public".to_string(),
                    properties: vec![("enum_entry".to_string(), "true".to_string())],
                });
                ctx.refs.push(ExtractedRef {
                    kind: "Defines".to_string(),
                    from_qualified_name: qn.clone(),
                    to_qualified_name: eqn,
                });
            }
        }
    }
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

fn extract_function(ctx: &mut Ctx, node: Node, scope: &str) {
    // ``declarator`` is a ``function_declarator``; its own ``declarator``
    // field carries the name (plus any pointer/reference qualifiers).
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
    ctx.nodes.push(ExtractedNode {
        label: LABEL_FUNCTION.to_string(),
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
    if let Some(body) = node.child_by_field_name("body") {
        extract_calls(ctx, body, &qn);
    }
}

fn extract_function_prototype(ctx: &mut Ctx, node: Node, scope: &str) {
    // Only emit a Function node for pure prototypes — skip forward
    // declarations of variables (``int x;``) which also land in ``declaration``.
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
        label: LABEL_FUNCTION.to_string(),
        name: name.clone(),
        qualified_name: qn.clone(),
        start_line: node.start_position().row as u64 + 1,
        end_line: node.end_position().row as u64 + 1,
        visibility: "public".to_string(),
        properties: vec![("is_prototype".to_string(), "true".to_string())],
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
}

pub(crate) fn extract_calls(ctx: &mut Ctx, root: Node, caller_qn: &str) {
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if n.kind() == TS_CALL {
            let callee = node_field_text(ctx.source, n, "function");
            // For ``a.b(c)`` we keep the tail; for ``foo(c)`` we keep foo.
            let tail = callee
                .rsplit(|c: char| c == '.' || c == '>' || c == ':')
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
