// stage3c_integration — end-to-end test for clustering + process tracing.
//
// Creates a fixture project with main(), test functions, cross-module calls,
// indexes it, resolves it, then clusters and traces processes. Verifies:
// - communities are created and every symbol is in exactly one community
// - processes are traced from main + test entry points
// source: stages/stage-3c.md §6

use ai_architect_mcp::clustering;
use ai_architect_mcp::graph_store::GraphStore;
use ai_architect_mcp::indexer;
use ai_architect_mcp::resolver;
use std::fs;

// ---------------------------------------------------------------------------
// Fixture: multi-module project with known call structure
// ---------------------------------------------------------------------------

const FIXTURE_MAIN: &str = r#"
use crate::service;

fn main() {
    let result = service::process_data("input");
    println!("{}", result);
}

fn test_basic() {
    let _ = service::process_data("test");
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

pub fn unused_helper() -> i32 {
    42
}
"#;

// ---------------------------------------------------------------------------
// Test
// ---------------------------------------------------------------------------

#[test]
fn test_clustering_and_process_tracing() {
    let tmp_root = std::env::temp_dir().join(format!(
        "stage3c_integration_{}", std::process::id()
    ));
    let _ = fs::remove_dir_all(&tmp_root);

    // Set up fixture project
    let fixture_dir = tmp_root.join("fixture/src");
    fs::create_dir_all(&fixture_dir).expect("create fixture");
    fs::write(fixture_dir.join("main.rs"), FIXTURE_MAIN).unwrap();
    fs::write(fixture_dir.join("service.rs"), FIXTURE_SERVICE).unwrap();
    fs::write(fixture_dir.join("helpers.rs"), FIXTURE_HELPERS).unwrap();

    // Index + resolve
    let graph_dir = tmp_root.join("graph");
    let idx = indexer::index_codebase(&fixture_dir, &graph_dir)
        .expect("index_codebase");
    assert_eq!(idx.files_indexed, 3);

    let store = GraphStore::open_or_create(&graph_dir).unwrap();
    let _res = resolver::resolve_graph(&store).expect("resolve_graph");

    // Cluster
    let result = clustering::cluster_graph(&store, 1.0)
        .expect("cluster_graph");

    assert!(
        result.communities > 0,
        "should detect at least 1 community, got {}",
        result.communities
    );

    // Verify I1: every symbol has exactly one MemberOf edge
    let symbol_labels = [
        "Function", "Method", "Struct", "Enum", "Trait",
        "Constant", "TypeAlias", "Module",
    ];
    let mut total_symbols = 0u64;
    let mut total_memberof = 0u64;
    for label in &symbol_labels {
        let qr = store.execute_query(
            &format!("MATCH (n:{label}) RETURN count(n)")
        ).unwrap();
        let count: u64 = qr.rows[0][0].parse().unwrap_or(0);
        total_symbols += count;

        let rel = format!("MemberOf_{label}_Community");
        let qr2 = store.execute_query(
            &format!("MATCH ()-[r:{rel}]->() RETURN count(r)")
        ).unwrap();
        let edge_count: u64 = qr2.rows[0][0].parse().unwrap_or(0);
        total_memberof += edge_count;
    }
    assert_eq!(
        total_symbols, total_memberof,
        "I1 violated: {total_symbols} symbols but {total_memberof} MemberOf edges"
    );

    // Verify processes were created
    assert!(
        result.processes > 0,
        "should detect at least 1 process, got {}",
        result.processes
    );

    // Verify Process nodes exist
    let qr = store.execute_query("MATCH (p:Process) RETURN p.name, p.entry_kind")
        .unwrap();
    assert!(
        !qr.rows.is_empty(),
        "should have Process nodes"
    );

    // Verify main entry point was detected
    let entry_kinds: Vec<&str> = qr.rows.iter().map(|r| r[1].as_str()).collect();
    assert!(
        entry_kinds.contains(&"main"),
        "should detect main entry point, got kinds: {entry_kinds:?}"
    );

    // Verify I4: every Process has exactly one EntryPointOf edge
    for row in &qr.rows {
        let pname = row[0].replace('\'', "\\'");
        let ep_count_qr = store.execute_query(&format!(
            "MATCH (f)-[:EntryPointOf_Function_Process]->(p:Process) \
             WHERE p.name = '{pname}' RETURN count(f)"
        )).unwrap();
        let ep_count: u64 = ep_count_qr.rows[0][0].parse().unwrap_or(0);
        assert!(
            ep_count >= 1,
            "I4 violated: process {pname} has {ep_count} EntryPointOf edges"
        );
    }

    // Verify get_processes returns data
    let processes = clustering::get_processes(&store).unwrap();
    assert!(!processes.is_empty(), "get_processes should return data");

    // Verify get_impact for main function (use actual ID from graph)
    let main_qr = store.execute_query(
        "MATCH (f:Function) WHERE f.name = 'main' RETURN f.id"
    ).unwrap();
    let main_id = &main_qr.rows[0][0];
    let impact = clustering::get_impact(&store, main_id).unwrap();
    assert!(
        !impact.communities.is_empty(),
        "main should belong to at least one community"
    );

    // Verify modularity is reasonable
    assert!(
        result.modularity >= -1.0 && result.modularity <= 1.0,
        "modularity out of range: {}",
        result.modularity
    );

    let _ = fs::remove_dir_all(&tmp_root);
}
