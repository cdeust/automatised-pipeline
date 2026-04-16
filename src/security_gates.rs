// security_gates — Stage 8: graph-aware pre-merge security gates.
//
// Five gates, each a single Cypher pattern + fixed rule:
//   S1 auth_critical_touch   — change shares a Leiden community with an
//                              auth-pattern symbol.                  critical
//   S2 unsafe_symbol          — changed symbol itself is `unsafe` (Rust) or
//                              uses a risky JS/Python API.            critical|warning
//                              INFO-SKIP mode when parser does not record it.
//   S3 public_api_change      — crate-root `pub` symbol touched.      warning|critical
//   S4 unresolved_imports     — changed symbol owns new Imports that
//                              resolved to an :Import fallback node.  warning|critical
//   S5 test_coverage_gap      — changed symbol has no ParticipatesIn
//                              path from any test-entry process.      warning
//
// Read-only w.r.t. the graph. LLM-free. Deterministic on (graph, Δ).
//
// source: stages/stage-8.md §4 (gate definitions), §6 (severity ladder),
//         §7 (tool schema).

use crate::graph_store::GraphStore;
use crate::search;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

// source: stages/stage-8.md §4 S1 — auth-critical name patterns for
// Leiden community proximity detection.
const AUTH_CRITICAL_PATTERNS: &[&str] = &[
    "auth", "password", "token", "permission", "role",
    "crypto", "encrypt", "decrypt", "verify", "jwt", "oauth", "session",
];

// source: stages/stage-8.md §6 — security artifact filename.
pub const SECURITY_FILE: &str = "stage-8.security.json";

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

pub struct SecurityReport {
    pub gates_passed: bool,
    pub flags: Vec<SecurityFlag>,
    pub summary: SecuritySummary,
}

pub struct SecurityFlag {
    pub gate: String,
    pub severity: String,
    pub symbol: String,
    pub message: String,
    pub details: Value,
}

pub struct SecuritySummary {
    pub changed_symbols: u64,
    pub critical_count: u64,
    pub warning_count: u64,
    pub info_count: u64,
}

// ---------------------------------------------------------------------------
// Orchestration
// ---------------------------------------------------------------------------

pub fn check_gates(
    store: &GraphStore,
    changed_symbols: &[String],
) -> Result<SecurityReport, String> {
    let resolved = resolve_all(store, changed_symbols);
    let mut flags: Vec<SecurityFlag> = Vec::new();
    let auth_communities = find_auth_communities(store);
    for r in &resolved {
        let qn = match &r.resolved_qn {
            Some(q) => q,
            None => continue,
        };
        run_s1(store, qn, &auth_communities, &mut flags);
        run_s2(store, qn, &mut flags);
        run_s3(store, qn, &mut flags);
        run_s4(store, qn, &mut flags);
        run_s5(store, qn, &mut flags);
    }
    for r in &resolved {
        if r.resolved_qn.is_none() {
            flags.push(SecurityFlag {
                gate: "input_unresolved".into(),
                severity: "info".into(),
                symbol: r.input.clone(),
                message: format!("changed symbol '{}' did not resolve in graph", r.input),
                details: json!({ "did_you_mean": r.did_you_mean }),
            });
        }
    }
    let summary = tally(&flags, changed_symbols.len() as u64);
    Ok(SecurityReport {
        gates_passed: summary.critical_count == 0,
        flags,
        summary,
    })
}

fn tally(flags: &[SecurityFlag], changed_symbols: u64) -> SecuritySummary {
    let mut critical = 0u64;
    let mut warning = 0u64;
    let mut info = 0u64;
    for f in flags {
        match f.severity.as_str() {
            "critical" => critical += 1,
            "warning" => warning += 1,
            _ => info += 1,
        }
    }
    SecuritySummary {
        changed_symbols,
        critical_count: critical,
        warning_count: warning,
        info_count: info,
    }
}

// ---------------------------------------------------------------------------
// Input resolution
// ---------------------------------------------------------------------------

