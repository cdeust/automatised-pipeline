// indexer — Walk + Parse + Persist pipeline for multi-language codebases.
//
// Wires graph_store (step 1) and parser module (step 2) into an indexer
// that processes a full directory of source files. Supports Rust, Python,
// and TypeScript. Zero dependency on main.rs.

use crate::graph_store::GraphStore;
use crate::parser::{self, Language};
use std::path::{Path, PathBuf};
use std::time::Instant;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

pub struct IndexResult {
    pub graph_path: PathBuf,
    pub node_count: u64,
    pub edge_count: u64,
    pub files_indexed: u64,
    pub elapsed_ms: u64,
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Indexes source files under `codebase_path` into a LadybugDB graph
/// at `graph_path`. Continues on per-file parse errors.
/// Convenience wrapper that auto-detects language by file extension.
#[allow(dead_code)]
pub fn index_codebase(codebase_path: &Path, graph_path: &Path) -> Result<IndexResult, String> {
    index_codebase_with_language(codebase_path, graph_path, None)
}

/// Indexes source files with an optional language filter.
pub fn index_codebase_with_language(
    codebase_path: &Path,
    graph_path: &Path,
    language_filter: Option<Language>,
) -> Result<IndexResult, String> {
    let start = Instant::now();
    let store = GraphStore::open_or_create(graph_path)?;
    store.create_schema()?;

    let source_files = collect_source_files(codebase_path, language_filter)?;
    let mut files_indexed: u64 = 0;
    let mut dir_nodes_inserted = std::collections::HashSet::<PathBuf>::new();

    for file_path in &source_files {
        let rel = relative_path(codebase_path, file_path);
        let rel_str = rel.to_string_lossy();
        insert_ancestor_dirs(&store, codebase_path, file_path, &mut dir_nodes_inserted)?;
        insert_file_node(&store, file_path, &rel_str)?;
        insert_dir_file_edge(&store, &rel)?;
        match index_single_file(&store, file_path, &rel_str) {
            Ok(()) => files_indexed += 1,
            Err(e) => eprintln!("indexer: skipping {}: {e}", rel_str),
        }
    }

    let node_count = store.node_count()?;
    let edge_count = store.edge_count()?;
    let elapsed_ms = start.elapsed().as_millis() as u64;

    Ok(IndexResult {
        graph_path: graph_path.to_path_buf(),
        node_count,
        edge_count,
        files_indexed,
        elapsed_ms,
    })
}

// ---------------------------------------------------------------------------
// Directory walking
// ---------------------------------------------------------------------------

/// Recursively collects source files, skipping hidden dirs, target/, node_modules/.
/// When `language_filter` is Some, only collects files for that language.
/// When None, collects all files with recognized extensions.
fn collect_source_files(
    root: &Path,
    language_filter: Option<Language>,
) -> Result<Vec<PathBuf>, String> {
    let mut result = Vec::new();
    walk_dir_recursive(root, &mut result, language_filter)?;
    result.sort();
    Ok(result)
}

fn walk_dir_recursive(
    dir: &Path,
    out: &mut Vec<PathBuf>,
    language_filter: Option<Language>,
) -> Result<(), String> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| format!("read_dir {}: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("dir entry: {e}"))?;
        let path = entry.path();
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if should_skip(&name_str) {
            continue;
        }
        if path.is_dir() {
            walk_dir_recursive(&path, out, language_filter)?;
        } else if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            let detected = Language::from_extension(ext);
            match (language_filter, detected) {
                (Some(filter), Some(lang)) if filter == lang => out.push(path),
                (None, Some(_)) => out.push(path),
                _ => {}
            }
        }
    }
    Ok(())
}

/// Returns true for directories that should be skipped during walk.
fn should_skip(name: &str) -> bool {
    name.starts_with('.')
        || name == "target"
        || name == "node_modules"
        || name == "__pycache__"
        || name == ".venv"
        || name == "venv"
}

// ---------------------------------------------------------------------------
// Directory and File node insertion
// ---------------------------------------------------------------------------

