// parser::python — tree-sitter-based Python source parser for code-intelligence graph.
//
// Parses a single `.py` file and extracts typed symbols matching the
// graph_store schema. Produces the same ParseResult types as the Rust parser.
//
// Grammar reference: https://github.com/tree-sitter/tree-sitter-python

use tree_sitter::{Node, Parser};

use super::{
    node_field_text, node_text, qual, ExtractedNode, ExtractedRef, ParseResult,
    LABEL_CALL_SITE, LABEL_CONSTANT, LABEL_FUNCTION, LABEL_IMPORT, LABEL_METHOD,
    LABEL_STRUCT,
};

// ---------------------------------------------------------------------------
// Tree-sitter node type constants
// source: https://github.com/tree-sitter/tree-sitter-python/blob/master/src/node-types.json
// ---------------------------------------------------------------------------

const TS_FUNCTION_DEF: &str = "function_definition";
const TS_CLASS_DEF: &str = "class_definition";
const TS_IMPORT_STMT: &str = "import_statement";
const TS_IMPORT_FROM: &str = "import_from_statement";
const TS_DECORATED_DEF: &str = "decorated_definition";
const TS_EXPRESSION_STMT: &str = "expression_statement";
const TS_ASSIGNMENT: &str = "assignment";
const TS_CALL: &str = "call";

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Parses a single `.py` file and extracts typed symbols and relationships.
pub fn parse_python_file(source: &str, file_path: &str) -> Result<ParseResult, String> {
    let lang: tree_sitter::Language = tree_sitter_python::LANGUAGE.into();
    let mut parser = Parser::new();
    parser
        .set_language(&lang)
        .map_err(|e| format!("failed to set Python language: {e}"))?;
    // source: H2 fix — 5 s per-file tree-sitter timeout (see super::PARSE_TIMEOUT_MICROS).
    parser.set_timeout_micros(super::PARSE_TIMEOUT_MICROS);
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| "parse_timeout_or_none: tree-sitter returned None".to_string())?;

    let mut ctx = ExtractCtx {
        source,
        file_path,
        nodes: Vec::new(),
        refs: Vec::new(),
        emitted_qns: std::collections::HashSet::new(),
    };
    extract_top_level(&mut ctx, tree.root_node(), file_path, None);
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
    /// Qualified names already emitted in this file. Used to disambiguate
    /// Python @property/@setter pairs and other same-named overloads
    /// (Rust impl Trait for X & inherent impl can also share method names).
    emitted_qns: std::collections::HashSet<String>,
}

impl<'a> ExtractCtx<'a> {
    /// Returns a unique qn: the input if unseen, else `qn@{start_line}` so
    /// every Method/Function node has a unique primary key while preserving
    /// the readable name for resolver name-based lookups.
    fn dedup_qn(&mut self, qn: String, start_line: u64) -> String {
        if self.emitted_qns.insert(qn.clone()) {
            return qn;
        }
        let unique = format!("{qn}@{start_line}");
        self.emitted_qns.insert(unique.clone());
        unique
    }
}

// ---------------------------------------------------------------------------
// Top-level extraction
// ---------------------------------------------------------------------------

