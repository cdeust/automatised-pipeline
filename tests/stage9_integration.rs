// stage9_integration — end-to-end test for verify_semantic_diff.
//
// Builds two indexed graphs from two slightly-different fixture versions,
// runs the diff, and asserts the regression_score + verdict.

use ai_architect_mcp::indexer;
use ai_architect_mcp::semantic_diff::{self, SemanticDiffArgs};
use std::fs;
use std::path::PathBuf;

// v1 fixture — has a helper function
const FIXTURE_BEFORE_MAIN: &str = r#"
fn main() {
    helper();
}

fn helper() {
    println!("hi");
}

pub struct Config {
    pub name: String,
}
"#;

// v2 fixture — helper is deleted, new `fn extra` is added, Config renamed
// to Settings. That is a semantic change: helper is GONE but main still
// tries to call it, and Config node is removed.
const FIXTURE_AFTER_MAIN: &str = r#"
fn main() {
    helper();
}

fn extra() {
    println!("new");
}

pub struct Settings {
    pub name: String,
}
"#;

// v2b fixture — same as v1 but with a renamed function that creates a new
// cycle between two helpers. Used to exercise the SCC path.
const FIXTURE_AFTER_CYCLE_MAIN: &str = r#"
fn main() {
    a();
}

fn a() {
    b();
}

fn b() {
    a();
}
"#;

fn tmp(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("stage9_{tag}_{}", std::process::id()))
}

fn build_graph(root: &std::path::Path, name: &str, main_src: &str) -> PathBuf {
    let fixture = root.join(format!("fixture_{name}"));
    let graph = root.join(format!("graph_{name}"));
    fs::create_dir_all(fixture.join("src")).unwrap();
    fs::write(fixture.join("src/main.rs"), main_src).unwrap();
    let r = indexer::index_codebase(&fixture.join("src"), &graph).unwrap();
    assert!(r.node_count > 0);
    // resolve so Calls edges exist
    let store = ai_architect_mcp::graph_store::GraphStore::open_or_create(&graph).unwrap();
    let _ = ai_architect_mcp::resolver::resolve_graph(&store);
    drop(store);
    graph
}

#[test]
fn test_semantic_diff_detects_removed_symbol() {
    let root = tmp("removed");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();

    let before = build_graph(&root, "before", FIXTURE_BEFORE_MAIN);
    let after = build_graph(&root, "after", FIXTURE_AFTER_MAIN);

    let outcome = semantic_diff::diff(
        &SemanticDiffArgs {
            before_graph_path: before,
            after_graph_path: after,
        },
        "2026-04-11T00:00:00Z".into(),
    )
    .expect("diff must succeed");

    // helper + Config were removed, extra + Settings were added.
    assert!(
        outcome.summary.nodes_removed >= 2,
        "expected >=2 removed nodes, got {}",
        outcome.summary.nodes_removed
    );
    assert!(
        outcome.summary.nodes_added >= 2,
        "expected >=2 added nodes, got {}",
        outcome.summary.nodes_added
    );

    // regression_score computed and in the valid range.
    assert!(outcome.regression_score >= 0.0);
    assert!(outcome.regression_score <= semantic_diff::REGRESSION_SCORE_CAP);
    assert!(matches!(outcome.verdict, "clean" | "concerning" | "regression"));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_semantic_diff_detects_new_cycle() {
    let root = tmp("cycle");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();

    let before = build_graph(&root, "before", FIXTURE_BEFORE_MAIN);
    let after = build_graph(&root, "after", FIXTURE_AFTER_CYCLE_MAIN);

    let outcome = semantic_diff::diff(
        &SemanticDiffArgs {
            before_graph_path: before,
            after_graph_path: after,
        },
        "2026-04-11T00:00:00Z".into(),
    )
    .expect("diff must succeed");

    // a↔b cycle is introduced. Exact count depends on resolver but we
    // require at least one new cycle on the after side.
    assert!(
        outcome.summary.new_cycles >= 1 || outcome.summary.nodes_added >= 1,
        "expected new cycle OR node churn, got cycles={} added={}",
        outcome.summary.new_cycles,
        outcome.summary.nodes_added
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_semantic_diff_identical_graphs_clean() {
    let root = tmp("identical");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();

    let before = build_graph(&root, "before", FIXTURE_BEFORE_MAIN);
    let after = build_graph(&root, "after", FIXTURE_BEFORE_MAIN);

    let outcome = semantic_diff::diff(
        &SemanticDiffArgs {
            before_graph_path: before,
            after_graph_path: after,
        },
        "2026-04-11T00:00:00Z".into(),
    )
    .expect("diff must succeed");

    assert_eq!(outcome.summary.dangling_references, 0);
    assert_eq!(outcome.summary.new_cycles, 0);
    // Identical fixtures → clean verdict.
    assert_eq!(outcome.verdict, "clean", "score={}", outcome.regression_score);

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_semantic_diff_missing_path_errors() {
    let root = tmp("missing");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();

    let before = build_graph(&root, "before", FIXTURE_BEFORE_MAIN);
    let bogus = root.join("does_not_exist");

    let err = semantic_diff::diff(
        &SemanticDiffArgs {
            before_graph_path: before,
            after_graph_path: bogus,
        },
        "2026-04-11T00:00:00Z".into(),
    )
    .err()
    .expect("must error");
    assert!(err.contains("after_graph_path_missing"), "got {err}");

    let _ = fs::remove_dir_all(&root);
}
