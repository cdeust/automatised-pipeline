// stage3d_integration — end-to-end test for search + context tools.
//
// Reuses the same fixture project from 3c, runs the full pipeline
// (index + resolve + cluster), then verifies search_graph and get_context
// return meaningful results.
// source: stages/stage-3.md §3d

use ai_architect_mcp::clustering;
use ai_architect_mcp::graph_store::GraphStore;
use ai_architect_mcp::indexer;
use ai_architect_mcp::resolver;
use ai_architect_mcp::search;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

// ---------------------------------------------------------------------------
// Fixture: multi-module project with known call structure
// ---------------------------------------------------------------------------

const FIXTURE_MAIN: &str = r#"
use crate::service;

fn main() {
    let result = service::process_data("input");
    println!("{}", result);
}

fn handle_request() {
    let _ = service::validate("test");
}
"#;

const FIXTURE_SERVICE: &str = r#"
use crate::helpers;

pub fn process_data(input: &str) -> String {
    let cleaned = helpers::sanitize(input);
    helpers::transform(&cleaned)
}

pub fn validate(input: &str) -> bool {
    !input.is_empty()
}
"#;

const FIXTURE_HELPERS: &str = r#"
pub fn sanitize(input: &str) -> String {
    input.trim().to_string()
}

pub fn transform(input: &str) -> String {
    input.to_uppercase()
}
"#;

// ---------------------------------------------------------------------------
// Unique directory per test to avoid lbug lock conflicts
// ---------------------------------------------------------------------------

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn setup_graph(test_name: &str) -> (PathBuf, GraphStore) {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let tmp_root = std::env::temp_dir().join(format!(
        "stage3d_{}_{n}_{}", test_name, std::process::id()
    ));
    let _ = fs::remove_dir_all(&tmp_root);

    let fixture_dir = tmp_root.join("fixture/src");
    fs::create_dir_all(&fixture_dir).expect("create fixture");
    fs::write(fixture_dir.join("main.rs"), FIXTURE_MAIN).unwrap();
    fs::write(fixture_dir.join("service.rs"), FIXTURE_SERVICE).unwrap();
    fs::write(fixture_dir.join("helpers.rs"), FIXTURE_HELPERS).unwrap();

    let graph_dir = tmp_root.join("graph");
    let _idx = indexer::index_codebase(&fixture_dir, &graph_dir)
        .expect("index_codebase");
    let store = GraphStore::open_or_create(&graph_dir).unwrap();
    let _res = resolver::resolve_graph(&store).expect("resolve_graph");
    let _cl = clustering::cluster_graph(&store, 1.0).expect("cluster_graph");

    (tmp_root, store)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn test_search_exact_name() {
    let (tmp_root, store) = setup_graph("exact");
    let opts = search::SearchOptions {
        limit: 10,
        label_filter: None,
        min_score: 0.0,
    };
    let results = search::search_graph(&store, "main", &opts).unwrap();
    assert!(!results.is_empty(), "search for 'main' should return results");
    assert_eq!(results[0].name, "main", "top result should be exact match");
    assert!(
        results[0].score >= 0.9,
        "exact match score should be >= 0.9, got {}",
        results[0].score
    );
    let _ = fs::remove_dir_all(&tmp_root);
}

#[test]
fn test_search_partial_name() {
    let (tmp_root, store) = setup_graph("partial");
    let opts = search::SearchOptions {
        limit: 10,
        label_filter: None,
        min_score: 0.0,
    };
    let results = search::search_graph(&store, "handle", &opts).unwrap();
    assert!(!results.is_empty(), "search for 'handle' should find handle_request");
    let found = results.iter().any(|r| r.name.contains("handle"));
    assert!(found, "should find a symbol containing 'handle'");
    let _ = fs::remove_dir_all(&tmp_root);
}

#[test]
fn test_search_label_filter() {
    let (tmp_root, store) = setup_graph("filter");
    let opts = search::SearchOptions {
        limit: 10,
        label_filter: Some("Function".to_string()),
        min_score: 0.0,
    };
    let results = search::search_graph(&store, "process", &opts).unwrap();
    for r in &results {
        assert_eq!(r.label, "Function", "label filter should be respected");
    }
    let _ = fs::remove_dir_all(&tmp_root);
}

#[test]
fn test_search_results_have_context() {
    let (tmp_root, store) = setup_graph("context_search");
    let opts = search::SearchOptions {
        limit: 5,
        label_filter: None,
        min_score: 0.0,
    };
    let results = search::search_graph(&store, "main", &opts).unwrap();
    let main_result = results.iter().find(|r| r.name == "main").unwrap();
    assert!(!main_result.file_path.is_empty(), "should have file_path");
    assert!(main_result.community_id.is_some(), "main should have a community");
    let _ = fs::remove_dir_all(&tmp_root);
}

#[test]
fn test_get_context() {
    let (tmp_root, store) = setup_graph("get_ctx");
    let qr = store.execute_query(
        "MATCH (f:Function) WHERE f.name = 'main' RETURN f.qualified_name"
    ).unwrap();
    assert!(!qr.rows.is_empty(), "should find main");
    let qn = &qr.rows[0][0];

    let ctx = search::get_context(&store, qn).unwrap();
    assert_eq!(ctx.name, "main");
    assert_eq!(ctx.label, "Function");
    assert!(!ctx.file_path.is_empty());
    assert!(ctx.community.is_some(), "main should have community context");
    assert!(!ctx.processes.is_empty(), "main should participate in processes");

    let _ = fs::remove_dir_all(&tmp_root);
}