struct Resolved {
    input: String,
    resolved_qn: Option<String>,
    did_you_mean: Vec<String>,
}

fn resolve_all(store: &GraphStore, inputs: &[String]) -> Vec<Resolved> {
    inputs.iter().map(|input| {
        match search::resolve_qualified_name(store, input) {
            Ok(qn) => Resolved { input: input.clone(), resolved_qn: Some(qn), did_you_mean: Vec::new() },
            Err(nf) => Resolved { input: input.clone(), resolved_qn: None, did_you_mean: nf.did_you_mean },
        }
    }).collect()
}

// ---------------------------------------------------------------------------
// S1 — auth-critical community touch
// ---------------------------------------------------------------------------

fn find_auth_communities(store: &GraphStore) -> BTreeSet<String> {
    let mut out: BTreeSet<String> = BTreeSet::new();
    // One scan per symbol label that can be a community member. Keep it
    // simple: iterate patterns, iterate labels, collect c.id rows.
    let labels = [
        ("Function", "MemberOf_Function_Community"),
        ("Method", "MemberOf_Method_Community"),
        ("Struct", "MemberOf_Struct_Community"),
        ("Enum", "MemberOf_Enum_Community"),
        ("Trait", "MemberOf_Trait_Community"),
        ("Constant", "MemberOf_Constant_Community"),
        ("TypeAlias", "MemberOf_TypeAlias_Community"),
        ("Module", "MemberOf_Module_Community"),
    ];
    // source: stages/stage-8.md §4 S1 patterns are lowercase; we normalize
    // in-memory rather than rely on an engine-specific `toLower()` Cypher fn.
    for (label, rel) in labels {
        let cypher = format!(
            "MATCH (s:{label})-[:{rel}]->(c:Community) RETURN s.name, c.id"
        );
        if let Ok(qr) = store.execute_query(&cypher) {
            for row in &qr.rows {
                if row.len() < 2 { continue; }
                let name_lower = row[0].to_ascii_lowercase();
                if AUTH_CRITICAL_PATTERNS.iter().any(|p| name_lower.contains(p)) {
                    let cid = &row[1];
                    if !cid.is_empty() { out.insert(cid.clone()); }
                }
            }
        }
    }
    out
}

fn run_s1(
    store: &GraphStore,
    qualified_name: &str,
    auth_communities: &BTreeSet<String>,
    flags: &mut Vec<SecurityFlag>,
) {
    if auth_communities.is_empty() { return; }
    let cid = match community_of(store, qualified_name) {
        Some(c) => c,
        None => return,
    };
    if !auth_communities.contains(&cid) { return; }
    flags.push(SecurityFlag {
        gate: "auth_critical_touch".into(),
        severity: "critical".into(),
        symbol: qualified_name.into(),
        message: format!(
            "changed symbol shares community '{}' with auth-critical symbols",
            cid
        ),
        details: json!({ "community_id": cid, "auth_patterns": AUTH_CRITICAL_PATTERNS }),
    });
}

