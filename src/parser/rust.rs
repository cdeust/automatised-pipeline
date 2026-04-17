// parser::rust — tree-sitter-based Rust source parser for code-intelligence graph.
//
// Parses a single `.rs` file and extracts typed symbols matching the
// graph_store schema. Zero dependency on graph_store or any storage layer.
//
// Grammar reference: https://github.com/tree-sitter/tree-sitter-rust

use tree_sitter::{Node, Parser};

use super::{
    node_field_text, node_text, qual, ExtractedNode, ExtractedRef, ParseResult,
    LABEL_CALL_SITE, LABEL_CONSTANT, LABEL_ENUM, LABEL_FIELD, LABEL_FUNCTION,
    LABEL_IMPORT, LABEL_METHOD, LABEL_MODULE, LABEL_STRUCT, LABEL_TRAIT,
    LABEL_TYPE_ALIAS, LABEL_VARIANT,
};

// ---------------------------------------------------------------------------
// Tree-sitter node type constants
// source: https://github.com/tree-sitter/tree-sitter-rust/blob/master/src/node-types.json
// ---------------------------------------------------------------------------

const TS_FUNCTION_ITEM: &str = "function_item";
const TS_FUNCTION_SIG: &str = "function_signature_item";
const TS_STRUCT_ITEM: &str = "struct_item";
const TS_ENUM_ITEM: &str = "enum_item";
const TS_ENUM_VARIANT: &str = "enum_variant";
const TS_TRAIT_ITEM: &str = "trait_item";
const TS_IMPL_ITEM: &str = "impl_item";
const TS_FIELD_DECL: &str = "field_declaration";
const TS_CONST_ITEM: &str = "const_item";
const TS_TYPE_ITEM: &str = "type_item";
const TS_USE_DECL: &str = "use_declaration";
const TS_MOD_ITEM: &str = "mod_item";
const TS_VIS_MOD: &str = "visibility_modifier";
const TS_FUNC_MODS: &str = "function_modifiers";
const TS_DECL_LIST: &str = "declaration_list";
const TS_FIELD_DECL_LIST: &str = "field_declaration_list";
const TS_ENUM_VARIANT_LIST: &str = "enum_variant_list";
const TS_USE_AS_CLAUSE: &str = "use_as_clause";
const TS_USE_WILDCARD: &str = "use_wildcard";
const TS_CALL_EXPR: &str = "call_expression";

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Parses a single `.rs` file and extracts typed symbols and relationships.
pub fn parse_rust_file(source: &str, file_path: &str) -> Result<ParseResult, String> {
    let lang: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
    let mut parser = Parser::new();
    parser
        .set_language(&lang)
        .map_err(|e| format!("failed to set language: {e}"))?;
    // source: H2 fix — cap tree-sitter work at 5 s per file. `parse` returns
    // None on timeout or if the parser is cancelled.
    parser.set_timeout_micros(super::PARSE_TIMEOUT_MICROS);
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| "parse_timeout_or_none: tree-sitter returned None \
                        (parse cancelled, timeout exceeded, or source rejected)".to_string())?;

    let mut ctx = ExtractCtx {
        source,
        file_path,
        nodes: Vec::new(),
        refs: Vec::new(),
    };
    extract_top_level(&mut ctx, tree.root_node(), file_path);
    Ok(ParseResult {
        nodes: ctx.nodes,
        refs: ctx.refs,
    })
}

// ---------------------------------------------------------------------------
// Extraction context
// ---------------------------------------------------------------------------

struct ExtractCtx<'a> {
    source: &'a str,
    file_path: &'a str,
    nodes: Vec<ExtractedNode>,
    refs: Vec<ExtractedRef>,
}

// ---------------------------------------------------------------------------
// Top-level extraction
// ---------------------------------------------------------------------------

