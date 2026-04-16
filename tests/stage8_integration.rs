// stage8_integration — end-to-end tests for check_security_gates.
//
// Uses a fixture with auth-pattern symbols and verifies S1, S3, S5 gate
// behaviors plus the S2 info-skip mode.

use ai_architect_mcp::clustering;
use ai_architect_mcp::graph_store::GraphStore;
use ai_architect_mcp::indexer;
use ai_architect_mcp::resolver;
use ai_architect_mcp::security_gates;
use std::fs;
use std::path::PathBuf;

// Fixture: one auth-pattern fn (`verify_token`) and one bystander that calls
// it. Louvain should put them in the same community because of the direct
// Calls edge. Also includes a top-level pub fn for the S3 test.
const FIXTURE_MAIN: &str = r#"
fn main() {
    api_v1();
}

pub fn api_v1() -> String {
    let t = verify_token("abc");
    t.to_string()
}

fn verify_token(token: &str) -> &str {
    token
}

pub fn untested_helper() -> u32 {
    42
}
"#;

fn tmp(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("stage8_{tag}_{}", std::process::id()))
}

fn build_fixture_graph(fixture_dir: &std::path::Path, graph_dir: &std::path::Path) -> GraphStore {
    fs::create_dir_all(fixture_dir.join("src")).unwrap();
    fs::write(fixture_dir.join("src/main.rs"), FIXTURE_MAIN).unwrap();
    let _ = indexer::index_codebase(&fixture_dir.join("src"), graph_dir).expect("index");
    let store = GraphStore::open_or_create(graph_dir).unwrap();
    let _ = resolver::resolve_graph(&store);
    let _ = clustering::cluster_graph(&store, 1.0);
    GraphStore::open_or_create(graph_dir).unwrap()
}

#[test]
fn test_s1_auth_critical_community_flag() {
    let root = tmp("s1");
    let _ = fs::remove_dir_all(&root);
    let fixture_dir = root.join("fixture");
    let graph_dir = root.join("graph");
    let store = build_fixture_graph(&fixture_dir, &graph_dir);

    // Change `api_v1` — it calls verify_token so they should share a community.
    let changed = vec!["main.rs::api_v1".to_string()];
    let report = security_gates::check_gates(&store, &changed).expect("check");
    let s1: Vec<_> = report.flags.iter()
        .filter(|f| f.gate == "auth_critical_touch")
        .collect();
    assert!(
        !s1.is_empty(),
        "expected S1 flag; flags were: {:?}",
        report.flags.iter().map(|f| (f.gate.clone(), f.severity.clone())).collect::<Vec<_>>()
    );
    assert_eq!(s1[0].severity, "critical");
    assert!(!report.gates_passed, "critical flag must fail the gate");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_s2_info_skip_mode() {
    let root = tmp("s2");
    let _ = fs::remove_dir_all(&root);
    let fixture_dir = root.join("fixture");
    let graph_dir = root.join("graph");
    let store = build_fixture_graph(&fixture_dir, &graph_dir);

    let changed = vec!["main.rs::untested_helper".to_string()];
    let report = security_gates::check_gates(&store, &changed).expect("check");
    let s2: Vec<_> = report.flags.iter()
        .filter(|f| f.gate == "unsafe_symbol")
        .collect();
    assert_eq!(s2.len(), 1, "S2 must emit exactly one info flag per changed symbol");
    assert_eq!(s2[0].severity, "info");
    assert!(s2[0].message.contains("is_unsafe"));
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_s3_public_api_warning() {
    let root = tmp("s3");
    let _ = fs::remove_dir_all(&root);
    let fixture_dir = root.join("fixture");
    let graph_dir = root.join("graph");
    let store = build_fixture_graph(&fixture_dir, &graph_dir);

    // api_v1 is a crate-root `pub fn`.
    let changed = vec!["main.rs::api_v1".to_string()];
    let report = security_gates::check_gates(&store, &changed).expect("check");
    let s3: Vec<_> = report.flags.iter()
        .filter(|f| f.gate == "public_api_change")
        .collect();
    assert!(!s3.is_empty(), "expected S3 warning on crate-root pub fn");
    assert_eq!(s3[0].severity, "warning");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_s5_test_coverage_gap() {
    let root = tmp("s5");
    let _ = fs::remove_dir_all(&root);
    let fixture_dir = root.join("fixture");
    let graph_dir = root.join("graph");
    let store = build_fixture_graph(&fixture_dir, &graph_dir);

    // No test entry points exist in the fixture — every symbol should trip S5.
    let changed = vec!["main.rs::untested_helper".to_string()];
    let report = security_gates::check_gates(&store, &changed).expect("check");
    let s5: Vec<_> = report.flags.iter()
        .filter(|f| f.gate == "test_coverage_gap")
        .collect();
    assert!(!s5.is_empty(), "expected S5 warning when no test process reaches the symbol");
    assert_eq!(s5[0].severity, "warning");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_gates_passed_reflects_severity() {
    let root = tmp("aggregate");
    let _ = fs::remove_dir_all(&root);
    let fixture_dir = root.join("fixture");
    let graph_dir = root.join("graph");
    let store = build_fixture_graph(&fixture_dir, &graph_dir);

    // `untested_helper` is NOT in the verify_token community, so S1 should
    // not fire on it. Warnings-only must PASS the gate.
    let changed = vec!["main.rs::untested_helper".to_string()];
    let report = security_gates::check_gates(&store, &changed).expect("check");
    assert_eq!(report.summary.critical_count, 0,
        "expected no critical flags for bystander symbol; got {:?}",
        report.flags.iter().map(|f| (f.gate.clone(), f.severity.clone())).collect::<Vec<_>>());
    assert!(report.gates_passed, "warnings-only must pass the gate");

    // api_v1 shares community with verify_token (S1 critical) => fails.
    let changed = vec!["main.rs::api_v1".to_string()];
    let report = security_gates::check_gates(&store, &changed).expect("check");
    assert!(report.summary.critical_count >= 1);
    assert!(!report.gates_passed);
    let _ = fs::remove_dir_all(&root);
}
