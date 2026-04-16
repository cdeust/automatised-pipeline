// prd_validator — Stage 6: validate a PRD's claimed changes against the
// resolved + clustered code graph.
//
// Three validation axes, all expressible as Cypher queries over G:
//   1. Symbol hallucination          — claimed symbols that don't exist.
//   2. Community-consistency         — scope claim vs communities touched.
//   3. Process-impact contradiction  — "does not affect <process>" claim.
//
// Read-only w.r.t. the graph. LLM-free. Deterministic given the same PRD +
// same graph.
//
// source: stages/stage-6.md §4 (extraction contract + regex fallback),
//         §5 (validation axes), §6 (output schema).

use crate::graph_store::GraphStore;
use crate::search;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

// source: stages/stage-6.md §4.2 — structured affected-symbols contract filename.
#[allow(dead_code)] // referenced by docs and tests; exported for downstream consumers.
pub const AFFECTED_SYMBOLS_FILE: &str = "stage-5.affected_symbols.json";

// source: stages/stage-6.md §6.2 — validation artifact filename.
pub const VALIDATION_FILE: &str = "stage-6.validation.json";

// source: stages/stage-6.md §5 V2 — threshold for community-consistency warning.
// ≥2 communities -> warning, ≥3 -> critical (matches specs §4/§5 severity ladder).
pub const COMMUNITY_SPAN_WARNING_THRESHOLD: u64 = 2;
pub const COMMUNITY_SPAN_CRITICAL_THRESHOLD: u64 = 3;

// source: stages/stage-6.md §4.3 — regex fallback token minimum length.
// Matches prd_input.rs MIN_TOKEN_LEN rationale (Lucene StandardAnalyzer).
const FALLBACK_MIN_TOKEN_LEN: usize = 3;

// source: stages/stage-6.md §4.3 — cap extracted tokens so a pathological PRD
// can't explode the graph lookup budget. Mirrors prd_input.rs MAX_TOKENS.
const FALLBACK_MAX_TOKENS: usize = 64;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

pub struct ValidationReport {
    pub validation_status: String,
    pub findings: Vec<ValidationFinding>,
    pub summary: ValidationSummary,
    pub extraction_mode: String,
    pub contract_missing: bool,
    pub affected_symbol_count: usize,
    pub scope_claim_count: usize,
}

pub struct ValidationFinding {
    pub axis: String,
    pub severity: String,
    pub message: String,
    pub symbol: Option<String>,
    pub details: Value,
}

pub struct ValidationSummary {
    pub claimed_symbols: u64,
    pub resolved_symbols: u64,
    pub hallucinated_symbols: u64,
    pub communities_spanned: u64,
    pub processes_impacted: u64,
}

// ---------------------------------------------------------------------------
// Orchestration
// ---------------------------------------------------------------------------

pub fn validate_prd(
    store: &GraphStore,
    prd_path: &Path,
    affected_symbols_path: Option<&Path>,
) -> Result<ValidationReport, String> {
    let prd_text = read_prd_text(prd_path)?;
    let (claims, scope_claims, mode, contract_missing) =
        load_claims(&prd_text, affected_symbols_path);
    let resolved = resolve_claims(store, &claims);
    let mut findings: Vec<ValidationFinding> = Vec::new();
    emit_symbol_hallucination(&resolved, &mut findings);
    let communities = communities_for_resolved(store, &resolved);
    emit_community_consistency(&scope_claims, &communities, &mut findings);
    let processes = processes_for_resolved(store, &resolved);
    emit_process_impact(&scope_claims, &processes, &mut findings);
    emit_unresolved_info(&resolved, mode == "regex_fallback", &mut findings);
    let status = compute_status(&findings);
    Ok(ValidationReport {
        validation_status: status,
        summary: ValidationSummary {
            claimed_symbols: claims.len() as u64,
            resolved_symbols: resolved.iter().filter(|r| r.resolved_qn.is_some()).count() as u64,
            hallucinated_symbols: resolved.iter().filter(|r| r.resolved_qn.is_none()).count() as u64,
            communities_spanned: distinct_count(&communities) as u64,
            processes_impacted: processes.len() as u64,
        },
        findings,
        extraction_mode: mode,
        contract_missing,
        affected_symbol_count: claims.len(),
        scope_claim_count: scope_claims.len(),
    })
}

