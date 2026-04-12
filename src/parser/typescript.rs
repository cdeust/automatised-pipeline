// parser::typescript — tree-sitter-based TypeScript source parser for code-intelligence graph.
//
// Parses a single `.ts`/`.tsx` file and extracts typed symbols matching the
// graph_store schema. Produces the same ParseResult types as the Rust parser.
//
// Grammar reference: https://github.com/tree-sitter/tree-sitter-typescript

use tree_sitter::{Node, Parser};

use super::{
    node_field_text, node_text, qual, ExtractedNode, ExtractedRef, ParseResult,
    LABEL_CALL_SITE, LABEL_CONSTANT, LABEL_ENUM, LABEL_FIELD, LABEL_FUNCTION,
    LABEL_IMPORT, LABEL_METHOD, LABEL_STRUCT, LABEL_TRAIT, LABEL_TYPE_ALIAS,
    LABEL_VARIANT,
};

// ---------------------------------------------------------------------------
// Tree-sitter node type constants
// source: https://github.com/tree-sitter/tree-sitter-typescript/blob/master/typescript/src/node-types.json
// ---------------------------------------------------------------------------

const TS_FUNC_DECL: &str = "function_declaration";
const TS_CLASS_DECL: &str = "class_declaration";
const TS_INTERFACE_DECL: &str = "interface_declaration";
const TS_ENUM_DECL: &str = "enum_declaration";
const TS_TYPE_ALIAS_DECL: &str = "type_alias_declaration";
const TS_IMPORT_STMT: &str = "import_statement";
const TS_EXPORT_STMT: &str = "export_statement";
const TS_LEXICAL_DECL: &str = "lexical_declaration";
const TS_METHOD_DEF: &str = "method_definition";
const TS_PUBLIC_FIELD: &str = "public_field_definition";
const TS_ENUM_BODY: &str = "enum_body";
const TS_CLASS_BODY: &str = "class_body";
const TS_INTERFACE_BODY: &str = "interface_body";
const TS_CALL_EXPR: &str = "call_expression";
const TS_ARROW_FUNC: &str = "arrow_function";
const TS_VAR_DECLARATOR: &str = "variable_declarator";
const TS_PROPERTY_SIGNATURE: &str = "property_signature";
const TS_METHOD_SIGNATURE: &str = "method_signature";

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Parses a single `.ts`/`.tsx` file and extracts typed symbols and relationships.
pub fn parse_typescript_file(source: &str, file_path: &str) -> Result<ParseResult, String> {
    let lang: tree_sitter::Language = tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into();
    let mut parser = Parser::new();
    parser
        .set_language(&lang)
        .map_err(|e| format!("failed to set TypeScript language: {e}"))?;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| "tree-sitter parse returned None".to_string())?;

    let mut ctx = ExtractCtx {
        source,
        file_path,
        nodes: Vec::new(),
        refs: Vec::new(),
    };
    extract_top_level(&mut ctx, tree.root_node(), file_path, false);
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
    #[allow(dead_code)]
    file_path: &'a str,
    nodes: Vec<ExtractedNode>,
    refs: Vec<ExtractedRef>,
}

// ---------------------------------------------------------------------------
// Top-level extraction
// ---------------------------------------------------------------------------

