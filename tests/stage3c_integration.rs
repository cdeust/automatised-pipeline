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
mod common;
use common::TempDirExt;

// ---------------------------------------------------------------------------
// Fixture: multi-module project with known call structure
// ---------------------------------------------------------------------------

const FIXTURE_MAIN: &str = r#"
use crate::service;

fn main() {
    let result = service::process_data("input");
    println!("{}", result);
}

#[test]
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

fn indexed_fixture(tmp_root: &std::path::Path) -> (GraphStore, indexer::IndexResult) {
    // Set up fixture project
    let fixture_dir = tmp_root.join("fixture/src");
    fs::create_dir_all(&fixture_dir).expect("create fixture");
    fs::write(fixture_dir.join("main.rs"), FIXTURE_MAIN).unwrap();
    fs::write(fixture_dir.join("service.rs"), FIXTURE_SERVICE).unwrap();
    fs::write(fixture_dir.join("helpers.rs"), FIXTURE_HELPERS).unwrap();

    // Index + resolve
    let graph_dir = tmp_root.join("graph");
    let idx = indexer::index_codebase(&fixture_dir, &graph_dir).expect("index_codebase");

    let store = GraphStore::open_or_create(&graph_dir).unwrap();
    resolver::resolve_graph(&store).expect("resolve_graph");

    (store, idx)
}

fn assert_symbol_membership(store: &GraphStore) {
    // Verify I1: every symbol has exactly one MemberOf edge
    let symbol_labels = [
        "Function",
        "Method",
        "Struct",
        "Enum",
        "Trait",
        "Constant",
        "TypeAlias",
        "Module",
    ];
    let mut total_symbols = 0u64;
    let mut total_memberof = 0u64;
    for label in &symbol_labels {
        let qr = store
            .execute_query(&format!("MATCH (n:{label}) RETURN count(n)"))
            .unwrap();
        let count: u64 = qr.rows[0][0].parse().unwrap_or(0);
        total_symbols += count;

        let rel = format!("MemberOf_{label}_Community");
        let qr2 = store
            .execute_query(&format!("MATCH ()-[r:{rel}]->() RETURN count(r)"))
            .unwrap();
        let edge_count: u64 = qr2.rows[0][0].parse().unwrap_or(0);
        total_memberof += edge_count;
    }
    assert_eq!(
        total_symbols, total_memberof,
        "I1 violated: {total_symbols} symbols but {total_memberof} MemberOf edges"
    );
}

fn assert_process_entry_points(store: &GraphStore) {
    // Verify Process nodes exist
    let qr = store
        .execute_query("MATCH (p:Process) RETURN p.name, p.entry_kind")
        .unwrap();
    assert!(!qr.rows.is_empty(), "should have Process nodes");

    // Verify main entry point was detected
    let entry_kinds: Vec<&str> = qr.rows.iter().map(|r| r[1].as_str()).collect();
    assert!(
        entry_kinds.contains(&"main"),
        "should detect main entry point, got kinds: {entry_kinds:?}"
    );

    // Verify I4: every Process has exactly one EntryPointOf edge
    for row in &qr.rows {
        let pname = row[0].replace('\'', "\\'");
        let ep_count_qr = store
            .execute_query(&format!(
                "MATCH (f)-[:EntryPointOf_Function_Process]->(p:Process) \
             WHERE p.name = '{pname}' RETURN count(f)"
            ))
            .unwrap();
        let ep_count: u64 = ep_count_qr.rows[0][0].parse().unwrap_or(0);
        assert!(
            ep_count >= 1,
            "I4 violated: process {pname} has {ep_count} EntryPointOf edges"
        );
    }
}