fn read_prd_text(prd_path: &Path) -> Result<String, String> {
    if !prd_path.exists() {
        // PRD path is optional for the symbol-hallucination axis when the
        // structured contract is present. Return empty-string so the regex
        // fallback path yields zero tokens rather than panicking on missing.
        return Ok(String::new());
    }
    fs::read_to_string(prd_path)
        .map_err(|e| format!("prd_read_failed: {}: {}", prd_path.display(), e))
}

// ---------------------------------------------------------------------------
// Claim extraction — contract-first with regex fallback
// ---------------------------------------------------------------------------

struct SymbolClaim {
    token: String,          // raw text as it appeared (qualified_name or identifier)
    change_kind: String,    // add | modify | remove | rename | unknown
    #[allow(dead_code)] // retained from structured contract; future axes will surface it in findings.
    rationale: String,
}

#[derive(Clone, Debug)]
enum ScopeClaim {
    CommunityScope {
        #[allow(dead_code)] // surfaced in future "expected community name" axis (stage-6 §5 V2).
        assertion: String,
    },
    ProcessExclusion { processes: Vec<String> },
}

fn load_claims(
    prd_text: &str,
    affected_symbols_path: Option<&Path>,
) -> (Vec<SymbolClaim>, Vec<ScopeClaim>, String, bool) {
    if let Some(path) = affected_symbols_path {
        if path.exists() {
            if let Ok(raw) = fs::read_to_string(path) {
                if let Ok(v) = serde_json::from_str::<Value>(&raw) {
                    let (claims, scopes) = parse_structured_claims(&v);
                    return (claims, scopes, "structured".into(), false);
                }
            }
        }
    }
    // Fallback — regex-only extraction, high recall, low precision.
    let claims = regex_extract_symbols(prd_text);
    (claims, Vec::new(), "regex_fallback".into(), true)
}

fn parse_structured_claims(v: &Value) -> (Vec<SymbolClaim>, Vec<ScopeClaim>) {
    let mut claims = Vec::new();
    if let Some(arr) = v.get("affected_symbols").and_then(|x| x.as_array()) {
        for item in arr {
            let qn = str_field(item, "qualified_name");
            if qn.is_empty() { continue; }
            claims.push(SymbolClaim {
                token: qn,
                change_kind: str_field_default(item, "change_kind", "unknown"),
                rationale: str_field(item, "rationale"),
            });
        }
    }
    let mut scopes = Vec::new();
    if let Some(arr) = v.get("scope_claims").and_then(|x| x.as_array()) {
        for item in arr {
            match str_field(item, "kind").as_str() {
                "community_scope" => scopes.push(ScopeClaim::CommunityScope {
                    assertion: str_field(item, "assertion"),
                }),
                "process_exclusion" => {
                    let procs: Vec<String> = item
                        .get("processes")
                        .and_then(|x| x.as_array())
                        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                        .unwrap_or_default();
                    scopes.push(ScopeClaim::ProcessExclusion { processes: procs });
                }
                _ => {}
            }
        }
    }
    (claims, scopes)
}

fn str_field(v: &Value, key: &str) -> String {
    v.get(key).and_then(|x| x.as_str()).unwrap_or("").to_string()
}

fn str_field_default(v: &Value, key: &str, default: &str) -> String {
    let s = str_field(v, key);
    if s.is_empty() { default.to_string() } else { s }
}

// Regex fallback (implemented without an external regex crate — the "rules"
// state no new crates). We do a hand-written scan that matches the three
// patterns from stages/stage-6.md §4.3:
//   1. Backticked qualified name `A::B::C`
//   2. Backticked identifier `word_like`  (len >= 3)
//   3. File path with extension `src/main.rs`
fn regex_extract_symbols(text: &str) -> Vec<SymbolClaim> {
    let mut out: Vec<SymbolClaim> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for raw in extract_backticked(text) {
        push_token(&mut out, &mut seen, raw);
        if out.len() >= FALLBACK_MAX_TOKENS { return out; }
    }
    for raw in extract_file_paths(text) {
        push_token(&mut out, &mut seen, raw);
        if out.len() >= FALLBACK_MAX_TOKENS { return out; }
    }
    out
}