fn extract_top_level(ctx: &mut ExtractCtx, parent: Node, scope: &str, is_exported: bool) {
    let mut cursor = parent.walk();
    for child in parent.children(&mut cursor) {
        match child.kind() {
            TS_FUNC_DECL => extract_function(ctx, child, scope, is_exported),
            TS_CLASS_DECL => extract_class(ctx, child, scope, is_exported),
            TS_INTERFACE_DECL => extract_interface(ctx, child, scope, is_exported),
            TS_ENUM_DECL => extract_enum(ctx, child, scope, is_exported),
            TS_TYPE_ALIAS_DECL => extract_type_alias(ctx, child, scope, is_exported),
            TS_IMPORT_STMT => extract_import(ctx, child, scope),
            TS_EXPORT_STMT => extract_export(ctx, child, scope),
            TS_LEXICAL_DECL => extract_lexical_decl(ctx, child, scope, is_exported),
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Function extraction
// ---------------------------------------------------------------------------

fn extract_function(ctx: &mut ExtractCtx, node: Node, scope: &str, is_exported: bool) {
    let name = node_field_text(ctx.source, node, "name");
    if name.is_empty() {
        return;
    }
    let qn = qual(scope, &name);
    let vis = if is_exported || has_export_keyword(node) { "pub".to_string() } else { String::new() };
    let is_async = has_async_keyword(ctx.source, node);
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
// Class extraction
// ---------------------------------------------------------------------------

fn extract_class(ctx: &mut ExtractCtx, node: Node, scope: &str, is_exported: bool) {
    let name = node_field_text(ctx.source, node, "name");
    if name.is_empty() {
        return;
    }
    let qn = qual(scope, &name);
    let vis = if is_exported || has_export_keyword(node) { "pub".to_string() } else { String::new() };

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

    // Extract heritage (extends/implements)
    extract_class_heritage(ctx, node, &qn);

    // Extract class body: methods and fields
    if let Some(body) = node.child_by_field_name("body") {
        extract_class_body(ctx, body, &qn);
    }
}

fn extract_class_heritage(ctx: &mut ExtractCtx, class_node: Node, class_qn: &str) {
    let mut cursor = class_node.walk();
    for child in class_node.children(&mut cursor) {
        if child.kind() == "class_heritage" {
            let mut hcursor = child.walk();
            for heritage in child.children(&mut hcursor) {
                if heritage.kind() == "extends_clause" {
                    extract_heritage_clause(ctx, heritage, class_qn, "Extends");
                } else if heritage.kind() == "implements_clause" {
                    extract_heritage_clause(ctx, heritage, class_qn, "Implements");
                }
            }
        }
    }
}

fn extract_heritage_clause(
    ctx: &mut ExtractCtx,
    clause: Node,
    class_qn: &str,
    edge_kind: &str,
) {
    let mut cursor = clause.walk();
    for child in clause.children(&mut cursor) {
        if child.kind() == "identifier" || child.kind() == "type_identifier" {
            let name = node_text(ctx.source, child);
            if !name.is_empty() {
                ctx.refs.push(ExtractedRef {
                    kind: edge_kind.to_string(),
                    from_qualified_name: class_qn.to_string(),
                    to_qualified_name: name,
                });
            }
        } else if child.kind() == "generic_type" {
            // class Foo extends Bar<T> — extract "Bar"
            if let Some(type_name) = child.child_by_field_name("name") {
                let name = node_text(ctx.source, type_name);
                if !name.is_empty() {
                    ctx.refs.push(ExtractedRef {
                        kind: edge_kind.to_string(),
                        from_qualified_name: class_qn.to_string(),
                        to_qualified_name: name,
                    });
                }
            }
        }
    }
}

fn extract_class_body(ctx: &mut ExtractCtx, body: Node, class_qn: &str) {
    if body.kind() != TS_CLASS_BODY {
        return;
    }
    let mut cursor = body.walk();
    for child in body.children(&mut cursor) {
        match child.kind() {
            TS_METHOD_DEF => extract_method(ctx, child, class_qn),
            TS_PUBLIC_FIELD => extract_field(ctx, child, class_qn),
            _ => {}
        }
    }
}

fn extract_method(ctx: &mut ExtractCtx, node: Node, class_qn: &str) {
    let name = node_field_text(ctx.source, node, "name");
    if name.is_empty() {
        return;
    }
    let qn = qual(class_qn, &name);
    let is_async = has_async_keyword(ctx.source, node);
    let vis = extract_ts_member_visibility(ctx.source, node);

    ctx.nodes.push(ExtractedNode {
        label: LABEL_METHOD.to_string(),
        name: name.clone(),
        qualified_name: qn.clone(),
        start_line: node.start_position().row as u64 + 1,
        end_line: node.end_position().row as u64 + 1,
        visibility: vis,
        properties: vec![
            ("is_async".to_string(), is_async.to_string()),
            ("receiver_type".to_string(), class_qn.to_string()),
        ],
    });
    ctx.refs.push(ExtractedRef {
        kind: "HasMethod".to_string(),
        from_qualified_name: class_qn.to_string(),
        to_qualified_name: qn.clone(),
    });
    if let Some(body) = node.child_by_field_name("body") {
        extract_call_sites(ctx, body, &qn);
    }
}

fn extract_field(ctx: &mut ExtractCtx, node: Node, class_qn: &str) {
    let name = node_field_text(ctx.source, node, "name");
    if name.is_empty() {
        return;
    }
    let type_ann = node.child_by_field_name("type")
        .map(|n| node_text(ctx.source, n))
        .unwrap_or_default();
    let vis = extract_ts_member_visibility(ctx.source, node);
    let fqn = qual(class_qn, &name);

    ctx.nodes.push(ExtractedNode {
        label: LABEL_FIELD.to_string(),
        name,
        qualified_name: fqn.clone(),
        start_line: node.start_position().row as u64 + 1,
        end_line: node.end_position().row as u64 + 1,
        visibility: vis,
        properties: vec![("type_annotation".to_string(), type_ann)],
    });
    ctx.refs.push(ExtractedRef {
        kind: "HasField".to_string(),
        from_qualified_name: class_qn.to_string(),
        to_qualified_name: fqn,
    });
}

// ---------------------------------------------------------------------------
// Interface extraction (maps to Trait label)
// ---------------------------------------------------------------------------

fn extract_interface(ctx: &mut ExtractCtx, node: Node, scope: &str, is_exported: bool) {
    let name = node_field_text(ctx.source, node, "name");
    if name.is_empty() {
        return;
    }
    let qn = qual(scope, &name);
    let vis = if is_exported || has_export_keyword(node) { "pub".to_string() } else { String::new() };

    ctx.nodes.push(ExtractedNode {
        label: LABEL_TRAIT.to_string(),
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

    // Extract extends clauses for interface
    extract_interface_extends(ctx, node, &qn);

    // Extract interface body (method/property signatures)
    if let Some(body) = node.child_by_field_name("body") {
        extract_interface_body(ctx, body, &qn);
    }
}

fn extract_interface_extends(ctx: &mut ExtractCtx, iface_node: Node, iface_qn: &str) {
    let mut cursor = iface_node.walk();
    for child in iface_node.children(&mut cursor) {
        if child.kind() == "extends_type_clause" {
            let mut hcursor = child.walk();
            for hchild in child.children(&mut hcursor) {
                if hchild.kind() == "type_identifier" || hchild.kind() == "identifier" {
                    let name = node_text(ctx.source, hchild);
                    if !name.is_empty() {
                        ctx.refs.push(ExtractedRef {
                            kind: "Extends".to_string(),
                            from_qualified_name: iface_qn.to_string(),
                            to_qualified_name: name,
                        });
                    }
                }
            }
        }
    }
}

fn extract_interface_body(ctx: &mut ExtractCtx, body: Node, iface_qn: &str) {
    if body.kind() != TS_INTERFACE_BODY && body.kind() != "object_type" {
        return;
    }
    let mut cursor = body.walk();
    for child in body.children(&mut cursor) {
        match child.kind() {
            TS_METHOD_SIGNATURE => {
                let name = node_field_text(ctx.source, child, "name");
                if name.is_empty() { continue; }
                let mqn = qual(iface_qn, &name);
                ctx.nodes.push(ExtractedNode {
                    label: LABEL_METHOD.to_string(),
                    name: name.clone(),
                    qualified_name: mqn.clone(),
                    start_line: child.start_position().row as u64 + 1,
                    end_line: child.end_position().row as u64 + 1,
                    visibility: String::new(),
                    properties: vec![
                        ("is_async".to_string(), "false".to_string()),
                        ("receiver_type".to_string(), iface_qn.to_string()),
                    ],
                });
                ctx.refs.push(ExtractedRef {
                    kind: "HasMethod".to_string(),
                    from_qualified_name: iface_qn.to_string(),
                    to_qualified_name: mqn,
                });
            }
            TS_PROPERTY_SIGNATURE => {
                let name = node_field_text(ctx.source, child, "name");
                if name.is_empty() { continue; }
                let type_ann = child.child_by_field_name("type")
                    .map(|n| node_text(ctx.source, n))
                    .unwrap_or_default();
                let fqn = qual(iface_qn, &name);
                ctx.nodes.push(ExtractedNode {
                    label: LABEL_FIELD.to_string(),
                    name,
                    qualified_name: fqn.clone(),
                    start_line: child.start_position().row as u64 + 1,
                    end_line: child.end_position().row as u64 + 1,
                    visibility: String::new(),
                    properties: vec![("type_annotation".to_string(), type_ann)],
                });
                ctx.refs.push(ExtractedRef {
                    kind: "HasField".to_string(),
                    from_qualified_name: iface_qn.to_string(),
                    to_qualified_name: fqn,
                });
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Enum extraction
// ---------------------------------------------------------------------------

fn extract_enum(ctx: &mut ExtractCtx, node: Node, scope: &str, is_exported: bool) {
    let name = node_field_text(ctx.source, node, "name");
    if name.is_empty() {
        return;
    }
    let qn = qual(scope, &name);
    let vis = if is_exported || has_export_keyword(node) { "pub".to_string() } else { String::new() };

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

    // Extract enum members
    if let Some(body) = node.child_by_field_name("body") {
        extract_enum_members(ctx, body, &qn);
    }
}

fn extract_enum_members(ctx: &mut ExtractCtx, body: Node, enum_qn: &str) {
    if body.kind() != TS_ENUM_BODY {
        return;
    }
    let mut cursor = body.walk();
    for child in body.children(&mut cursor) {
        if child.kind() == "enum_assignment" || child.kind() == "property_identifier" {
            let name = if child.kind() == "enum_assignment" {
                node_field_text(ctx.source, child, "name")
            } else {
                node_text(ctx.source, child)
            };
            if name.is_empty() { continue; }
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
}

// ---------------------------------------------------------------------------
// Type alias extraction
// ---------------------------------------------------------------------------

fn extract_type_alias(ctx: &mut ExtractCtx, node: Node, scope: &str, is_exported: bool) {
    let name = node_field_text(ctx.source, node, "name");
    if name.is_empty() {
        return;
    }
    let qn = qual(scope, &name);
    let vis = if is_exported || has_export_keyword(node) { "pub".to_string() } else { String::new() };
    let target = node.child_by_field_name("value")
        .map(|n| node_text(ctx.source, n))
        .unwrap_or_default();

    ctx.nodes.push(ExtractedNode {
        label: LABEL_TYPE_ALIAS.to_string(),
        name,
        qualified_name: qn.clone(),
        start_line: node.start_position().row as u64 + 1,
        end_line: node.end_position().row as u64 + 1,
        visibility: vis,
        properties: vec![("target_type".to_string(), target)],
    });
    ctx.refs.push(ExtractedRef {
        kind: "Defines".to_string(),
        from_qualified_name: scope.to_string(),
        to_qualified_name: qn,
    });
}

// ---------------------------------------------------------------------------
// Import extraction
// ---------------------------------------------------------------------------

fn extract_import(ctx: &mut ExtractCtx, node: Node, scope: &str) {
    let source_node = node.child_by_field_name("source");
    let path = source_node
        .map(|n| {
            let text = node_text(ctx.source, n);
            // Strip quotes from the import path
            text.trim_matches(|c| c == '\'' || c == '"').to_string()
        })
        .unwrap_or_default();

    // Normalize path separators to ::
    let normalized_path = path.replace('/', "::");

    // Find import clause children
    let mut cursor = node.walk();
    let mut found_any = false;
    for child in node.children(&mut cursor) {
        if child.kind() == "import_clause" {
            extract_import_clause(ctx, child, scope, &normalized_path, node);
            found_any = true;
        }
    }
    if !found_any && !normalized_path.is_empty() {
        // Side-effect import: import 'foo'
        emit_ts_import(ctx, scope, &normalized_path, "", false, node);
    }
}

fn extract_import_clause(
    ctx: &mut ExtractCtx,
    clause: Node,
    scope: &str,
    module_path: &str,
    import_node: Node,
) {
    let mut cursor = clause.walk();
    for child in clause.children(&mut cursor) {
        match child.kind() {
            "identifier" => {
                // Default import: import Foo from 'bar'
                let name = node_text(ctx.source, child);
                let full_path = format!("{module_path}::default");
                emit_ts_import(ctx, scope, &full_path, &name, false, import_node);
            }
            "named_imports" => {
                extract_named_imports(ctx, child, scope, module_path, import_node);
            }
            "namespace_import" => {
                // import * as Foo from 'bar'
                let alias = {
                    let mut found = child.child_by_field_name("name");
                    if found.is_none() {
                        let mut c = child.walk();
                        found = child.children(&mut c).find(|n| n.kind() == "identifier");
                    }
                    found.map(|n| node_text(ctx.source, n)).unwrap_or_default()
                };
                emit_ts_import(ctx, scope, module_path, &alias, true, import_node);
            }
            _ => {}
        }
    }
}

fn extract_named_imports(
    ctx: &mut ExtractCtx,
    node: Node,
    scope: &str,
    module_path: &str,
    import_node: Node,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "import_specifier" {
            let name_node = child.child_by_field_name("name");
            let alias_node = child.child_by_field_name("alias");
            let name = name_node.map(|n| node_text(ctx.source, n)).unwrap_or_default();
            let alias = alias_node.map(|n| node_text(ctx.source, n)).unwrap_or_default();
            let full_path = format!("{module_path}::{name}");
            emit_ts_import(ctx, scope, &full_path, &alias, false, import_node);
        }
    }
}

fn emit_ts_import(
    ctx: &mut ExtractCtx,
    scope: &str,
    path: &str,
    alias: &str,
    is_glob: bool,
    node: Node,
) {
    if path.is_empty() {
        return;
    }
    let display_name = if !alias.is_empty() {
        alias.to_string()
    } else if is_glob {
        format!("{path}::*")
    } else {
        path.to_string()
    };
    let qn = qual(scope, &display_name);
    ctx.nodes.push(ExtractedNode {
        label: LABEL_IMPORT.to_string(),
        name: display_name,
        qualified_name: qn.clone(),
        start_line: node.start_position().row as u64 + 1,
        end_line: node.end_position().row as u64 + 1,
        visibility: String::new(),
        properties: vec![
            ("path".to_string(), path.to_string()),
            ("alias".to_string(), alias.to_string()),
            ("is_glob".to_string(), is_glob.to_string()),
        ],
    });
    ctx.refs.push(ExtractedRef {
        kind: "Defines".to_string(),
        from_qualified_name: scope.to_string(),
        to_qualified_name: qn,
    });
}

// ---------------------------------------------------------------------------
// Export statement extraction
// ---------------------------------------------------------------------------

fn extract_export(ctx: &mut ExtractCtx, node: Node, scope: &str) {
    // Export wraps another declaration — extract the inner with is_exported=true
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            TS_FUNC_DECL => extract_function(ctx, child, scope, true),
            TS_CLASS_DECL => extract_class(ctx, child, scope, true),
            TS_INTERFACE_DECL => extract_interface(ctx, child, scope, true),
            TS_ENUM_DECL => extract_enum(ctx, child, scope, true),
            TS_TYPE_ALIAS_DECL => extract_type_alias(ctx, child, scope, true),
            TS_LEXICAL_DECL => extract_lexical_decl(ctx, child, scope, true),
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Lexical declaration extraction (const/let)
// ---------------------------------------------------------------------------

fn extract_lexical_decl(ctx: &mut ExtractCtx, node: Node, scope: &str, is_exported: bool) {
    let is_const = is_const_declaration(ctx.source, node);
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == TS_VAR_DECLARATOR {
            extract_variable_declarator(ctx, child, scope, is_const, is_exported);
        }
    }
}

fn extract_variable_declarator(
    ctx: &mut ExtractCtx,
    node: Node,
    scope: &str,
    is_const: bool,
    is_exported: bool,
) {
    let name = node_field_text(ctx.source, node, "name");
    if name.is_empty() {
        return;
    }

    let value = node.child_by_field_name("value");
    let is_arrow = value.map_or(false, |v| v.kind() == TS_ARROW_FUNC);

    if is_arrow {
        // const foo = () => {} — extract as Function
        let arrow = value.unwrap();
        let qn = qual(scope, &name);
        let vis = if is_exported { "pub".to_string() } else { String::new() };
        let is_async = has_async_keyword(ctx.source, arrow);
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
        if let Some(body) = arrow.child_by_field_name("body") {
            extract_call_sites(ctx, body, &qn);
        }
    } else if is_const {
        // const FOO = ... — extract as Constant
        let qn = qual(scope, &name);
        let vis = if is_exported { "pub".to_string() } else { String::new() };
        let type_ann = node.child_by_field_name("type")
            .map(|n| node_text(ctx.source, n))
            .unwrap_or_default();
        ctx.nodes.push(ExtractedNode {
            label: LABEL_CONSTANT.to_string(),
            name,
            qualified_name: qn.clone(),
            start_line: node.start_position().row as u64 + 1,
            end_line: node.end_position().row as u64 + 1,
            visibility: vis,
            properties: vec![("type_annotation".to_string(), type_ann)],
        });
        ctx.refs.push(ExtractedRef {
            kind: "Defines".to_string(),
            from_qualified_name: scope.to_string(),
            to_qualified_name: qn,
        });
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
// Helpers
// ---------------------------------------------------------------------------

fn has_export_keyword(node: Node) -> bool {
    if let Some(prev) = node.prev_sibling() {
        if prev.kind() == "export" {
            return true;
        }
    }
    false
}

fn has_async_keyword(source: &str, node: Node) -> bool {
    let text = &source[node.byte_range()];
    text.starts_with("async ")
}

fn is_const_declaration(source: &str, node: Node) -> bool {
    let text = &source[node.byte_range()];
    text.starts_with("const ")
}

fn extract_ts_member_visibility(source: &str, node: Node) -> String {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "accessibility_modifier" {
            return node_text(source, child);
        }
    }
    String::new()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_typescript() {
        let src = r#"
import { Router } from 'express';
import * as fs from 'fs';

export const MAX_RETRIES = 3;

export function greet(name: string): string {
    return `Hello, ${name}`;
}

export async function fetchData(url: string): Promise<Response> {
    return fetch(url);
}

export const handler = (req: Request) => {
    greet("world");
};

export class Animal {
    public name: string;
    private age: number;

    constructor(name: string) {
        this.name = name;
    }

    speak(): string {
        return "";
    }
}

export class Dog extends Animal {
    speak(): string {
        return "Woof";
    }
}

export interface Serializable {
    serialize(): string;
    readonly id: number;
}

export enum Color {
    Red = "RED",
    Green = "GREEN",
    Blue = "BLUE",
}

export type StringOrNumber = string | number;
"#;
        let result = parse_typescript_file(src, "test.ts").expect("parse should succeed");
        let labels: Vec<&str> = result.nodes.iter().map(|n| n.label.as_str()).collect();
        let names: Vec<&str> = result.nodes.iter().map(|n| n.name.as_str()).collect();

        // Functions
        assert!(labels.contains(&"Function"), "missing Function");
        assert!(names.contains(&"greet"), "missing greet");
        assert!(names.contains(&"fetchData"), "missing fetchData");
        assert!(names.contains(&"handler"), "missing handler (arrow fn)");

        // Classes (Struct)
        assert!(labels.contains(&"Struct"), "missing Struct (class)");
        assert!(names.contains(&"Animal"), "missing Animal");
        assert!(names.contains(&"Dog"), "missing Dog");

        // Interface (Trait)
        assert!(labels.contains(&"Trait"), "missing Trait (interface)");
        assert!(names.contains(&"Serializable"), "missing Serializable");

        // Enum
        assert!(labels.contains(&"Enum"), "missing Enum");
        assert!(names.contains(&"Color"), "missing Color");

        // TypeAlias
        assert!(labels.contains(&"TypeAlias"), "missing TypeAlias");
        assert!(names.contains(&"StringOrNumber"), "missing StringOrNumber");

        // Methods
        assert!(labels.contains(&"Method"), "missing Method");

        // Fields
        assert!(labels.contains(&"Field"), "missing Field");

        // Imports
        assert!(labels.contains(&"Import"), "missing Import");

        // Constants
        assert!(labels.contains(&"Constant"), "missing Constant");
        assert!(names.contains(&"MAX_RETRIES"), "missing MAX_RETRIES");

        // Async detection
        let fetch_fn = result.nodes.iter().find(|n| n.name == "fetchData").unwrap();
        let is_async = fetch_fn.properties.iter().find(|(k, _)| k == "is_async").unwrap();
        assert_eq!(is_async.1, "true");

        // Export = pub visibility
        let greet_fn = result.nodes.iter().find(|n| n.name == "greet").unwrap();
        assert_eq!(greet_fn.visibility, "pub");

        // Extends edge for Dog extends Animal
        let extends = result.refs.iter()
            .any(|r| r.kind == "Extends" && r.from_qualified_name.contains("Dog"));
        assert!(extends, "missing Extends edge for Dog");
    }

    #[test]
    fn test_typescript_imports() {
        let src = r#"
import { foo, bar as baz } from './module';
import * as utils from '../utils';
import defaultExport from 'package';
"#;
        let result = parse_typescript_file(src, "test.ts").expect("parse");
        let imports: Vec<_> = result.nodes.iter()
            .filter(|n| n.label == "Import")
            .collect();

        assert!(imports.len() >= 3, "expected at least 3 imports, got {}", imports.len());

        // Check path normalization (/ -> ::)
        let has_normalized = imports.iter().any(|n|
            n.properties.iter().any(|(k, v)| k == "path" && v.contains("::"))
        );
        assert!(has_normalized, "import paths should be normalized to :: separator");
    }
}