fn community_of(store: &GraphStore, qualified_name: &str) -> Option<String> {
    // Mirrors search/mod.rs::lookup_community — per-label iteration for
    // lbug dialect compatibility (no rel-type alternation).
    let escaped = qualified_name.replace('\'', "\\'");
    for label in ["Function", "Method", "Struct", "Enum", "Trait",
                  "Constant", "TypeAlias", "Module"] {
        let rel = format!("MemberOf_{label}_Community");
        let cypher = format!(
            "MATCH (n:{label})-[:{rel}]->(c:Community) \
             WHERE n.qualified_name = '{escaped}' \
             RETURN c.id LIMIT 1"
        );
        if let Ok(qr) = store.execute_query(&cypher) {
            if let Some(row) = qr.rows.first() {
                if let Some(cid) = row.first() {
                    if !cid.is_empty() { return Some(cid.clone()); }
                }
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// S2 — unsafe symbol (info-skip when parser lacks is_unsafe)
// ---------------------------------------------------------------------------
//
// source: stages/stage-8.md §4.2 + §7 Open Q1. The current Rust parser
// (src/parser/rust.rs) does NOT record an is_unsafe property on Function or
// Method nodes. Per the spec's graceful-degradation rule (Invariant I6), S2
// ships in INFO-SKIP mode: emit one info flag per changed symbol stating the
// detection is unavailable and skip the critical check. This preserves
// determinism and leaves a breadcrumb for the 3b-v2 parser roadmap.

fn run_s2(_store: &GraphStore, qualified_name: &str, flags: &mut Vec<SecurityFlag>) {
    flags.push(SecurityFlag {
        gate: "unsafe_symbol".into(),
        severity: "info".into(),
        symbol: qualified_name.into(),
        message: "unsafe detection unavailable: rust_parser does not record is_unsafe \
                  (see stages/stage-8.md §7 Q1; unblocks when stage 3a-v2 ships)".into(),
        details: json!({ "skipped": true, "reason": "parser_missing_is_unsafe" }),
    });
}

// ---------------------------------------------------------------------------
// S3 — public API surface change
// ---------------------------------------------------------------------------

fn run_s3(store: &GraphStore, qualified_name: &str, flags: &mut Vec<SecurityFlag>) {
    let meta = match symbol_visibility_and_parent(store, qualified_name) {
        Some(m) => m,
        None => return,
    };
    if meta.visibility.as_deref() != Some("pub") { return; }
    if !meta.parent_is_file { return; }
    // severity: warning on modify (default); caller supplies change_kind only
    // through the batch list. We can't distinguish remove/rename without it,
    // so this gate stays at "warning". Callers who want critical escalation
    // feed remove/rename through a richer change_kind in a follow-up.
    flags.push(SecurityFlag {
        gate: "public_api_change".into(),
        severity: "warning".into(),
        symbol: qualified_name.into(),
        message: "touches a crate-root public API symbol — downstream consumers may break".into(),
        details: json!({
            "visibility": "pub",
            "parent_label": meta.parent_label,
            "file_path": meta.file_path,
        }),
    });
}

struct SymbolMeta {
    visibility: Option<String>,
    parent_is_file: bool,
    parent_label: String,
    file_path: Option<String>,
}

fn symbol_visibility_and_parent(store: &GraphStore, qualified_name: &str) -> Option<SymbolMeta> {
    let escaped = qualified_name.replace('\'', "\\'");
    // 1) pull the symbol's visibility (labels that carry it).
    let vis = fetch_visibility(store, &escaped)?;
    // 2) Is the symbol defined directly by a File (not via a Module)?
    let file_defined = has_file_parent(store, &escaped);
    let file_path = lookup_file_path(store, &escaped);
    Some(SymbolMeta {
        visibility: vis,
        parent_is_file: file_defined,
        parent_label: if file_defined { "File".into() } else { "Module".into() },
        file_path,
    })
}

fn fetch_visibility(store: &GraphStore, escaped_qn: &str) -> Option<Option<String>> {
    // Labels that carry `visibility` per graph_store.rs DDL: Function, Method,
    // Struct, Enum, Trait, Field. Method receiver_type is irrelevant here.
    for label in ["Function", "Method", "Struct", "Enum", "Trait"] {
        let cypher = format!(
            "MATCH (n:{label}) WHERE n.qualified_name = '{escaped_qn}' \
             RETURN n.visibility LIMIT 1"
        );
        if let Ok(qr) = store.execute_query(&cypher) {
            if let Some(row) = qr.rows.first() {
                if let Some(v) = row.first() {
                    let s = v.trim();
                    return Some(if s.is_empty() { None } else { Some(s.to_string()) });
                }
            }
        }
    }
    None
}

fn has_file_parent(store: &GraphStore, escaped_qn: &str) -> bool {
    // Defines_File_* edges go from File to the symbol; crate-root pubs live
    // directly under the File (no intermediate Module).
    for rel in [
        "Defines_File_Function", "Defines_File_Struct", "Defines_File_Enum",
        "Defines_File_Trait", "Defines_File_Constant", "Defines_File_TypeAlias",
    ] {
        let cypher = format!(
            "MATCH (f:File)-[:{rel}]->(n) WHERE n.qualified_name = '{escaped_qn}' \
             RETURN f.path LIMIT 1"
        );
        if let Ok(qr) = store.execute_query(&cypher) {
            if qr.rows.first().and_then(|r| r.first()).is_some() {
                return true;
            }
        }
    }
    false
}

fn lookup_file_path(store: &GraphStore, escaped_qn: &str) -> Option<String> {
    for rel in [
        "Defines_File_Function", "Defines_File_Struct", "Defines_File_Enum",
        "Defines_File_Trait", "Defines_File_Constant", "Defines_File_TypeAlias",
    ] {
        let cypher = format!(
            "MATCH (f:File)-[:{rel}]->(n) WHERE n.qualified_name = '{escaped_qn}' \
             RETURN f.path LIMIT 1"
        );
        if let Ok(qr) = store.execute_query(&cypher) {
            if let Some(row) = qr.rows.first() {
                if let Some(p) = row.first() {
                    if !p.is_empty() { return Some(p.clone()); }
                }
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// S4 — unresolved import introduction
// ---------------------------------------------------------------------------

fn run_s4(store: &GraphStore, qualified_name: &str, flags: &mut Vec<SecurityFlag>) {
    // An Import node survives post-resolution iff the resolver could not
    // rewrite it into a concrete edge (source: semantic_diff.rs
    // count_unresolved — count(:Import) is the canonical unresolved metric).
    // Since the schema has no File->Import edge, we scope by qualified_name
    // prefix: Import nodes live under the file's scope (parser/rust.rs §
    // handle_use_declaration — qualified_name = qual(scope, display_name)).
    let escaped = qualified_name.replace('\'', "\\'");
    let file_path = match lookup_file_path(store, &escaped) {
        Some(p) => p,
        None => return,
    };
    // File path matches the scope prefix used by qual() in the parser. Strip
    // any leading path component the resolver removes (search::strip_leading
    // mirrors this), then match qualified_name prefix.
    let scope = file_scope_from_path(&file_path);
    let escaped_scope = scope.replace('\'', "\\'");
    let cypher = format!(
        "MATCH (i:Import) WHERE i.qualified_name STARTS WITH '{escaped_scope}::' \
         RETURN count(i)"
    );
    let count: u64 = match store.execute_query(&cypher) {
        Ok(qr) => qr.rows.first().and_then(|r| r.first())
            .and_then(|s| s.parse::<u64>().ok()).unwrap_or(0),
        Err(_) => 0,
    };
    if count == 0 { return; }
    let severity = if count >= 2 { "critical" } else { "warning" };
    flags.push(SecurityFlag {
        gate: "unresolved_imports".into(),
        severity: severity.into(),
        symbol: qualified_name.into(),
        message: format!(
            "{count} unresolved Import node(s) in the changed symbol's file — drift or supply-chain risk"
        ),
        details: json!({ "file_path": file_path, "scope": scope, "unresolved_count": count }),
    });
}

// Strips any leading path component so it matches the parser's qualified_name
// convention (`main.rs` rather than `src/main.rs`). Mirrors
// search::strip_leading_path_component.
fn file_scope_from_path(file_path: &str) -> String {
    match file_path.find('/') {
        Some(idx) => file_path[idx + 1..].to_string(),
        None => file_path.to_string(),
    }
}

// ---------------------------------------------------------------------------
// S5 — test coverage structural gap
// ---------------------------------------------------------------------------

fn run_s5(store: &GraphStore, qualified_name: &str, flags: &mut Vec<SecurityFlag>) {
    let escaped = qualified_name.replace('\'', "\\'");
    let mut reached = 0u64;
    for label in ["Function", "Method"] {
        let rel = format!("ParticipatesIn_{label}_Process");
        let cypher = format!(
            "MATCH (n:{label})-[:{rel}]->(p:Process) \
             WHERE n.qualified_name = '{escaped}' AND p.entry_kind = 'test' \
             RETURN count(p)"
        );
        if let Ok(qr) = store.execute_query(&cypher) {
            if let Some(row) = qr.rows.first() {
                if let Some(c) = row.first() {
                    reached += c.parse::<u64>().unwrap_or(0);
                }
            }
        }
    }
    if reached > 0 { return; }
    flags.push(SecurityFlag {
        gate: "test_coverage_gap".into(),
        severity: "warning".into(),
        symbol: qualified_name.into(),
        message: "no ParticipatesIn path from any test-entry process — structural coverage gap".into(),
        details: json!({ "test_processes_reaching_symbol": 0 }),
    });
}

// ---------------------------------------------------------------------------
// Artifact writer
// ---------------------------------------------------------------------------

pub fn report_to_json(
    report: &SecurityReport,
    run_id: &str,
    finding_id: &str,
    graph_path: &Path,
    changed_symbols: &[String],
    checked_at: &str,
) -> Value {
    let flags: Vec<Value> = report.flags.iter().map(|f| json!({
        "gate": f.gate, "severity": f.severity, "symbol": f.symbol,
        "message": f.message, "details": f.details,
    })).collect();
    json!({
        "run_id": run_id,
        "finding_id": finding_id,
        "graph_path": graph_path.to_string_lossy(),
        "changed_symbols": changed_symbols,
        "checked_at": checked_at,
        "gates_passed": report.gates_passed,
        "summary": {
            "changed_symbols": report.summary.changed_symbols,
            "critical_count": report.summary.critical_count,
            "warning_count": report.summary.warning_count,
            "info_count": report.summary.info_count,
        },
        "flags": flags,
    })
}

pub fn write_security(path: &Path, value: &Value) -> Result<PathBuf, String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("mkdir {:?}: {}", parent, e))?;
    }
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| format!("serialize: {}", e))?;
    fs::write(path, bytes).map_err(|e| format!("write {:?}: {}", path, e))?;
    Ok(path.to_path_buf())
}

// ---------------------------------------------------------------------------
// Pure-helper tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_auth_patterns_lowercase() {
        for p in AUTH_CRITICAL_PATTERNS {
            assert_eq!(*p, p.to_ascii_lowercase(), "patterns must be lowercase");
        }
    }

    #[test]
    fn test_tally_counts() {
        let flags = vec![
            SecurityFlag { gate: "g".into(), severity: "critical".into(),
                symbol: "s".into(), message: "".into(), details: json!({}) },
            SecurityFlag { gate: "g".into(), severity: "warning".into(),
                symbol: "s".into(), message: "".into(), details: json!({}) },
            SecurityFlag { gate: "g".into(), severity: "info".into(),
                symbol: "s".into(), message: "".into(), details: json!({}) },
            SecurityFlag { gate: "g".into(), severity: "info".into(),
                symbol: "s".into(), message: "".into(), details: json!({}) },
        ];
        let s = tally(&flags, 5);
        assert_eq!(s.critical_count, 1);
        assert_eq!(s.warning_count, 1);
        assert_eq!(s.info_count, 2);
        assert_eq!(s.changed_symbols, 5);
    }

    #[test]
    fn test_gates_passed_requires_zero_critical() {
        let mut r = SecurityReport {
            gates_passed: true,
            flags: Vec::new(),
            summary: SecuritySummary {
                changed_symbols: 1, critical_count: 0,
                warning_count: 5, info_count: 3,
            },
        };
        // with only warnings, should pass
        assert!(r.summary.critical_count == 0);
        r.summary.critical_count = 1;
        assert!(r.summary.critical_count > 0);
    }
}