fn push_token(out: &mut Vec<SymbolClaim>, seen: &mut BTreeSet<String>, token: String) {
    if token.len() < FALLBACK_MIN_TOKEN_LEN { return; }
    if !seen.insert(token.clone()) { return; }
    out.push(SymbolClaim {
        token,
        change_kind: "unknown".into(),
        rationale: "regex_fallback".into(),
    });
}

fn extract_backticked(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'`' {
            let start = i + 1;
            let mut j = start;
            while j < bytes.len() && bytes[j] != b'`' && bytes[j] != b'\n' {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'`' {
                let slice = &text[start..j];
                if is_identifier_or_qn(slice) {
                    out.push(slice.to_string());
                }
                i = j + 1;
                continue;
            }
        }
        i += 1;
    }
    out
}

fn is_identifier_or_qn(s: &str) -> bool {
    if s.is_empty() { return false; }
    s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':')
        && s.chars().any(|c| c.is_ascii_alphabetic() || c == '_')
}

fn extract_file_paths(text: &str) -> Vec<String> {
    // Match `word(/word)+\.(rs|ts|tsx|py|go|js)` — file-path-looking tokens.
    let mut out = Vec::new();
    for raw in text.split(|c: char| c.is_ascii_whitespace() || c == '`' || c == '(' || c == ')' || c == ',') {
        if looks_like_file_path(raw) {
            out.push(raw.trim_end_matches(&['.', ',', ';', ':'][..]).to_string());
        }
    }
    out
}

fn looks_like_file_path(s: &str) -> bool {
    let exts = [".rs", ".ts", ".tsx", ".py", ".go", ".js"];
    s.contains('/')
        && exts.iter().any(|e| s.ends_with(e))
        && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '/' | '-' | '.'))
}

// ---------------------------------------------------------------------------
// Claim resolution — use search::resolve_qualified_name for strip-prefix + fuzz
// ---------------------------------------------------------------------------

struct ResolvedClaim<'a> {
    claim: &'a SymbolClaim,
    resolved_qn: Option<String>,
    did_you_mean: Vec<String>,
}

fn resolve_claims<'a>(store: &GraphStore, claims: &'a [SymbolClaim]) -> Vec<ResolvedClaim<'a>> {
    claims.iter().map(|claim| resolve_one(store, claim)).collect()
}

fn resolve_one<'a>(store: &GraphStore, claim: &'a SymbolClaim) -> ResolvedClaim<'a> {
    match search::resolve_qualified_name(store, &claim.token) {
        Ok(qn) => ResolvedClaim { claim, resolved_qn: Some(qn), did_you_mean: Vec::new() },
        Err(nf) => ResolvedClaim { claim, resolved_qn: None, did_you_mean: nf.did_you_mean },
    }
}

// ---------------------------------------------------------------------------
// Axis 1 — symbol hallucination
// ---------------------------------------------------------------------------

fn emit_symbol_hallucination(resolved: &[ResolvedClaim], findings: &mut Vec<ValidationFinding>) {
    for r in resolved {
        if r.resolved_qn.is_some() { continue; }
        // `add` + regex_fallback tokens are info; structured `modify/remove/rename`
        // absent = critical (can't modify what isn't in the graph).
        let kind = r.claim.change_kind.as_str();
        let is_structured_modify = matches!(kind, "modify" | "remove" | "rename");
        if is_structured_modify {
            findings.push(ValidationFinding {
                axis: "symbol_hallucination".into(),
                severity: "critical".into(),
                message: format!(
                    "claimed symbol '{}' (change_kind={}) not found in graph",
                    r.claim.token, kind
                ),
                symbol: Some(r.claim.token.clone()),
                details: json!({
                    "change_kind": kind,
                    "did_you_mean": r.did_you_mean,
                }),
            });
        }
    }
}

fn emit_unresolved_info(
    resolved: &[ResolvedClaim],
    is_regex_fallback: bool,
    findings: &mut Vec<ValidationFinding>,
) {
    if !is_regex_fallback { return; }
    for r in resolved {
        if r.resolved_qn.is_some() { continue; }
        findings.push(ValidationFinding {
            axis: "symbol_hallucination".into(),
            severity: "info".into(),
            message: format!("unresolved token '{}' (regex fallback — likely prose)", r.claim.token),
            symbol: Some(r.claim.token.clone()),
            details: json!({ "did_you_mean": r.did_you_mean, "extraction_mode": "regex_fallback" }),
        });
    }
}