/// Inserts Directory nodes for all ancestor dirs of a file (relative to root).
fn insert_ancestor_dirs(
    store: &GraphStore,
    root: &Path,
    file_path: &Path,
    seen: &mut std::collections::HashSet<PathBuf>,
) -> Result<(), String> {
    let rel = relative_path(root, file_path);
    let mut current = PathBuf::new();
    // Walk each component except the filename itself.
    if let Some(parent) = rel.parent() {
        for component in parent.components() {
            let prev = current.clone();
            current.push(component);
            if seen.contains(&current) {
                continue;
            }
            seen.insert(current.clone());
            let dir_id = current.to_string_lossy();
            let dir_name = component.as_os_str().to_string_lossy();
            insert_directory_node(store, &dir_id, &dir_name)?;
            if !prev.as_os_str().is_empty() {
                insert_dir_dir_edge(store, &prev.to_string_lossy(), &dir_id)?;
            }
        }
    }
    Ok(())
}

fn insert_directory_node(store: &GraphStore, id: &str, name: &str) -> Result<(), String> {
    store.insert_node("Directory", &[
        ("id", &cypher_str(id)),
        ("path", &cypher_str(id)),
        ("name", &cypher_str(name)),
    ])
}

fn insert_file_node(store: &GraphStore, abs_path: &Path, rel_path: &str) -> Result<(), String> {
    let name = abs_path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let ext = abs_path.extension()
        .map(|e| e.to_string_lossy().to_string())
        .unwrap_or_default();
    let size = std::fs::metadata(abs_path)
        .map(|m| m.len())
        .unwrap_or(0);
    store.insert_node("File", &[
        ("id", &cypher_str(rel_path)),
        ("path", &cypher_str(rel_path)),
        ("name", &cypher_str(&name)),
        ("extension", &cypher_str(&ext)),
        ("size_bytes", &size.to_string()),
    ])
}

// ---------------------------------------------------------------------------
// Structural edges: Contains
// ---------------------------------------------------------------------------

fn insert_dir_file_edge(store: &GraphStore, rel_path: &Path) -> Result<(), String> {
    let file_id = rel_path.to_string_lossy();
    let parent_id = rel_path.parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    if parent_id.is_empty() {
        return Ok(()); // file is at root level, no parent directory node
    }
    store.insert_edge("Contains_Dir_File", &parent_id, &file_id, &[])
}

fn insert_dir_dir_edge(store: &GraphStore, parent_id: &str, child_id: &str) -> Result<(), String> {
    store.insert_edge("Contains_Dir_Dir", parent_id, child_id, &[])
}

// ---------------------------------------------------------------------------
// Single-file indexing: parse → insert nodes → insert edges
// ---------------------------------------------------------------------------