fn extract_top_level(
    ctx: &mut ExtractCtx,
    parent: Node,
    scope: &str,
    enclosing_class: Option<&str>,
) {
    let mut cursor = parent.walk();
    for child in parent.children(&mut cursor) {
        match child.kind() {
            TS_FUNCTION_DEF => {
                extract_function_or_method(ctx, child, scope, enclosing_class, &[]);
            }
            TS_CLASS_DEF => extract_class(ctx, child, scope),
            TS_IMPORT_STMT => extract_import(ctx, child, scope),
            TS_IMPORT_FROM => extract_import_from(ctx, child, scope),
            TS_DECORATED_DEF => extract_decorated(ctx, child, scope, enclosing_class),
            TS_EXPRESSION_STMT => {
                // Check for module-level constant assignments
                if enclosing_class.is_none() {
                    extract_module_constant(ctx, child, scope);
                }
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Function / Method extraction
// ---------------------------------------------------------------------------

fn extract_function_or_method(
    ctx: &mut ExtractCtx,
    node: Node,
    scope: &str,
    enclosing_class: Option<&str>,
    decorators: &[String],
) {
    let name = node_field_text(ctx.source, node, "name");
    if name.is_empty() {
        return;
    }

    let is_async = is_async_function(ctx.source, node);
    let visibility = python_visibility(&name);
    let raw_qn = qual(scope, &name);
    let start_line = node.start_position().row as u64 + 1;
    // Disambiguate @property/@setter/@deleter overloads (and any other
    // legitimately-same-named symbols within the same scope) so each
    // gets a unique primary key. Resolver name-based lookups still work.
    let qn = ctx.dedup_qn(raw_qn, start_line);

    let mut props = vec![
        ("is_async".to_string(), is_async.to_string()),
    ];
    if !decorators.is_empty() {
        props.push(("decorators".to_string(), decorators.join(",")));
    }

    if let Some(class_name) = enclosing_class {
        // It's a method
        props.push(("receiver_type".to_string(), class_name.to_string()));
        ctx.nodes.push(ExtractedNode {
            label: LABEL_METHOD.to_string(),
            name: name.clone(),
            qualified_name: qn.clone(),
            start_line,
            end_line: node.end_position().row as u64 + 1,
            visibility,
            properties: props,
        });
        ctx.refs.push(ExtractedRef {
            kind: "HasMethod".to_string(),
            from_qualified_name: scope.to_string(),
            to_qualified_name: qn.clone(),
        });
    } else {
        // Top-level function
        ctx.nodes.push(ExtractedNode {
            label: LABEL_FUNCTION.to_string(),
            name: name.clone(),
            qualified_name: qn.clone(),
            start_line,
            end_line: node.end_position().row as u64 + 1,
            visibility,
            properties: props,
        });
        ctx.refs.push(ExtractedRef {
            kind: "Defines".to_string(),
            from_qualified_name: scope.to_string(),
            to_qualified_name: qn.clone(),
        });
    }

    // Extract call sites from function body
    if let Some(body) = node.child_by_field_name("body") {
        extract_call_sites(ctx, body, &qn);
    }
}

// ---------------------------------------------------------------------------
// Class extraction (maps to Struct label — closest equivalent)
// ---------------------------------------------------------------------------

fn extract_class(ctx: &mut ExtractCtx, node: Node, scope: &str) {
    let name = node_field_text(ctx.source, node, "name");
    if name.is_empty() {
        return;
    }
    let qn = qual(scope, &name);
    let visibility = python_visibility(&name);

    ctx.nodes.push(ExtractedNode {
        label: LABEL_STRUCT.to_string(),
        name: name.clone(),
        qualified_name: qn.clone(),
        start_line: node.start_position().row as u64 + 1,
        end_line: node.end_position().row as u64 + 1,
        visibility,
        properties: vec![],
    });
    ctx.refs.push(ExtractedRef {
        kind: "Defines".to_string(),
        from_qualified_name: scope.to_string(),
        to_qualified_name: qn.clone(),
    });

    // Extract base classes (superclass_list is the "superclasses" field)
    extract_base_classes(ctx, node, &qn);

    // Recurse into class body for methods and nested classes
    if let Some(body) = node.child_by_field_name("body") {
        extract_top_level(ctx, body, &qn, Some(&qn));
    }
}

fn extract_base_classes(ctx: &mut ExtractCtx, class_node: Node, class_qn: &str) {
    let superclasses = match class_node.child_by_field_name("superclasses") {
        Some(s) => s,
        None => return,
    };
    let mut cursor = superclasses.walk();
    for child in superclasses.children(&mut cursor) {
        let kind = child.kind();
        if kind == "identifier" || kind == "attribute" {
            let base_name = node_text(ctx.source, child);
            if !base_name.is_empty() {
                // Normalize dots to :: for consistent qualified names
                let normalized = base_name.replace('.', "::");
                ctx.refs.push(ExtractedRef {
                    kind: "Extends".to_string(),
                    from_qualified_name: class_qn.to_string(),
                    to_qualified_name: normalized,
                });
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Decorated definition extraction
// ---------------------------------------------------------------------------

fn extract_decorated(
    ctx: &mut ExtractCtx,
    node: Node,
    scope: &str,
    enclosing_class: Option<&str>,
) {
    let mut decorators = Vec::new();
    let mut definition = None;

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "decorator" {
            let text = node_text(ctx.source, child);
            // Strip the leading '@'
            let dec_name = text.trim_start_matches('@').trim().to_string();
            decorators.push(dec_name);
        } else if child.kind() == TS_FUNCTION_DEF {
            definition = Some(child);
        } else if child.kind() == TS_CLASS_DEF {
            // Decorated class — extract the class, ignoring decorators for now
            extract_class(ctx, child, scope);
            return;
        }
    }

    if let Some(func_node) = definition {
        extract_function_or_method(ctx, func_node, scope, enclosing_class, &decorators);
    }
}

// ---------------------------------------------------------------------------
// Import extraction
// ---------------------------------------------------------------------------

fn extract_import(ctx: &mut ExtractCtx, node: Node, scope: &str) {
    // `import foo` / `import foo.bar` / `import foo as f`
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "dotted_name" {
            let path = node_text(ctx.source, child).replace('.', "::");
            emit_import(ctx, scope, &path, "", false, node);
        } else if child.kind() == "aliased_import" {
            let name_node = child.child_by_field_name("name");
            let alias_node = child.child_by_field_name("alias");
            let path = name_node.map(|n| node_text(ctx.source, n).replace('.', "::")).unwrap_or_default();
            let alias = alias_node.map(|n| node_text(ctx.source, n)).unwrap_or_default();
            emit_import(ctx, scope, &path, &alias, false, node);
        }
    }
}

fn extract_import_from(ctx: &mut ExtractCtx, node: Node, scope: &str) {
    // `from foo import bar` / `from foo import *` / `from foo import bar as b`
    let module_name = node.child_by_field_name("module_name")
        .map(|n| node_text(ctx.source, n).replace('.', "::"))
        .unwrap_or_default();

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "wildcard_import" {
            emit_import(ctx, scope, &module_name, "", true, node);
            return;
        }
    }

    // Iterate named imports
    let mut cursor2 = node.walk();
    for child in node.children(&mut cursor2) {
        if child.kind() == "dotted_name" || child.kind() == "identifier" {
            // Skip the module_name node itself
            if Some(child.id()) == node.child_by_field_name("module_name").map(|n| n.id()) {
                continue;
            }
            let name = node_text(ctx.source, child);
            let full_path = if module_name.is_empty() {
                name.clone()
            } else {
                format!("{module_name}::{name}")
            };
            emit_import(ctx, scope, &full_path, "", false, node);
        } else if child.kind() == "aliased_import" {
            let name_node = child.child_by_field_name("name");
            let alias_node = child.child_by_field_name("alias");
            let name = name_node.map(|n| node_text(ctx.source, n)).unwrap_or_default();
            let alias = alias_node.map(|n| node_text(ctx.source, n)).unwrap_or_default();
            let full_path = if module_name.is_empty() {
                name
            } else {
                format!("{module_name}::{name}")
            };
            emit_import(ctx, scope, &full_path, &alias, false, node);
        }
    }
}

fn emit_import(
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
// Module-level constant extraction (UPPER_SNAKE assignments)
// ---------------------------------------------------------------------------

fn extract_module_constant(ctx: &mut ExtractCtx, expr_stmt: Node, scope: &str) {
    let mut cursor = expr_stmt.walk();
    for child in expr_stmt.children(&mut cursor) {
        if child.kind() != TS_ASSIGNMENT {
            continue;
        }
        let left = match child.child_by_field_name("left") {
            Some(l) if l.kind() == "identifier" => l,
            _ => continue,
        };
        let name = node_text(ctx.source, left);
        if !is_upper_snake_case(&name) {
            continue;
        }
        // Check for type annotation
        let type_ann = child.child_by_field_name("type")
            .map(|n| node_text(ctx.source, n))
            .unwrap_or_default();
        let qn = qual(scope, &name);
        ctx.nodes.push(ExtractedNode {
            label: LABEL_CONSTANT.to_string(),
            name,
            qualified_name: qn.clone(),
            start_line: child.start_position().row as u64 + 1,
            end_line: child.end_position().row as u64 + 1,
            visibility: String::new(),
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
        if node.kind() == TS_CALL {
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
    if callee.is_empty() {
        return;
    }
    // Skip method calls (x.foo()) for now — same as Rust parser scope
    if callee.contains('.') {
        return;
    }
    let line = node.start_position().row as u64 + 1;
    let col = node.start_position().column as u64;
    // Chained calls (f()()) share start_byte because the outer call's
    // function child is the inner call (same starting token). Use the
    // (start_byte, end_byte) span — outer call ends after the trailing
    // ``)``, inner ends earlier — to give every call_expression a unique
    // primary key while preserving the human-readable line:col prefix.
    let start_byte = node.start_byte() as u64;
    let end_byte = node.end_byte() as u64;
    let cs_id = format!("{caller_qn}::call@{line}:{col}#{start_byte}-{end_byte}");
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

/// Python visibility convention: leading underscore = private.
fn python_visibility(name: &str) -> String {
    // Dunder methods (__init__, __str__, etc.) are public by convention
    if name.starts_with("__") && name.ends_with("__") {
        return String::new();
    }
    if name.starts_with('_') {
        "private".to_string()
    } else {
        String::new()
    }
}

/// Checks if a name is UPPER_SNAKE_CASE (Python constant convention).
fn is_upper_snake_case(name: &str) -> bool {
    !name.is_empty()
        && name.chars().all(|c| c.is_ascii_uppercase() || c == '_' || c.is_ascii_digit())
        && name.chars().any(|c| c.is_ascii_uppercase())
}

/// Detects async def by checking if "async" keyword precedes "def".
fn is_async_function(source: &str, node: Node) -> bool {
    // In tree-sitter-python, async functions are still function_definition
    // but the parent or a sibling might be "async" keyword, or the node text starts with "async"
    let text = &source[node.byte_range()];
    text.starts_with("async ")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_python() {
        let src = r#"
import os
from pathlib import Path
from typing import *

MAX_SIZE = 100
_PRIVATE = "hidden"

def greet(name: str) -> str:
    return f"Hello, {name}"

async def fetch_data(url):
    pass

class Animal:
    def __init__(self, name):
        self.name = name

    def speak(self):
        pass

class Dog(Animal):
    def speak(self):
        return "Woof"
"#;
        let result = parse_python_file(src, "test.py").expect("parse should succeed");
        let labels: Vec<&str> = result.nodes.iter().map(|n| n.label.as_str()).collect();
        let names: Vec<&str> = result.nodes.iter().map(|n| n.name.as_str()).collect();

        // Functions
        assert!(labels.contains(&"Function"), "missing Function");
        assert!(names.contains(&"greet"), "missing greet function");
        assert!(names.contains(&"fetch_data"), "missing fetch_data function");

        // Classes (mapped to Struct)
        assert!(labels.contains(&"Struct"), "missing Struct (class)");
        assert!(names.contains(&"Animal"), "missing Animal class");
        assert!(names.contains(&"Dog"), "missing Dog class");

        // Methods
        assert!(labels.contains(&"Method"), "missing Method");
        assert!(names.contains(&"__init__"), "missing __init__ method");
        assert!(names.contains(&"speak"), "missing speak method");

        // Imports
        assert!(labels.contains(&"Import"), "missing Import");

        // Constants
        assert!(labels.contains(&"Constant"), "missing Constant");
        assert!(names.contains(&"MAX_SIZE"), "missing MAX_SIZE constant");

        // Async detection
        let fetch = result.nodes.iter().find(|n| n.name == "fetch_data").unwrap();
        let is_async = fetch.properties.iter().find(|(k, _)| k == "is_async").unwrap();
        assert_eq!(is_async.1, "true");

        // Extends edge for Dog(Animal)
        let extends = result.refs.iter()
            .any(|r| r.kind == "Extends" && r.from_qualified_name.contains("Dog"));
        assert!(extends, "missing Extends edge for Dog");

        // Glob import
        let glob_import = result.nodes.iter()
            .any(|n| n.label == "Import" && n.properties.iter().any(|(k, v)| k == "is_glob" && v == "true"));
        assert!(glob_import, "missing glob import for 'from typing import *'");
    }

    #[test]
    fn test_python_visibility() {
        assert_eq!(python_visibility("public_func"), "");
        assert_eq!(python_visibility("_private_func"), "private");
        assert_eq!(python_visibility("__mangled"), "private");
        // Dunder methods (__init__, __str__, etc.) are public by convention
        assert_eq!(python_visibility("__init__"), "");
        assert_eq!(python_visibility("__str__"), "");
    }

    #[test]
    fn test_upper_snake_case() {
        assert!(is_upper_snake_case("MAX_SIZE"));
        assert!(is_upper_snake_case("FOO"));
        assert!(is_upper_snake_case("HTTP_200"));
        assert!(!is_upper_snake_case("foo"));
        assert!(!is_upper_snake_case("Foo_Bar"));
        assert!(!is_upper_snake_case("_"));
    }

    #[test]
    fn test_python_import_normalization() {
        let src = r#"
import os.path
from collections.abc import Mapping
"#;
        let result = parse_python_file(src, "test.py").expect("parse");
        let imports: Vec<_> = result.nodes.iter()
            .filter(|n| n.label == "Import")
            .collect();

        // os.path should be normalized to os::path
        let has_normalized = imports.iter().any(|n|
            n.properties.iter().any(|(k, v)| k == "path" && v == "os::path")
        );
        assert!(has_normalized, "import paths should be normalized to :: separator");
    }
}
