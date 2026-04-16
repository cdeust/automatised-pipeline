// lsp_resolver — LSP-enhanced resolution pass for unresolved call sites.
//
// Queries a Language Server Protocol server for textDocument/definition
// to resolve method calls on inferred types that the static 3b resolver
// cannot handle. Runs AFTER resolve_graph as an optional enhancement.
//
// source: stages/stage-3b.md §7 — "method calls on inferred types" deferred to LSP

use crate::graph_store::GraphStore;
use crate::lsp_client::{self, LspClient, LspResolutionResult};
use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Run LSP-enhanced resolution on unresolved call sites in the graph.
///
/// 1. Collects CallSite nodes that lack a Calls edge (unresolved).
/// 2. Starts the appropriate LSP server.
/// 3. For each unresolved site, queries textDocument/definition.
/// 4. Maps definition locations back to graph nodes, adds Calls edges.
pub fn resolve_with_lsp(
    store: &GraphStore,
    codebase_path: &Path,
    language: &str,
    lsp_command_override: Option<&str>,
    timeout: Duration,
) -> Result<LspResolutionResult, String> {
    let start = Instant::now();

    // Determine LSP command.
    // source: C3 fix — caller-provided `lsp_command_override` must be validated
    // against the allowlist BEFORE `Command::new` to prevent arbitrary binary
    // execution. `LspClient::start` also validates as defense-in-depth.
    let (cmd, default_args) = match lsp_command_override {
        Some(c) => {
            lsp_client::validate_lsp_command(c)?;
            (c, &[] as &[&str])
        }
        None => {
            let detected = lsp_client::detect_lsp_command(language)
                .ok_or(format!("no LSP server known for language '{language}'"))?;
            (detected.0, detected.1)
        }
    };

    if !lsp_client::is_command_available(cmd) {
        return Err(format!("lsp_not_found: {cmd} not found in PATH"));
    }

    // Collect unresolved call sites
    let unresolved = collect_unresolved_callsites(store)?;
    if unresolved.is_empty() {
        return Ok(LspResolutionResult {
            resolved_count: 0,
            failed_count: 0,
            skipped_count: 0,
            elapsed_ms: start.elapsed().as_millis() as u64,
        });
    }

    // Start LSP server
    let mut client = LspClient::start(cmd, default_args, codebase_path, timeout)?;
    client.initialize(codebase_path)?;

    // Group call sites by file for efficient didOpen batching
    let by_file = group_by_file(&unresolved);

    // Build a position index of graph nodes for mapping definition results
    let node_index = build_node_position_index(store)?;

    let mut resolved_count = 0u64;
    let mut failed_count = 0u64;
    let mut skipped_count = 0u64;
    let per_request_timeout = Duration::from_secs(5);

    for (file_path, sites) in &by_file {
        let abs_path = codebase_path.join(file_path);
        if !abs_path.exists() {
            skipped_count += sites.len() as u64;
            continue;
        }
        let file_uri = lsp_client::path_to_file_uri(&abs_path);
        let lang_id = language_id_for(language);

        // Read file content for didOpen
        let content = match std::fs::read_to_string(&abs_path) {
            Ok(c) => c,
            Err(_) => {
                skipped_count += sites.len() as u64;
                continue;
            }
        };

        if client.did_open(&file_uri, lang_id, &content).is_err() {
            skipped_count += sites.len() as u64;
            continue;
        }

        for site in sites {
            let result = client.get_definition(&file_uri, site.line, site.col);
            match result {
                Ok(Some(def)) => {
                    if try_add_lsp_edge(store, site, &def, &node_index) {
                        resolved_count += 1;
                    } else {
                        failed_count += 1;
                    }
                }
                Ok(None) => failed_count += 1,
                Err(e) => {
                    if e.contains("timeout") {
                        // Skip remaining sites in this file on timeout
                        skipped_count += 1;
                    } else {
                        failed_count += 1;
                    }
                }
            }

            // Respect per-request timeout budget
            if start.elapsed() > timeout.saturating_sub(per_request_timeout) {
                skipped_count += (sites.len() as u64).saturating_sub(resolved_count + failed_count);
                break;
            }
        }
    }

    let _ = client.shutdown();

    Ok(LspResolutionResult {
        resolved_count,
        failed_count,
        skipped_count,
        elapsed_ms: start.elapsed().as_millis() as u64,
    })
}

// ---------------------------------------------------------------------------
// Unresolved call site collection
// ---------------------------------------------------------------------------