fn assert_process_queries_and_depth(store: &GraphStore) {
    // Verify get_processes returns data
    let processes = clustering::get_processes(store).unwrap();
    assert!(!processes.is_empty(), "get_processes should return data");

    // Verify get_impact for main function (use actual ID from graph)
    let main_qr = store
        .execute_query("MATCH (f:Function) WHERE f.name = 'main' RETURN f.id")
        .unwrap();
    let main_id = &main_qr.rows[0][0];
    let impact = clustering::get_impact(store, main_id).unwrap();
    assert!(
        !impact.communities.is_empty(),
        "main should belong to at least one community"
    );

    // Ordering fix: ParticipatesIn edges must carry the real BFS depth, not a
    // flattened 0. The fixture chain is main(0) -> process_data(1) ->
    // {sanitize,transform}(2), so the max depth across participants is > 0.
    let depth_qr = store
        .execute_query("MATCH ()-[r:ParticipatesIn_Function_Process]->() RETURN r.depth")
        .unwrap();
    let max_part_depth: u64 = depth_qr
        .rows
        .iter()
        .filter_map(|r| r.first().and_then(|d| d.parse::<u64>().ok()))
        .max()
        .unwrap_or(0);
    assert!(
        max_part_depth > 0,
        "ParticipatesIn depth must reflect BFS distance (the call-chain order), \
         not be flattened to 0; got max depth {max_part_depth}"
    );
}

fn assert_reverse_impact(store: &GraphStore) {
    // Reverse-traversal fix: get_impact must return the symbols that DEPEND ON
    // the target. process_data is called by both main and test_basic, so its
    // callers set is non-empty and contains main.
    let pd_qr = store
        .execute_query("MATCH (f:Function) WHERE f.name = 'process_data' RETURN f.id")
        .unwrap();
    let pd_id = &pd_qr.rows[0][0];
    let pd_impact = clustering::get_impact(store, pd_id).unwrap();
    assert!(
        !pd_impact.callers.is_empty(),
        "get_impact(process_data) must return its callers (main, test_basic), \
         got none — reverse traversal flattened away"
    );
    let caller_qns: Vec<&str> = pd_impact
        .callers
        .iter()
        .map(|c| c.qualified_name.as_str())
        .collect();
    assert!(
        caller_qns.iter().any(|q| q.ends_with("::main")),
        "main must appear as a caller of process_data; got {caller_qns:?}"
    );
    // Handles must be re-queryable: a non-empty id is what lets the caller keep
    // traversing through MCP (get_symbol/get_context) instead of dead-ending.
    assert!(
        pd_impact.callers.iter().all(|c| !c.id.is_empty()),
        "every caller handle must carry a non-empty id for further traversal"
    );
}

fn assert_mapping_entries(memberships: &clustering::ClusterMemberships) {
    assert!(
        !memberships.entries.is_empty(),
        "clusters mapping must be non-empty after cluster_graph persistence"
    );
    assert_eq!(
        memberships.truncated_at, None,
        "fixture has <10k symbols, truncation flag must not trigger"
    );
    assert_eq!(memberships.entries.len(), memberships.total);

    // Every entry must carry a real qualified_name and a non-negative
    // cluster id extracted from the community_id suffix.
    for m in &memberships.entries {
        assert!(
            !m.qualified_name.is_empty(),
            "empty qualified_name in membership"
        );
        assert!(
            m.community_id.starts_with("community::louvain::"),
            "unexpected community_id shape: {}",
            m.community_id
        );
        assert!(
            m.cluster_id >= 0,
            "cluster_id failed to parse from {}",
            m.community_id
        );
    }
}

#[test]
fn test_clustering_and_process_tracing() {
    // issue #25 audit: process::id() collides across processes under PID
    // reuse; tempfile's random suffix does not.
    let tmp_root = tempfile::Builder::new()
        .prefix("stage3c_integration_")
        .tempdir()
        .expect("create temp dir")
        .keep_managed();
    let _ = fs::remove_dir_all(&tmp_root);

    let (store, idx) = indexed_fixture(&tmp_root);
    assert_eq!(idx.files_indexed, 3);

    // Cluster
    let result = clustering::cluster_graph(&store, 1.0).expect("cluster_graph");

    assert!(
        result.communities > 0,
        "should detect at least 1 community, got {}",
        result.communities
    );

    assert_symbol_membership(&store);

    // Verify processes were created
    assert!(
        result.processes > 0,
        "should detect at least 1 process, got {}",
        result.processes
    );

    assert_process_entry_points(&store);

    assert_process_queries_and_depth(&store);

    assert_reverse_impact(&store);

    // Verify modularity is reasonable
    assert!(
        result.modularity >= -1.0 && result.modularity <= 1.0,
        "modularity out of range: {}",
        result.modularity
    );

    let _ = fs::remove_dir_all(&tmp_root);
}

