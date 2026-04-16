// stage6_integration — end-to-end tests for validate_prd_against_graph.
//
// Index a small fixture, then:
// 1. Prove the hallucinated-symbol critical finding fires.
// 2. Prove community-consistency warning/critical severities fire.
// 3. Prove process-impact contradiction fires.

use ai_architect_mcp::clustering;
use ai_architect_mcp::graph_store::GraphStore;
use ai_architect_mcp::indexer;
use ai_architect_mcp::prd_validator;
use ai_architect_mcp::resolver;
use serde_json::json;
use std::fs;
use std::path::PathBuf;

const FIXTURE_MAIN: &str = r#"
fn main() {
    handle_tool_call("probe");
}

fn handle_tool_call(name: &str) -> String {
    let resolved = resolve_name(name);
    format!("{}", resolved)
}

fn resolve_name(name: &str) -> String {
    name.to_string()
}

pub struct Tool {
    pub name: String,
}
"#;

fn tmp(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("stage6_{tag}_{}", std::process::id()))
}

fn build_fixture_graph(fixture_dir: &std::path::Path, graph_dir: &std::path::Path) -> GraphStore {
    fs::create_dir_all(fixture_dir.join("src")).unwrap();
    fs::write(fixture_dir.join("src/main.rs"), FIXTURE_MAIN).unwrap();
    let _ = indexer::index_codebase(&fixture_dir.join("src"), graph_dir).expect("index");
    let store = GraphStore::open_or_create(graph_dir).unwrap();
    let _ = resolver::resolve_graph(&store);
    let _ = clustering::cluster_graph(&store, 1.0);
    // Re-open so our caller sees the committed state.
    GraphStore::open_or_create(graph_dir).unwrap()
}

fn write_prd(dir: &std::path::Path, body: &str) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    let p = dir.join("stage-5.prd.md");
    fs::write(&p, body).unwrap();
    p
}

fn write_affected(dir: &std::path::Path, value: serde_json::Value) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    let p = dir.join("stage-5.affected_symbols.json");
    fs::write(&p, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
    p
}

#[test]
fn test_hallucinated_symbol_produces_critical_finding() {
    let root = tmp("hallucinate");
    let _ = fs::remove_dir_all(&root);
    let fixture_dir = root.join("fixture");
    let graph_dir = root.join("graph");
    let store = build_fixture_graph(&fixture_dir, &graph_dir);

    let prd_dir = root.join("prd");
    let prd_path = write_prd(&prd_dir, "modify `handle_tool_call` and `totally_fake_symbol`");
    let affected = write_affected(&prd_dir, json!({
        "affected_symbols": [
            { "qualified_name": "main.rs::handle_tool_call", "change_kind": "modify", "rationale": "real" },
            { "qualified_name": "main.rs::totally_fake_symbol", "change_kind": "modify", "rationale": "hallucinated" }
        ],
        "scope_claims": []
    }));

    let report = prd_validator::validate_prd(&store, &prd_path, Some(&affected))
        .expect("validate");

    let hallucination: Vec<_> = report.findings.iter()
        .filter(|f| f.axis == "symbol_hallucination" && f.severity == "critical")
        .collect();
    assert!(
        !hallucination.is_empty(),
        "expected at least one critical symbol_hallucination finding; got {:?}",
        report.findings.iter().map(|f| (f.axis.clone(), f.severity.clone(), f.symbol.clone())).collect::<Vec<_>>()
    );
    assert!(
        hallucination.iter().any(|f| f.symbol.as_deref() == Some("main.rs::totally_fake_symbol")
            && f.message.contains("not found")),
        "hallucinated symbol must be named in the finding"
    );
    assert_eq!(report.validation_status, "fail");
    assert_eq!(report.summary.hallucinated_symbols, 1);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_process_impact_contradiction_fires() {
    let root = tmp("processimpact");
    let _ = fs::remove_dir_all(&root);
    let fixture_dir = root.join("fixture");
    let graph_dir = root.join("graph");
    let store = build_fixture_graph(&fixture_dir, &graph_dir);

    let prd_dir = root.join("prd");
    let prd_path = write_prd(&prd_dir, "change handle_tool_call only; no main impact");

    // Process IDs follow process::<qualified_name> (clustering.rs §489).
    // main() lives at `main.rs::main`, hence process `process::main.rs::main`.
    let affected = write_affected(&prd_dir, json!({
        "affected_symbols": [
            { "qualified_name": "main.rs::handle_tool_call", "change_kind": "modify", "rationale": "modify body" }
        ],
        "scope_claims": [
            { "kind": "process_exclusion", "processes": ["process::main.rs::main"] }
        ]
    }));

    let report = prd_validator::validate_prd(&store, &prd_path, Some(&affected))
        .expect("validate");
    let critical: Vec<_> = report.findings.iter()
        .filter(|f| f.axis == "process_impact" && f.severity == "critical")
        .collect();
    assert!(
        !critical.is_empty(),
        "expected critical process_impact finding; got {:?}",
        report.findings.iter().map(|f| (f.axis.clone(), f.severity.clone())).collect::<Vec<_>>()
    );
    assert!(critical[0].message.contains("main"));
    assert_eq!(report.validation_status, "fail");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_regex_fallback_when_structured_contract_missing() {
    let root = tmp("fallback");
    let _ = fs::remove_dir_all(&root);
    let fixture_dir = root.join("fixture");
    let graph_dir = root.join("graph");
    let store = build_fixture_graph(&fixture_dir, &graph_dir);

    let prd_dir = root.join("prd");
    let prd_path = write_prd(&prd_dir, "modify `handle_tool_call` in `src/main.rs`");

    let report = prd_validator::validate_prd(&store, &prd_path, None)
        .expect("validate");
    assert!(report.contract_missing, "must flag contract_missing");
    assert_eq!(report.extraction_mode, "regex_fallback");
    // Regex fallback extracts at least handle_tool_call and src/main.rs.
    // `src/main.rs` must resolve via layer-2 strip-prefix (main.rs::...);
    // bare `handle_tool_call` only yields did-you-mean suggestions, so it
    // surfaces as an info-level unresolved token rather than a hit.
    assert!(report.summary.claimed_symbols >= 2,
        "expected >= 2 claims from regex fallback; got {}", report.summary.claimed_symbols);
    let info_count = report.findings.iter()
        .filter(|f| f.axis == "symbol_hallucination" && f.severity == "info")
        .count();
    assert!(info_count >= 1,
        "regex fallback must surface unresolved tokens as info findings");
    // No critical findings expected — structured-modify gate doesn't fire.
    let critical = report.findings.iter()
        .filter(|f| f.severity == "critical")
        .count();
    assert_eq!(critical, 0, "regex fallback unresolved tokens must not be critical");
    let _ = fs::remove_dir_all(&root);
}