struct UnresolvedCallSite {
    caller_qn: String,
    caller_label: String,
    #[allow(dead_code)] // retained for diagnostics and future logging
    callee_name: String,
    file_path: String,
    line: u64,
    col: u64,
}

fn collect_unresolved_callsites(store: &GraphStore) -> Result<Vec<UnresolvedCallSite>, String> {
    // Get all CallSite nodes
    let qr = store.execute_query(
        "MATCH (cs:CallSite) RETURN cs.id, cs.callee_name, cs.line, cs.col"
    )?;

    let mut sites = Vec::new();
    for row in &qr.rows {
        if row.len() < 4 { continue; }
        let cs_id = &row[0];
        let callee = &row[1];
        let line: u64 = row[2].parse().unwrap_or(0);
        let col: u64 = row[3].parse().unwrap_or(0);

        // Check if this callsite already has a Calls edge
        if has_calls_edge(store, cs_id) {
            continue;
        }

        let (file_path, caller_qn) = parse_callsite_id(cs_id);
        let caller_label = determine_caller_label(store, &caller_qn);

        sites.push(UnresolvedCallSite {
            caller_qn,
            caller_label,
            callee_name: callee.clone(),
            file_path,
            line,
            col,
        });
    }
    Ok(sites)
}

fn has_calls_edge(store: &GraphStore, callsite_id: &str) -> bool {
    let caller_qn = extract_caller_from_callsite_id(callsite_id);
    // Check all Calls edge types
    for prefix in &["Calls_Function_Function", "Calls_Function_Method",
                     "Calls_Method_Function", "Calls_Method_Method"] {
        let parts: Vec<&str> = prefix.splitn(3, '_').collect();
        if parts.len() < 3 { continue; }
        let from_label = parts[1];
        let to_label = parts[2];
        let esc = caller_qn.replace('\'', "\\'");
        let cypher = format!(
            "MATCH (a:{from_label})-[r:{prefix}]->(b:{to_label}) \
             WHERE a.id = '{esc}' RETURN count(r)"
        );
        if let Ok(qr) = store.execute_query(&cypher) {
            if !qr.rows.is_empty() {
                let count: u64 = qr.rows[0][0].parse().unwrap_or(0);
                if count > 0 { return true; }
            }
        }
    }
    false
}

fn extract_caller_from_callsite_id(cs_id: &str) -> String {
    if let Some(idx) = cs_id.rfind("::call@") {
        cs_id[..idx].to_string()
    } else {
        cs_id.to_string()
    }
}

fn parse_callsite_id(cs_id: &str) -> (String, String) {
    let caller_qn = extract_caller_from_callsite_id(cs_id);
    let file_path = extract_file_from_qn(&caller_qn);
    (file_path, caller_qn)
}

fn extract_file_from_qn(qn: &str) -> String {
    for ext in &[".rs::", ".py::", ".ts::", ".tsx::"] {
        if let Some(idx) = qn.find(ext) {
            return qn[..idx + ext.len() - 2].to_string();
        }
    }
    qn.to_string()
}

fn determine_caller_label(store: &GraphStore, caller_qn: &str) -> String {
    let esc = caller_qn.replace('\'', "\\'");
    for label in &["Function", "Method"] {
        let cypher = format!(
            "MATCH (n:{label}) WHERE n.qualified_name = '{esc}' RETURN n.id"
        );
        if let Ok(qr) = store.execute_query(&cypher) {
            if !qr.rows.is_empty() {
                return label.to_string();
            }
        }
    }
    "Function".to_string()
}

fn group_by_file(sites: &[UnresolvedCallSite]) -> HashMap<String, Vec<&UnresolvedCallSite>> {
    let mut map: HashMap<String, Vec<&UnresolvedCallSite>> = HashMap::new();
    for site in sites {
        map.entry(site.file_path.clone()).or_default().push(site);
    }
    map
}

fn language_id_for(language: &str) -> &str {
    match language {
        "rust" => "rust",
        "python" => "python",
        "typescript" => "typescript",
        _ => "plaintext",
    }
}

// ---------------------------------------------------------------------------
// Node position index — maps (file, line) to (node_id, label)
// ---------------------------------------------------------------------------

struct NodePosition {
    id: String,
    label: String,
}

fn build_node_position_index(
    store: &GraphStore,
) -> Result<HashMap<(String, u64), NodePosition>, String> {
    let mut index = HashMap::new();
    for label in &["Function", "Method", "Struct", "Enum", "Trait"] {
        let qr = store.execute_query(&format!(
            "MATCH (n:{label}) RETURN n.id, n.qualified_name, n.start_line"
        ))?;
        for row in &qr.rows {
            if row.len() < 3 { continue; }
            let file = extract_file_from_qn(&row[1]);
            let line: u64 = row[2].parse().unwrap_or(0);
            index.insert((file, line), NodePosition {
                id: row[0].clone(),
                label: label.to_string(),
            });
        }
    }
    Ok(index)
}