fn index_single_file(store: &GraphStore, abs_path: &Path, rel_path: &str) -> Result<(), String> {
    let source = std::fs::read_to_string(abs_path)
        .map_err(|e| format!("read {}: {e}", abs_path.display()))?;
    let ext = abs_path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let lang = Language::from_extension(ext)
        .ok_or_else(|| format!("unsupported file extension: {ext}"))?;
    let parsed = parser::parse_file(&source, rel_path, lang)?;
    insert_parsed_nodes(store, &parsed.nodes)?;
    insert_parsed_edges(store, &parsed.refs)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Node insertion from parsed results
// ---------------------------------------------------------------------------

fn insert_parsed_nodes(
    store: &GraphStore,
    nodes: &[parser::ExtractedNode],
) -> Result<(), String> {
    for node in nodes {
        let props = build_node_properties(node);
        let prop_refs: Vec<(&str, &str)> = props.iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        store.insert_node(&node.label, &prop_refs)?;
    }
    Ok(())
}

/// Builds the full property list for a node, mapping ExtractedNode fields
/// to the schema columns defined in graph_store.rs node_table_ddl().
fn build_node_properties(node: &parser::ExtractedNode) -> Vec<(String, String)> {
    let mut props = vec![("id".to_string(), cypher_str(&node.qualified_name))];
    if has_name_col(&node.label) {
        props.push(("name".to_string(), cypher_str(&node.name)));
    }
    if has_qualified_name_col(&node.label) {
        props.push(("qualified_name".to_string(), cypher_str(&node.qualified_name)));
    }
    if has_line_cols(&node.label) {
        props.push(("start_line".to_string(), node.start_line.to_string()));
        props.push(("end_line".to_string(), node.end_line.to_string()));
    }
    if has_visibility_col(&node.label) {
        props.push(("visibility".to_string(), cypher_str(&node.visibility)));
    }
    append_label_properties(&mut props, node);
    props
}

/// Maps parser extra properties to schema columns by label.
fn append_label_properties(props: &mut Vec<(String, String)>, node: &parser::ExtractedNode) {
    let find = |key: &str| -> String {
        node.properties.iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    };
    match node.label.as_str() {
        "Function" => {
            props.push(("is_async".to_string(), find("is_async")));
        }
        "Method" => {
            props.push(("is_async".to_string(), find("is_async")));
            props.push(("receiver_type".to_string(), cypher_str(&find("receiver_type"))));
        }
        "Field" => {
            props.push(("type_annotation".to_string(), cypher_str(&find("type_annotation"))));
        }
        "Constant" => {
            props.push(("type_annotation".to_string(), cypher_str(&find("type_annotation"))));
        }
        "TypeAlias" => {
            props.push(("target_type".to_string(), cypher_str(&find("target_type"))));
        }
        "Import" => {
            props.push(("path".to_string(), cypher_str(&find("path"))));
            props.push(("alias".to_string(), cypher_str(&find("alias"))));
            props.push(("is_glob".to_string(), find("is_glob")));
        }
        "CallSite" => {
            props.push(("callee_name".to_string(), cypher_str(&find("callee_name"))));
            props.push(("line".to_string(), node.start_line.to_string()));
            props.push(("col".to_string(), "0".to_string()));
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Edge insertion from parsed results
// ---------------------------------------------------------------------------

fn insert_parsed_edges(
    store: &GraphStore,
    refs: &[parser::ExtractedRef],
) -> Result<(), String> {
    for edge_ref in refs {
        let table = resolve_edge_table(
            &edge_ref.kind,
            &edge_ref.from_qualified_name,
            &edge_ref.to_qualified_name,
            store,
        );
        let table_name = match table {
            Some(t) => t,
            None => continue, // skip edges we can't map to a table
        };
        store.insert_edge(
            &table_name,
            &edge_ref.from_qualified_name,
            &edge_ref.to_qualified_name,
            &[],
        )?;
    }
    Ok(())
}

/// Resolves the multi-table edge name by probing both endpoints in the DB.
fn resolve_edge_table(
    kind: &str,
    from_qn: &str,
    to_qn: &str,
    store: &GraphStore,
) -> Option<String> {
    match kind {
        "Defines" => resolve_defines_table(from_qn, to_qn, store),
        "HasMethod" => resolve_has_method_table(from_qn, store),
        "HasField" => resolve_has_field_table(from_qn, store),
        "HasVariant" => Some("HasVariant_Enum_Variant".to_string()),
        // 3b: Extends refs are deferred to the resolver pass — skip here.
        "Extends" => None,
        _ => None,
    }
}

/// Probes both from (File|Module) and to (symbol label) to find the table.
fn resolve_defines_table(from_qn: &str, to_qn: &str, store: &GraphStore) -> Option<String> {
    let from_label = probe_node_label(from_qn, store, &["File", "Module"])?;
    let to_candidates = &[
        "Function", "Struct", "Enum", "Trait", "Constant",
        "TypeAlias", "Module", "Import",
    ];
    let to_label = probe_node_label(to_qn, store, to_candidates)?;
    let table = format!("Defines_{from_label}_{to_label}");
    if is_valid_rel_table(&table) { Some(table) } else { None }
}

fn resolve_has_method_table(from_qn: &str, store: &GraphStore) -> Option<String> {
    let from_label = probe_node_label(from_qn, store, &["Struct", "Enum", "Trait"])?;
    Some(format!("HasMethod_{from_label}_Method"))
}

fn resolve_has_field_table(from_qn: &str, store: &GraphStore) -> Option<String> {
    let from_label = probe_node_label(from_qn, store, &["Struct", "Enum"])?;
    Some(format!("HasField_{from_label}_Field"))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Checks which label a node with the given id belongs to.
fn probe_node_label(id: &str, store: &GraphStore, candidates: &[&str]) -> Option<String> {
    let escaped = id.replace('\'', "\\'");
    for label in candidates {
        let cypher = format!("MATCH (n:{label}) WHERE n.id = '{escaped}' RETURN n.id");
        if let Ok(qr) = store.execute_query(&cypher) {
            if !qr.rows.is_empty() {
                return Some(label.to_string());
            }
        }
    }
    None
}

/// Checks if a rel table name exists in the known schema.
fn is_valid_rel_table(name: &str) -> bool {
    // Must match one of the REL_TABLES entries in graph_store.
    // We check by name convention rather than importing the private const.
    const KNOWN: &[&str] = &[
        // 3a tables
        "Contains_Dir_File", "Contains_Dir_Dir", "Contains_File_Module",
        "Defines_File_Function", "Defines_File_Struct", "Defines_File_Enum",
        "Defines_File_Trait", "Defines_File_Constant", "Defines_File_TypeAlias",
        "Defines_Module_Function", "Defines_Module_Struct", "Defines_Module_Enum",
        "Defines_Module_Trait", "Defines_Module_Constant", "Defines_Module_TypeAlias",
        "HasMethod_Struct_Method", "HasMethod_Enum_Method", "HasMethod_Trait_Method",
        "HasField_Struct_Field", "HasField_Enum_Field",
        "HasVariant_Enum_Variant",
        // 3b Imports tables — source: stages/stage-3b.md §3
        "Imports_File_File", "Imports_File_Module", "Imports_File_Function",
        "Imports_File_Struct", "Imports_File_Enum", "Imports_File_Trait",
        "Imports_File_Constant", "Imports_File_TypeAlias",
        "Imports_Module_Function", "Imports_Module_Struct", "Imports_Module_Enum",
        "Imports_Module_Trait", "Imports_Module_Constant", "Imports_Module_TypeAlias",
        // 3b Calls tables
        "Calls_Function_Function", "Calls_Function_Method",
        "Calls_Method_Function", "Calls_Method_Method",
        // 3b Implements tables
        "Implements_Struct_Trait", "Implements_Enum_Trait",
        // 3b Extends table
        "Extends_Trait_Trait",
        // 3b Uses tables
        "Uses_Function_Struct", "Uses_Function_Enum", "Uses_Function_Trait",
        "Uses_Function_TypeAlias", "Uses_Method_Struct", "Uses_Method_Enum",
        "Uses_Method_Trait", "Uses_Method_TypeAlias", "Uses_Struct_Struct",
        "Uses_Struct_Enum", "Uses_Struct_Trait", "Uses_Field_Struct",
        "Uses_Field_Enum", "Uses_Field_Trait",
    ];
    KNOWN.contains(&name)
}

fn relative_path(root: &Path, file: &Path) -> PathBuf {
    file.strip_prefix(root).unwrap_or(file).to_path_buf()
}

/// Wraps a value in single-quotes for Cypher string literal.
fn cypher_str(s: &str) -> String {
    format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
}

// Schema awareness — source: graph_store.rs node_table_ddl().
// Each function returns true iff the label's CREATE NODE TABLE includes that column.

fn has_name_col(label: &str) -> bool {
    // All node tables have `name` EXCEPT Import (path/alias only).
    // CallSite stores callee_name via properties, not via 'name' column.
    !matches!(label, "Import" | "CallSite")
}

fn has_qualified_name_col(label: &str) -> bool {
    matches!(label,
        "Module" | "Function" | "Method" | "Struct" | "Enum" | "Variant" |
        "Trait" | "Constant" | "TypeAlias"
    )
}

fn has_line_cols(label: &str) -> bool {
    matches!(label, "Function" | "Method" | "Struct" | "Enum" | "Trait")
}

fn has_visibility_col(label: &str) -> bool {
    matches!(label,
        "Function" | "Method" | "Struct" | "Enum" | "Trait" | "Field"
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_index_own_project() {
        let tmp = std::env::temp_dir()
            .join(format!("indexer_test_graph_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        // Ensure the directory is fully gone before creating a fresh DB.
        assert!(!tmp.exists(), "failed to clean temp dir: {}", tmp.display());

        let result = index_codebase(
            Path::new("src"),
            &tmp,
        ).unwrap();

        assert!(
            result.files_indexed >= 3,
            "should index at least main.rs + graph_store.rs + rust_parser.rs, got {}",
            result.files_indexed
        );
        assert!(
            result.node_count > 50,
            "should have many nodes, got {}",
            result.node_count
        );
        assert!(
            result.edge_count > 30,
            "should have many edges, got {}",
            result.edge_count
        );

        // Verify a known function exists via Cypher
        let store = GraphStore::open_or_create(&tmp).unwrap();
        let qr = store.execute_query(
            "MATCH (f:Function) WHERE f.name = 'main' RETURN f.name"
        ).unwrap();
        assert!(
            !qr.rows.is_empty(),
            "should find main() in graph"
        );
        assert_eq!(qr.rows[0][0], "main");

        // Verify file nodes exist
        let qr2 = store.execute_query(
            "MATCH (f:File) RETURN count(f) AS cnt"
        ).unwrap();
        assert!(
            !qr2.rows.is_empty() && qr2.rows[0][0].parse::<u64>().unwrap_or(0) > 0,
            "should have File nodes"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