#[test]
fn test_cluster_graph_returns_mapping() {
    // Regression for B2: `cluster_graph` must surface a per-symbol
    // community membership mapping, not just counts/modularity. Without
    // this, the harness Q12 scorer (adjusted Rand index) has nothing to
    // compare against and collapses to 0.
    // issue #25 audit: process::id() collides across processes under PID
    // reuse; tempfile's random suffix does not.
    let tmp_root = tempfile::Builder::new()
        .prefix("stage3c_mapping_")
        .tempdir()
        .expect("create temp dir")
        .keep_managed();
    let _ = fs::remove_dir_all(&tmp_root);

    let (store, _) = indexed_fixture(&tmp_root);
    clustering::cluster_graph(&store, 1.0).expect("cluster_graph");

    let memberships = clustering::collect_cluster_memberships(&store).expect("collect");

    assert_mapping_entries(&memberships);

    // Spot-check: the fixture's known function symbols show up.
    let qns: Vec<&str> = memberships
        .entries
        .iter()
        .map(|m| m.qualified_name.as_str())
        .collect();
    assert!(
        qns.iter().any(|q| q.ends_with("::main")),
        "main symbol missing from cluster mapping: {qns:?}"
    );
    assert!(
        qns.iter().any(|q| q.ends_with("::process_data")),
        "process_data symbol missing from cluster mapping: {qns:?}"
    );

    let _ = fs::remove_dir_all(&tmp_root);
}

#[test]
fn test_cluster_graph_is_idempotent() {
    // Regression for bench q12 = 0.000: re-running cluster_graph on an
    // already-clustered graph aborted with a duplicate primary key on
    // Community (`community::louvain::1::0`) because prior Community and
    // Process nodes were never purged. The harness clusters once at setup
    // and once per q12 label, so the label call always hit the error path
    // and the ARI scorer compared against an empty mapping.
    // issue #25 audit: process::id() collides across processes under PID
    // reuse; tempfile's random suffix does not.
    let tmp_root = tempfile::Builder::new()
        .prefix("stage3c_idempotent_")
        .tempdir()
        .expect("create temp dir")
        .keep_managed();
    let _ = fs::remove_dir_all(&tmp_root);

    let fixture_dir = tmp_root.join("fixture/src");
    fs::create_dir_all(&fixture_dir).expect("create fixture");
    fs::write(fixture_dir.join("main.rs"), FIXTURE_MAIN).unwrap();
    fs::write(fixture_dir.join("service.rs"), FIXTURE_SERVICE).unwrap();
    fs::write(fixture_dir.join("helpers.rs"), FIXTURE_HELPERS).unwrap();

    let graph_dir = tmp_root.join("graph");
    indexer::index_codebase(&fixture_dir, &graph_dir).expect("index_codebase");
    let store = GraphStore::open_or_create(&graph_dir).unwrap();
    resolver::resolve_graph(&store).expect("resolve_graph");

    let first = clustering::cluster_graph(&store, 1.0).expect("first cluster_graph");
    let second = clustering::cluster_graph(&store, 1.0)
        .expect("second cluster_graph must not hit duplicate primary keys");

    // Same graph, same gamma → same partition size, and the membership
    // mapping must be complete (one entry per symbol), not doubled or empty.
    assert_eq!(first.communities, second.communities);
    let memberships = clustering::collect_cluster_memberships(&store).expect("collect");
    let mut seen = std::collections::HashSet::new();
    for m in &memberships.entries {
        assert!(
            seen.insert(m.qualified_name.clone()),
            "symbol {} has more than one MemberOf edge after re-cluster",
            m.qualified_name
        );
    }
    assert!(!memberships.entries.is_empty());

    // Community node count must equal the reported partition size — no
    // stale communities from the first pass may survive the purge.
    let qr = store
        .execute_query("MATCH (c:Community) RETURN count(c)")
        .expect("count communities");
    let count: u64 = qr.rows[0][0].parse().unwrap_or(0);
    assert_eq!(count, second.communities);

    let _ = fs::remove_dir_all(&tmp_root);
}