fn extract_top_level(ctx: &mut ExtractCtx, parent: Node, scope: &str) {
    let mut cursor = parent.walk();
    for child in parent.children(&mut cursor) {
        match child.kind() {
            TS_FUNCTION_ITEM => extract_function(ctx, child, scope),
            TS_STRUCT_ITEM => extract_struct(ctx, child, scope),
            TS_ENUM_ITEM => extract_enum(ctx, child, scope),
            TS_TRAIT_ITEM => extract_trait(ctx, child, scope),
            TS_IMPL_ITEM => extract_impl(ctx, child),
            TS_CONST_ITEM => extract_const(ctx, child, scope),
            TS_TYPE_ITEM => extract_type_alias(ctx, child, scope),
            TS_USE_DECL => extract_use(ctx, child, scope),
            TS_MOD_ITEM => extract_mod(ctx, child, scope),
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Function extraction
// ---------------------------------------------------------------------------

fn extract_function(ctx: &mut ExtractCtx, node: Node, scope: &str) {
    let name = node_field_text(ctx.source, node, "name");
    if name.is_empty() {
        return;
    }
    let qn = qual(scope, &name);
    let vis = extract_visibility(ctx.source, node);
    let is_async = has_async_modifier(node);
    ctx.nodes.push(ExtractedNode {
        label: LABEL_FUNCTION.to_string(),
        name: name.clone(),
        qualified_name: qn.clone(),
        start_line: node.start_position().row as u64 + 1,
        end_line: node.end_position().row as u64 + 1,
        visibility: vis,
        properties: vec![("is_async".to_string(), is_async.to_string())],
    });
    ctx.refs.push(ExtractedRef {
        kind: "Defines".to_string(),
        from_qualified_name: scope.to_string(),
        to_qualified_name: qn.clone(),
    });
    if let Some(body) = node.child_by_field_name("body") {
        extract_call_sites(ctx, body, &qn);
    }
}

// ---------------------------------------------------------------------------
// Struct extraction
// ---------------------------------------------------------------------------

fn extract_struct(ctx: &mut ExtractCtx, node: Node, scope: &str) {
    let name = node_field_text(ctx.source, node, "name");
    if name.is_empty() {
        return;
    }
    let qn = qual(scope, &name);
    let vis = extract_visibility(ctx.source, node);
    ctx.nodes.push(ExtractedNode {
        label: LABEL_STRUCT.to_string(),
        name: name.clone(),
        qualified_name: qn.clone(),
        start_line: node.start_position().row as u64 + 1,
        end_line: node.end_position().row as u64 + 1,
        visibility: vis,
        properties: vec![],
    });
    ctx.refs.push(ExtractedRef {
        kind: "Defines".to_string(),
        from_qualified_name: scope.to_string(),
        to_qualified_name: qn.clone(),
    });
    extract_fields(ctx, node, &qn, TS_FIELD_DECL_LIST);
}

// ---------------------------------------------------------------------------
// Enum extraction
// ---------------------------------------------------------------------------

fn extract_enum(ctx: &mut ExtractCtx, node: Node, scope: &str) {
    let name = node_field_text(ctx.source, node, "name");
    if name.is_empty() {
        return;
    }
    let qn = qual(scope, &name);
    let vis = extract_visibility(ctx.source, node);
    ctx.nodes.push(ExtractedNode {
        label: LABEL_ENUM.to_string(),
        name: name.clone(),
        qualified_name: qn.clone(),
        start_line: node.start_position().row as u64 + 1,
        end_line: node.end_position().row as u64 + 1,
        visibility: vis,
        properties: vec![],
    });
    ctx.refs.push(ExtractedRef {
        kind: "Defines".to_string(),
        from_qualified_name: scope.to_string(),
        to_qualified_name: qn.clone(),
    });
    extract_variants(ctx, node, &qn);
}

fn extract_variants(ctx: &mut ExtractCtx, enum_node: Node, enum_qn: &str) {
    let body = match enum_node.child_by_field_name("body") {
        Some(b) if b.kind() == TS_ENUM_VARIANT_LIST => b,
        _ => return,
    };
    let mut cursor = body.walk();
    for child in body.children(&mut cursor) {
        if child.kind() != TS_ENUM_VARIANT {
            continue;
        }
        let name = node_field_text(ctx.source, child, "name");
        if name.is_empty() {
            continue;
        }
        let vqn = qual(enum_qn, &name);
        ctx.nodes.push(ExtractedNode {
            label: LABEL_VARIANT.to_string(),
            name,
            qualified_name: vqn.clone(),
            start_line: child.start_position().row as u64 + 1,
            end_line: child.end_position().row as u64 + 1,
            visibility: String::new(),
            properties: vec![],
        });
        ctx.refs.push(ExtractedRef {
            kind: "HasVariant".to_string(),
            from_qualified_name: enum_qn.to_string(),
            to_qualified_name: vqn,
        });
    }
}

// ---------------------------------------------------------------------------
// Trait extraction
// ---------------------------------------------------------------------------

fn extract_trait(ctx: &mut ExtractCtx, node: Node, scope: &str) {
    let name = node_field_text(ctx.source, node, "name");
    if name.is_empty() {
        return;
    }
    let qn = qual(scope, &name);
    let vis = extract_visibility(ctx.source, node);
    let supers = extract_supertraits(ctx.source, node);
    let mut props = vec![];
    if !supers.is_empty() {
        props.push(("supertraits".to_string(), supers.join(",")));
    }
    ctx.nodes.push(ExtractedNode {
        label: LABEL_TRAIT.to_string(),
        name: name.clone(),
        qualified_name: qn.clone(),
        start_line: node.start_position().row as u64 + 1,
        end_line: node.end_position().row as u64 + 1,
        visibility: vis,
        properties: props,
    });
    ctx.refs.push(ExtractedRef {
        kind: "Defines".to_string(),
        from_qualified_name: scope.to_string(),
        to_qualified_name: qn.clone(),
    });
    for sup in &supers {
        ctx.refs.push(ExtractedRef {
            kind: "Extends".to_string(),
            from_qualified_name: qn.clone(),
            to_qualified_name: sup.clone(),
        });
    }
    extract_trait_methods(ctx, node, &qn);
}

fn extract_trait_methods(ctx: &mut ExtractCtx, trait_node: Node, trait_qn: &str) {
    let body = match trait_node.child_by_field_name("body") {
        Some(b) if b.kind() == TS_DECL_LIST => b,
        _ => return,
    };
    let mut cursor = body.walk();
    for child in body.children(&mut cursor) {
        let is_sig = child.kind() == TS_FUNCTION_SIG;
        let is_fn = child.kind() == TS_FUNCTION_ITEM;
        if !is_sig && !is_fn {
            continue;
        }
        let name = node_field_text(ctx.source, child, "name");
        if name.is_empty() {
            continue;
        }
        let mqn = qual(trait_qn, &name);
        let vis = extract_visibility(ctx.source, child);
        let is_async = if is_fn { has_async_modifier(child) } else { false };
        ctx.nodes.push(ExtractedNode {
            label: LABEL_METHOD.to_string(),
            name: name.clone(),
            qualified_name: mqn.clone(),
            start_line: child.start_position().row as u64 + 1,
            end_line: child.end_position().row as u64 + 1,
            visibility: vis,
            properties: vec![
                ("is_async".to_string(), is_async.to_string()),
                ("receiver_type".to_string(), trait_qn.to_string()),
            ],
        });
        ctx.refs.push(ExtractedRef {
            kind: "HasMethod".to_string(),
            from_qualified_name: trait_qn.to_string(),
            to_qualified_name: mqn,
        });
    }
}

// ---------------------------------------------------------------------------
// Impl extraction
// ---------------------------------------------------------------------------

fn extract_impl(ctx: &mut ExtractCtx, node: Node) {
    let impl_type = node_field_text(ctx.source, node, "type");
    if impl_type.is_empty() {
        return;
    }
    let trait_name = node_field_text(ctx.source, node, "trait");
    let receiver_qn = qual(ctx.file_path, &impl_type);

    let body = match node.child_by_field_name("body") {
        Some(b) if b.kind() == TS_DECL_LIST => b,
        _ => return,
    };
    let mut cursor = body.walk();
    for child in body.children(&mut cursor) {
        if child.kind() != TS_FUNCTION_ITEM && child.kind() != TS_FUNCTION_SIG {
            continue;
        }
        extract_impl_method(ctx, child, &receiver_qn, &trait_name);
    }
}

fn extract_impl_method(
    ctx: &mut ExtractCtx,
    node: Node,
    receiver_qn: &str,
    trait_name: &str,
) {
    let name = node_field_text(ctx.source, node, "name");
    if name.is_empty() {
        return;
    }
    let mqn = qual(receiver_qn, &name);
    let vis = extract_visibility(ctx.source, node);
    let is_async = has_async_modifier(node);
    let mut props = vec![
        ("is_async".to_string(), is_async.to_string()),
        ("receiver_type".to_string(), receiver_qn.to_string()),
    ];
    if !trait_name.is_empty() {
        props.push(("trait_name".to_string(), trait_name.to_string()));
    }
    ctx.nodes.push(ExtractedNode {
        label: LABEL_METHOD.to_string(),
        name: name.clone(),
        qualified_name: mqn.clone(),
        start_line: node.start_position().row as u64 + 1,
        end_line: node.end_position().row as u64 + 1,
        visibility: vis,
        properties: props,
    });
    ctx.refs.push(ExtractedRef {
        kind: "HasMethod".to_string(),
        from_qualified_name: receiver_qn.to_string(),
        to_qualified_name: mqn.clone(),
    });
    if let Some(body) = node.child_by_field_name("body") {
        extract_call_sites(ctx, body, &mqn);
    }
}

// ---------------------------------------------------------------------------
// Field extraction
// ---------------------------------------------------------------------------

fn extract_fields(ctx: &mut ExtractCtx, parent: Node, parent_qn: &str, list_kind: &str) {
    let body = match parent.child_by_field_name("body") {
        Some(b) if b.kind() == list_kind => b,
        _ => return,
    };
    let mut cursor = body.walk();
    for child in body.children(&mut cursor) {
        if child.kind() != TS_FIELD_DECL {
            continue;
        }
        let name = node_field_text(ctx.source, child, "name");
        if name.is_empty() {
            continue;
        }
        let type_ann = node_field_text(ctx.source, child, "type");
        let vis = extract_visibility(ctx.source, child);
        let fqn = qual(parent_qn, &name);
        ctx.nodes.push(ExtractedNode {
            label: LABEL_FIELD.to_string(),
            name,
            qualified_name: fqn.clone(),
            start_line: child.start_position().row as u64 + 1,
            end_line: child.end_position().row as u64 + 1,
            visibility: vis,
            properties: vec![("type_annotation".to_string(), type_ann)],
        });
        ctx.refs.push(ExtractedRef {
            kind: "HasField".to_string(),
            from_qualified_name: parent_qn.to_string(),
            to_qualified_name: fqn,
        });
    }
}

// ---------------------------------------------------------------------------
// Const extraction
// ---------------------------------------------------------------------------

fn extract_const(ctx: &mut ExtractCtx, node: Node, scope: &str) {
    let name = node_field_text(ctx.source, node, "name");
    if name.is_empty() {
        return;
    }
    let qn = qual(scope, &name);
    let type_ann = node_field_text(ctx.source, node, "type");
    ctx.nodes.push(ExtractedNode {
        label: LABEL_CONSTANT.to_string(),
        name,
        qualified_name: qn.clone(),
        start_line: node.start_position().row as u64 + 1,
        end_line: node.end_position().row as u64 + 1,
        visibility: extract_visibility(ctx.source, node),
        properties: vec![("type_annotation".to_string(), type_ann)],
    });
    ctx.refs.push(ExtractedRef {
        kind: "Defines".to_string(),
        from_qualified_name: scope.to_string(),
        to_qualified_name: qn,
    });
}

// ---------------------------------------------------------------------------
// Type alias extraction
// ---------------------------------------------------------------------------

fn extract_type_alias(ctx: &mut ExtractCtx, node: Node, scope: &str) {
    let name = node_field_text(ctx.source, node, "name");
    if name.is_empty() {
        return;
    }
    let qn = qual(scope, &name);
    let target = node_field_text(ctx.source, node, "type");
    ctx.nodes.push(ExtractedNode {
        label: LABEL_TYPE_ALIAS.to_string(),
        name,
        qualified_name: qn.clone(),
        start_line: node.start_position().row as u64 + 1,
        end_line: node.end_position().row as u64 + 1,
        visibility: extract_visibility(ctx.source, node),
        properties: vec![("target_type".to_string(), target)],
    });
    ctx.refs.push(ExtractedRef {
        kind: "Defines".to_string(),
        from_qualified_name: scope.to_string(),
        to_qualified_name: qn,
    });
}

// ---------------------------------------------------------------------------
// Use declaration extraction
// ---------------------------------------------------------------------------

fn extract_use(ctx: &mut ExtractCtx, node: Node, scope: &str) {
    let arg = match node.child_by_field_name("argument") {
        Some(a) => a,
        None => return,
    };
    // source: tree-sitter-rust grammar — use_declaration::argument may be any
    // of { identifier, scoped_identifier, use_list, scoped_use_list,
    // use_as_clause, use_wildcard }. Brace lists expand into multiple atomic
    // Import nodes so downstream consumers can match individual leaves.
    let leaves = collect_use_leaves(ctx.source, arg, "");
    let start_line = node.start_position().row as u64 + 1;
    let end_line = node.end_position().row as u64 + 1;
    let visibility = extract_visibility(ctx.source, node);
    for (path, alias, is_glob) in leaves {
        emit_import(ctx, scope, path, alias, is_glob, start_line, end_line, &visibility);
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_import(
    ctx: &mut ExtractCtx,
    scope: &str,
    path: String,
    alias: String,
    is_glob: bool,
    start_line: u64,
    end_line: u64,
    visibility: &str,
) {
    if path.is_empty() {
        return;
    }
    let display_name = if !alias.is_empty() {
        alias.clone()
    } else if is_glob {
        format!("{path}::*")
    } else {
        path.clone()
    };
    let qn = qual(scope, &display_name);
    ctx.nodes.push(ExtractedNode {
        label: LABEL_IMPORT.to_string(),
        name: display_name,
        qualified_name: qn.clone(),
        start_line,
        end_line,
        visibility: visibility.to_string(),
        properties: vec![
            ("path".to_string(), path),
            ("alias".to_string(), alias),
            ("is_glob".to_string(), is_glob.to_string()),
        ],
    });
    ctx.refs.push(ExtractedRef {
        kind: "Defines".to_string(),
        from_qualified_name: scope.to_string(),
        to_qualified_name: qn,
    });
}

/// Walks a `use_declaration` argument subtree and returns one tuple per leaf
/// import in canonical `(path, alias, is_glob)` form. `prefix` is prepended
/// (with `::`) to each path string — this is what carries the scope across
/// nested brace lists like `use a::{b, c::{d, e}};`.
fn collect_use_leaves(source: &str, node: Node, prefix: &str) -> Vec<(String, String, bool)> {
    match node.kind() {
        "use_list" => walk_use_list_children(source, node, prefix),
        "scoped_use_list" => leaf_from_scoped_use_list(source, node, prefix),
        TS_USE_AS_CLAUSE => vec![leaf_from_use_as_clause(source, node, prefix)],
        TS_USE_WILDCARD => vec![leaf_from_use_wildcard(source, node, prefix)],
        _ => vec![leaf_from_identifier(source, node, prefix)],
    }
}

fn leaf_from_scoped_use_list(
    source: &str,
    node: Node,
    prefix: &str,
) -> Vec<(String, String, bool)> {
    let head = node
        .child_by_field_name("path")
        .map(|n| node_text(source, n))
        .unwrap_or_default();
    let new_prefix = join_use_path(prefix, &head);
    match node.child_by_field_name("list") {
        Some(list) => walk_use_list_children(source, list, &new_prefix),
        None => Vec::new(),
    }
}

fn leaf_from_use_as_clause(source: &str, node: Node, prefix: &str) -> (String, String, bool) {
    let path = node
        .child_by_field_name("path")
        .map(|n| node_text(source, n))
        .unwrap_or_default();
    let alias = node
        .child_by_field_name("alias")
        .map(|n| node_text(source, n))
        .unwrap_or_default();
    (join_use_path(prefix, &path), alias, false)
}

fn leaf_from_use_wildcard(source: &str, node: Node, prefix: &str) -> (String, String, bool) {
    // use_wildcard text is `<path>::*` (or just `*` when nested in
    // a brace list with an outer prefix). Strip the trailing `::*`
    // if present, otherwise treat the wildcard as attaching to the
    // current prefix verbatim.
    let text = node_text(source, node);
    let stripped = text.trim_end_matches("::*").trim_end_matches('*');
    let stripped = stripped.trim_end_matches("::");
    (join_use_path(prefix, stripped), String::new(), true)
}

fn leaf_from_identifier(source: &str, node: Node, prefix: &str) -> (String, String, bool) {
    // identifier, scoped_identifier, crate, self, super, etc.
    // Inside a brace list, `self` refers to the brace-list prefix itself
    // (`use std::io::{self, BufRead}` → import `std::io` and `std::io::BufRead`).
    let leaf = node_text(source, node);
    let path = if leaf == "self" && !prefix.is_empty() {
        prefix.to_string()
    } else {
        join_use_path(prefix, &leaf)
    };
    (path, String::new(), false)
}

fn walk_use_list_children(source: &str, list: Node, prefix: &str) -> Vec<(String, String, bool)> {
    let mut cursor = list.walk();
    let mut out: Vec<(String, String, bool)> = Vec::new();
    for child in list.children(&mut cursor) {
        // Skip punctuation — `{`, `,`, `}` — by filtering on named children.
        if !child.is_named() {
            continue;
        }
        out.extend(collect_use_leaves(source, child, prefix));
    }
    out
}

fn join_use_path(prefix: &str, tail: &str) -> String {
    if prefix.is_empty() {
        tail.to_string()
    } else if tail.is_empty() {
        prefix.to_string()
    } else {
        format!("{prefix}::{tail}")
    }
}

// ---------------------------------------------------------------------------
// Module extraction
// ---------------------------------------------------------------------------

fn extract_mod(ctx: &mut ExtractCtx, node: Node, scope: &str) {
    let name = node_field_text(ctx.source, node, "name");
    if name.is_empty() {
        return;
    }
    let qn = qual(scope, &name);
    ctx.nodes.push(ExtractedNode {
        label: LABEL_MODULE.to_string(),
        name: name.clone(),
        qualified_name: qn.clone(),
        start_line: node.start_position().row as u64 + 1,
        end_line: node.end_position().row as u64 + 1,
        visibility: extract_visibility(ctx.source, node),
        properties: vec![],
    });
    ctx.refs.push(ExtractedRef {
        kind: "Defines".to_string(),
        from_qualified_name: scope.to_string(),
        to_qualified_name: qn.clone(),
    });
    if let Some(body) = node.child_by_field_name("body") {
        if body.kind() == TS_DECL_LIST {
            extract_top_level(ctx, body, &qn);
        }
    }
}

// ---------------------------------------------------------------------------
// Call-site extraction
// ---------------------------------------------------------------------------

fn extract_call_sites(ctx: &mut ExtractCtx, body: Node, caller_qn: &str) {
    let mut stack = vec![body];
    while let Some(node) = stack.pop() {
        if node.kind() == TS_CALL_EXPR {
            extract_single_call_site(ctx, node, caller_qn);
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
}

fn extract_single_call_site(ctx: &mut ExtractCtx, node: Node, caller_qn: &str) {
    let func_node = match node.child_by_field_name("function") {
        Some(f) => f,
        None => return,
    };
    let callee = node_text(ctx.source, func_node);
    if callee.is_empty() || callee.contains('.') {
        return;
    }
    let line = node.start_position().row as u64 + 1;
    let col = node.start_position().column as u64;
    let cs_id = format!("{caller_qn}::call@{line}:{col}");
    ctx.nodes.push(ExtractedNode {
        label: LABEL_CALL_SITE.to_string(),
        name: callee.clone(),
        qualified_name: cs_id.clone(),
        start_line: line,
        end_line: line,
        visibility: String::new(),
        properties: vec![
            ("callee_name".to_string(), callee),
            ("caller_qn".to_string(), caller_qn.to_string()),
        ],
    });
    ctx.refs.push(ExtractedRef {
        kind: "Defines".to_string(),
        from_qualified_name: caller_qn.to_string(),
        to_qualified_name: cs_id,
    });
}

// ---------------------------------------------------------------------------
// Supertrait extraction
// ---------------------------------------------------------------------------

fn extract_supertraits(source: &str, trait_node: Node) -> Vec<String> {
    let mut supers = Vec::new();
    let bounds = match trait_node.child_by_field_name("bounds") {
        Some(b) => b,
        None => return supers,
    };
    let mut cursor = bounds.walk();
    for child in bounds.children(&mut cursor) {
        if child.kind() == "type_identifier" || child.kind() == "scoped_type_identifier" {
            let text = node_text(source, child);
            if !text.is_empty() {
                supers.push(text);
            }
        }
    }
    supers
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn extract_visibility(source: &str, node: Node) -> String {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == TS_VIS_MOD {
            return node_text(source, child);
        }
    }
    String::new()
}

fn has_async_modifier(node: Node) -> bool {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == TS_FUNC_MODS {
            let mut inner = child.walk();
            for gc in child.children(&mut inner) {
                if gc.kind() == "async" {
                    return true;
                }
            }
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_own_source() {
        let source = std::fs::read_to_string("src/main.rs")
            .expect("should be able to read src/main.rs");
        let result = parse_rust_file(&source, "src/main.rs")
            .expect("parse should succeed");

        let fn_names: Vec<&str> = result
            .nodes
            .iter()
            .filter(|n| n.label == "Function")
            .map(|n| n.name.as_str())
            .collect();

        assert!(fn_names.contains(&"main"), "should find main()");
        assert!(fn_names.contains(&"handle_tool_call"), "should find handle_tool_call()");
        assert!(fn_names.contains(&"write_message"), "should find write_message()");
        assert!(result.nodes.len() > 30, "main.rs has dozens of items, got {}", result.nodes.len());

        for node in &result.nodes {
            assert!(!node.name.is_empty(), "node with label {} has empty name", node.label);
        }
        for node in &result.nodes {
            if node.start_line > 0 {
                assert!(node.end_line >= node.start_line);
            }
        }
        assert!(!result.refs.is_empty(), "should have extracted some relationships");
    }

    #[test]
    fn test_all_construct_types() {
        let src = r#"
pub async fn top_fn() {}
pub struct MyStruct { pub x: i32, y: String }
pub enum MyEnum { A, B }
pub trait MyTrait { fn method(&self); }
impl MyStruct { pub fn new() -> Self { todo!() } }
impl MyTrait for MyStruct { fn method(&self) {} }
const MAX: usize = 42;
type Alias = Vec<String>;
use std::collections::HashMap;
mod inner;
"#;
        let result = parse_rust_file(src, "test.rs").expect("parse");
        let labels: Vec<&str> = result.nodes.iter().map(|n| n.label.as_str()).collect();

        assert!(labels.contains(&"Function"), "missing Function");
        assert!(labels.contains(&"Struct"), "missing Struct");
        assert!(labels.contains(&"Enum"), "missing Enum");
        assert!(labels.contains(&"Variant"), "missing Variant");
        assert!(labels.contains(&"Trait"), "missing Trait");
        assert!(labels.contains(&"Method"), "missing Method");
        assert!(labels.contains(&"Field"), "missing Field");
        assert!(labels.contains(&"Constant"), "missing Constant");
        assert!(labels.contains(&"TypeAlias"), "missing TypeAlias");
        assert!(labels.contains(&"Import"), "missing Import");
        assert!(labels.contains(&"Module"), "missing Module");

        let top_fn = result.nodes.iter().find(|n| n.name == "top_fn").unwrap();
        let is_async_prop = top_fn.properties.iter().find(|(k, _)| k == "is_async").unwrap();
        assert_eq!(is_async_prop.1, "true");

        let x_field = result.nodes.iter().find(|n| n.name == "x").unwrap();
        let type_ann = x_field.properties.iter().find(|(k, _)| k == "type_annotation").unwrap();
        assert_eq!(type_ann.1, "i32");

        assert!(result.refs.iter().any(|r| r.kind == "HasVariant" && r.from_qualified_name.contains("MyEnum")));
        assert!(result.refs.iter().any(|r| r.kind == "HasField" && r.from_qualified_name.contains("MyStruct")));
        assert!(result.refs.iter().any(|r| r.kind == "HasMethod"));
    }

    #[test]
    fn test_visibility_extraction() {
        let src = r#"
pub fn public_fn() {}
pub(crate) fn crate_fn() {}
pub(super) fn super_fn() {}
fn private_fn() {}
"#;
        let result = parse_rust_file(src, "test.rs").expect("parse");
        let find = |name: &str| -> String {
            result.nodes.iter().find(|n| n.name == name)
                .map(|n| n.visibility.clone()).unwrap_or_default()
        };
        assert_eq!(find("public_fn"), "pub");
        assert_eq!(find("crate_fn"), "pub(crate)");
        assert_eq!(find("super_fn"), "pub(super)");
        assert_eq!(find("private_fn"), "");
    }

    #[test]
    fn test_parse_multi_brace_use() {
        // Multi-brace `use` lists must expand into one Import per leaf so
        // that q9 (imports in file) and q14 (unresolved externals) can match
        // individual symbols — not the raw brace substring.
        let src = r#"
use std::collections::{HashMap, HashSet};
use serde::{Deserialize, Serialize as Ser};
use std::io::{self, BufRead};
use a::b::*;
use a::{b, c::{d, e}};
"#;
        let result = parse_rust_file(src, "test.rs").expect("parse");
        let import_names: Vec<&str> = result
            .nodes
            .iter()
            .filter(|n| n.label == "Import")
            .map(|n| n.name.as_str())
            .collect();

        // HashMap / HashSet
        assert!(
            import_names.contains(&"std::collections::HashMap"),
            "missing std::collections::HashMap in {import_names:?}"
        );
        assert!(
            import_names.contains(&"std::collections::HashSet"),
            "missing std::collections::HashSet in {import_names:?}"
        );
        // alias: Serialize as Ser → display_name becomes the alias
        assert!(
            import_names.contains(&"Ser"),
            "aliased import should use alias as display name, got {import_names:?}"
        );
        // Deserialize is not aliased
        assert!(
            import_names.contains(&"serde::Deserialize"),
            "missing serde::Deserialize in {import_names:?}"
        );
        // `self` in brace list resolves to the prefix itself
        assert!(
            import_names.contains(&"std::io"),
            "missing std::io (from use std::io::{{self, ..}}) in {import_names:?}"
        );
        assert!(
            import_names.contains(&"std::io::BufRead"),
            "missing std::io::BufRead in {import_names:?}"
        );
        // nested brace list
        assert!(
            import_names.contains(&"a::b"),
            "missing a::b in {import_names:?}"
        );
        assert!(
            import_names.contains(&"a::c::d"),
            "missing a::c::d in {import_names:?}"
        );
        assert!(
            import_names.contains(&"a::c::e"),
            "missing a::c::e in {import_names:?}"
        );
        // Glob: display name ends in ::*
        assert!(
            import_names.contains(&"a::b::*"),
            "missing glob a::b::* in {import_names:?}"
        );
        // Regression: no entry should still contain a raw brace.
        for n in &import_names {
            assert!(
                !n.contains('{') && !n.contains('}'),
                "raw brace leaked into Import name: {n}"
            );
        }
    }

    #[test]
    fn test_impl_trait_property() {
        let src = r#"
trait MyTrait { fn do_it(&self); }
struct S;
impl MyTrait for S { fn do_it(&self) {} }
"#;
        let result = parse_rust_file(src, "test.rs").expect("parse");
        let method = result.nodes.iter()
            .find(|n| n.label == "Method" && n.name == "do_it"
                && n.properties.iter().any(|(k, v)| k == "trait_name" && v == "MyTrait"))
            .expect("should find impl method with trait_name property");
        assert!(method.qualified_name.contains("S"));
    }
}