// ---------------------------------------------------------------------------
// Axis 2 — community-consistency
// ---------------------------------------------------------------------------

fn communities_for_resolved(store: &GraphStore, resolved: &[ResolvedClaim]) -> Vec<Option<String>> {
    resolved
        .iter()
        .filter_map(|r| r.resolved_qn.as_ref())
        .map(|qn| community_of(store, qn))
        .collect()
}

fn community_of(store: &GraphStore, qualified_name: &str) -> Option<String> {
    // Iterate per-label rather than using rel-type alternation — mirrors
    // search/mod.rs::lookup_community for lbug dialect compatibility.
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

fn emit_community_consistency(
    scope_claims: &[ScopeClaim],
    communities: &[Option<String>],
    findings: &mut Vec<ValidationFinding>,
) {
    let distinct = distinct_count(communities) as u64;
    let has_scope_assertion = scope_claims.iter().any(|s| matches!(s, ScopeClaim::CommunityScope { .. }));
    if !has_scope_assertion && distinct < COMMUNITY_SPAN_WARNING_THRESHOLD {
        return;
    }
    let severity = if distinct >= COMMUNITY_SPAN_CRITICAL_THRESHOLD {
        "critical"
    } else if distinct >= COMMUNITY_SPAN_WARNING_THRESHOLD {
        "warning"
    } else {
        return;
    };
    let touched: Vec<String> = communities.iter().filter_map(|c| c.clone()).collect();
    let mut unique: Vec<String> = touched.iter().cloned().collect::<BTreeSet<_>>().into_iter().collect();
    unique.sort();
    findings.push(ValidationFinding {
        axis: "community_consistency".into(),
        severity: severity.into(),
        message: format!(
            "affected symbols span {} distinct Leiden communities (threshold: warning at {}, critical at {})",
            distinct, COMMUNITY_SPAN_WARNING_THRESHOLD, COMMUNITY_SPAN_CRITICAL_THRESHOLD
        ),
        symbol: None,
        details: json!({
            "distinct_communities": distinct,
            "communities": unique,
            "has_scope_assertion": has_scope_assertion,
        }),
    });
}

fn distinct_count(items: &[Option<String>]) -> usize {
    let set: BTreeSet<&String> = items.iter().filter_map(|x| x.as_ref()).collect();
    set.len()
}

// ---------------------------------------------------------------------------
// Axis 3 — process-impact contradiction
// ---------------------------------------------------------------------------

fn processes_for_resolved(
    store: &GraphStore,
    resolved: &[ResolvedClaim],
) -> Vec<(String, Vec<String>)> {
    resolved
        .iter()
        .filter_map(|r| r.resolved_qn.as_ref().map(|qn| (qn.clone(), processes_of(store, qn))))
        .collect()
}

fn processes_of(store: &GraphStore, qualified_name: &str) -> Vec<String> {
    let escaped = qualified_name.replace('\'', "\\'");
    let mut out: Vec<String> = Vec::new();
    for label in ["Function", "Method"] {
        let rel = format!("ParticipatesIn_{label}_Process");
        let cypher = format!(
            "MATCH (n:{label})-[:{rel}]->(p:Process) \
             WHERE n.qualified_name = '{escaped}' RETURN p.name"
        );
        if let Ok(qr) = store.execute_query(&cypher) {
            for row in &qr.rows {
                if let Some(name) = row.first() {
                    if !name.is_empty() && !out.contains(name) { out.push(name.clone()); }
                }
            }
        }
    }
    out
}

fn emit_process_impact(
    scope_claims: &[ScopeClaim],
    processes: &[(String, Vec<String>)],
    findings: &mut Vec<ValidationFinding>,
) {
    for claim in scope_claims {
        let excluded = match claim {
            ScopeClaim::ProcessExclusion { processes: ps } => ps,
            _ => continue,
        };
        for (symbol, actual) in processes {
            for hit in actual.iter().filter(|a| excluded.iter().any(|e| e == *a)) {
                findings.push(ValidationFinding {
                    axis: "process_impact".into(),
                    severity: "critical".into(),
                    message: format!(
                        "PRD claims exclusion of process '{}', but '{}' participates in it",
                        hit, symbol
                    ),
                    symbol: Some(symbol.clone()),
                    details: json!({ "process": hit, "excluded_processes": excluded }),
                });
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Status + artifact
// ---------------------------------------------------------------------------

fn compute_status(findings: &[ValidationFinding]) -> String {
    let mut has_critical = false;
    let mut has_warning = false;
    for f in findings {
        match f.severity.as_str() {
            "critical" => has_critical = true,
            "warning" => has_warning = true,
            _ => {}
        }
    }
    if has_critical { "fail".into() }
    else if has_warning { "warning".into() }
    else { "ok".into() }
}

pub fn report_to_json(
    report: &ValidationReport,
    run_id: &str,
    finding_id: &str,
    prd_path: &Path,
    graph_path: &Path,
    validated_at: &str,
) -> Value {
    let findings: Vec<Value> = report.findings.iter().map(|f| json!({
        "axis": f.axis, "severity": f.severity, "message": f.message,
        "symbol": f.symbol, "details": f.details,
    })).collect();
    json!({
        "run_id": run_id,
        "finding_id": finding_id,
        "prd_path": prd_path.to_string_lossy(),
        "graph_path": graph_path.to_string_lossy(),
        "validated_at": validated_at,
        "contract_missing": report.contract_missing,
        "extraction_mode": report.extraction_mode,
        "affected_symbol_count": report.affected_symbol_count,
        "scope_claim_count": report.scope_claim_count,
        "findings": findings,
        "validation_status": report.validation_status,
        "summary": {
            "claimed_symbols": report.summary.claimed_symbols,
            "resolved_symbols": report.summary.resolved_symbols,
            "hallucinated_symbols": report.summary.hallucinated_symbols,
            "communities_spanned": report.summary.communities_spanned,
            "processes_impacted": report.summary.processes_impacted,
        },
    })
}

pub fn write_validation(path: &Path, value: &Value) -> Result<PathBuf, String> {
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
    fn test_regex_extract_backticked() {
        let prd = "modify `handle_tool_call` in `src/main.rs` and the `GraphStore` helper.";
        let syms = regex_extract_symbols(prd);
        let tokens: Vec<String> = syms.iter().map(|s| s.token.clone()).collect();
        assert!(tokens.contains(&"handle_tool_call".to_string()), "got {:?}", tokens);
        assert!(tokens.contains(&"GraphStore".to_string()), "got {:?}", tokens);
        assert!(tokens.iter().any(|t| t == "src/main.rs"), "got {:?}", tokens);
    }

    #[test]
    fn test_regex_min_token_len() {
        let prd = "touch `ab` and `abc` and `foo`.";
        let syms = regex_extract_symbols(prd);
        assert!(syms.iter().all(|s| s.token.len() >= FALLBACK_MIN_TOKEN_LEN));
        assert!(syms.iter().any(|s| s.token == "abc"));
        assert!(!syms.iter().any(|s| s.token == "ab"));
    }

    #[test]
    fn test_compute_status_escalation() {
        let mut findings = Vec::new();
        findings.push(ValidationFinding {
            axis: "x".into(), severity: "info".into(),
            message: "".into(), symbol: None, details: json!({}),
        });
        assert_eq!(compute_status(&findings), "ok");
        findings.push(ValidationFinding {
            axis: "x".into(), severity: "warning".into(),
            message: "".into(), symbol: None, details: json!({}),
        });
        assert_eq!(compute_status(&findings), "warning");
        findings.push(ValidationFinding {
            axis: "x".into(), severity: "critical".into(),
            message: "".into(), symbol: None, details: json!({}),
        });
        assert_eq!(compute_status(&findings), "fail");
    }

    #[test]
    fn test_looks_like_file_path() {
        assert!(looks_like_file_path("src/main.rs"));
        assert!(looks_like_file_path("a/b/c.tsx"));
        assert!(!looks_like_file_path("main.rs"));
        assert!(!looks_like_file_path("src/main"));
        assert!(!looks_like_file_path("not/a/file.xyz"));
    }
}