// ---------------------------------------------------------------------------
// Edge insertion from LSP result
// ---------------------------------------------------------------------------

fn try_add_lsp_edge(
    store: &GraphStore,
    site: &UnresolvedCallSite,
    def: &lsp_client::DefinitionResult,
    node_index: &HashMap<(String, u64), NodePosition>,
) -> bool {
    // Convert LSP URI to relative file path
    let file_path = match uri_to_relative_path(&def.uri) {
        Some(p) => p,
        None => return false,
    };

    // Look up the definition in our node index.
    // LSP line is 0-based, our graph stores 1-based line numbers.
    let target_line = def.start_line + 1;

    // Try exact line match first, then scan nearby lines (+-2)
    let target = find_node_at_position(node_index, &file_path, target_line);
    let target = match target {
        Some(t) => t,
        None => return false,
    };

    // Build the edge type
    let rel_type = format!("Calls_{}_{}", site.caller_label, target.label);

    // Check the rel type is valid
    let valid_rels = [
        "Calls_Function_Function", "Calls_Function_Method",
        "Calls_Method_Function", "Calls_Method_Method",
    ];
    if !valid_rels.contains(&rel_type.as_str()) {
        return false;
    }

    // Insert edge with LSP-backed confidence (0.9)
    store.insert_edge(
        &rel_type,
        &site.caller_qn,
        &target.id,
        &[
            ("confidence", "0.9"),
            ("resolution_method", "'lsp-definition'"),
        ],
    ).is_ok()
}

fn uri_to_relative_path(uri: &str) -> Option<String> {
    let path = uri.strip_prefix("file://")?;
    // Return as-is; the caller handles prefix matching
    Some(path.to_string())
}

fn find_node_at_position<'a>(
    index: &'a HashMap<(String, u64), NodePosition>,
    file_path: &str,
    line: u64,
) -> Option<&'a NodePosition> {
    // Exact match
    if let Some(node) = index.get(&(file_path.to_string(), line)) {
        return Some(node);
    }
    // Try nearby lines (definition may start a few lines before the name)
    for offset in 1..=3 {
        if line >= offset {
            if let Some(node) = index.get(&(file_path.to_string(), line - offset)) {
                return Some(node);
            }
        }
        if let Some(node) = index.get(&(file_path.to_string(), line + offset)) {
            return Some(node);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_uri_to_relative_path() {
        assert_eq!(
            uri_to_relative_path("file:///Users/foo/project/src/main.rs"),
            Some("/Users/foo/project/src/main.rs".to_string())
        );
        assert_eq!(uri_to_relative_path("https://example.com"), None);
    }

    #[test]
    fn test_extract_caller_from_callsite_id() {
        assert_eq!(
            extract_caller_from_callsite_id("src/main.rs::main::call@5:4"),
            "src/main.rs::main"
        );
        assert_eq!(
            extract_caller_from_callsite_id("src/foo.rs::bar"),
            "src/foo.rs::bar"
        );
    }

    #[test]
    fn test_find_node_at_position_exact() {
        let mut index = HashMap::new();
        index.insert(("src/main.rs".to_string(), 10), NodePosition {
            id: "fn1".to_string(),
            label: "Function".to_string(),
        });
        let result = find_node_at_position(&index, "src/main.rs", 10);
        assert!(result.is_some());
        assert_eq!(result.unwrap().id, "fn1");
    }

    #[test]
    fn test_find_node_at_position_nearby() {
        let mut index = HashMap::new();
        index.insert(("src/main.rs".to_string(), 8), NodePosition {
            id: "fn2".to_string(),
            label: "Method".to_string(),
        });
        let result = find_node_at_position(&index, "src/main.rs", 10);
        assert!(result.is_some());
        assert_eq!(result.unwrap().id, "fn2");
    }

    #[test]
    fn test_find_node_at_position_not_found() {
        let index = HashMap::new();
        assert!(find_node_at_position(&index, "src/main.rs", 10).is_none());
    }

    #[test]
    fn test_language_id_for() {
        assert_eq!(language_id_for("rust"), "rust");
        assert_eq!(language_id_for("python"), "python");
        assert_eq!(language_id_for("unknown"), "plaintext");
    }
}
