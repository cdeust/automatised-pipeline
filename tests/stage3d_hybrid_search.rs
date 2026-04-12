// stage3d_hybrid_search — integration test for BM25 + TF-IDF + RRF hybrid search.
//
// Verifies the full pipeline: index → resolve → cluster → build_search_index → search.
// Tests both BM25 keyword matching and TF-IDF semantic matching via RRF fusion.

use ai_architect_mcp::clustering;
use ai_architect_mcp::graph_store::GraphStore;
use ai_architect_mcp::indexer;
use ai_architect_mcp::resolver;
use ai_architect_mcp::search;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

const FIXTURE_MAIN: &str = r#"
fn main() {
    let result = handle_tool_call("test");
    println!("{}", result);
}

fn handle_tool_call(input: &str) -> String {
    format!("handled: {}", input)
}

fn process_request(data: &str) -> bool {
    !data.is_empty()
}

fn validate_input(s: &str) -> bool {
    s.len() > 0
}
"#;

const FIXTURE_SERVICE: &str = r#"
pub fn dispatch_message(msg: &str) -> String {
    msg.to_uppercase()
}

pub fn route_incoming(path: &str) -> String {
    format!("routed: {}", path)
}
"#;

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn setup_with_search_index(test_name: &str) -> (PathBuf, GraphStore) {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let tmp_root = std::env::temp_dir().join(format!(
        "stage3d_hybrid_{}_{n}_{}", test_name, std::process::id()
    ));
    let _ = fs::remove_dir_all(&tmp_root);

    let fixture_dir = tmp_root.join("fixture/src");
    fs::create_dir_all(&fixture_dir).expect("create fixture");
    fs::write(fixture_dir.join("main.rs"), FIXTURE_MAIN).unwrap();
    fs::write(fixture_dir.join("service.rs"), FIXTURE_SERVICE).unwrap();

    let graph_dir = tmp_root.join("graph");
    let _idx = indexer::index_codebase(&fixture_dir, &graph_dir)
        .expect("index_codebase");
    let store = GraphStore::open_or_create(&graph_dir).unwrap();
    let _res = resolver::resolve_graph(&store).expect("resolve_graph");
    let _cl = clustering::cluster_graph(&store, 1.0).expect("cluster_graph");

    // Build search indexes
    let si = search::build_search_index(&store, &tmp_root)
        .expect("build_search_index");
    assert!(si.bm25_doc_count > 0, "should index BM25 docs");
    assert!(si.vector_doc_count > 0, "should index vector docs");

    // Set env hint for search_graph to find the index
    let idx_dir = tmp_root.join("search_index");
    std::env::set_var("AA_SEARCH_INDEX_DIR", idx_dir.to_string_lossy().as_ref());

    (tmp_root, store)
}

#[test]
fn test_hybrid_bm25_keyword_search() {
    let (tmp_root, store) = setup_with_search_index("bm25");
    let opts = search::SearchOptions {
        limit: 10,
        label_filter: None,
        min_score: 0.0,
    };
    let results = search::search_graph(&store, "handle tool", &opts).unwrap();
    assert!(!results.is_empty(), "BM25 should find 'handle tool'");
    let found = results.iter().any(|r| r.name.contains("handle_tool"));
    assert!(found, "should find handle_tool_call via BM25: {:?}",
        results.iter().map(|r| &r.name).collect::<Vec<_>>());

    let _ = fs::remove_dir_all(&tmp_root);
}

#[test]
fn test_hybrid_semantic_search() {
    let (tmp_root, store) = setup_with_search_index("semantic");
    let opts = search::SearchOptions {
        limit: 10,
        label_filter: None,
        min_score: 0.0,
    };
    // "process incoming requests" should find process_request or route_incoming
    // via TF-IDF even though the exact phrase doesn't appear
    let results = search::search_graph(&store, "process incoming requests", &opts).unwrap();
    assert!(!results.is_empty(),
        "semantic search should find results for 'process incoming requests'");

    // At minimum, we expect process_request or route_incoming
    let names: Vec<&str> = results.iter().map(|r| r.name.as_str()).collect();
    eprintln!("Semantic search results: {:?}", names);
    let has_relevant = results.iter().any(|r|
        r.name.contains("process") || r.name.contains("route") ||
        r.name.contains("handle") || r.name.contains("dispatch")
    );
    assert!(has_relevant,
        "should find semantically related symbols, got: {:?}", names);

    let _ = fs::remove_dir_all(&tmp_root);
}

#[test]
fn test_rrf_fusion_combines_rankings() {
    let (tmp_root, store) = setup_with_search_index("rrf");
    let opts = search::SearchOptions {
        limit: 10,
        label_filter: None,
        min_score: 0.0,
    };
    let results = search::search_graph(&store, "handle", &opts).unwrap();
    assert!(!results.is_empty(), "should find results for 'handle'");

    // RRF scores are in the range ~0.01-0.03 (1/(60+rank))
    // They should NOT be in the 0.7-1.0 range of the old substring scorer
    let top = &results[0];
    assert!(top.score < 0.5,
        "RRF scores should be small (< 0.5), got {}", top.score);
    assert!(top.score > 0.0,
        "RRF scores should be positive, got {}", top.score);

    let _ = fs::remove_dir_all(&tmp_root);
}

#[test]
fn test_build_search_index_creates_files() {
    let (tmp_root, _store) = setup_with_search_index("files");

    let bm25_dir = tmp_root.join("search_index/bm25");
    assert!(bm25_dir.exists(), "BM25 index directory should exist");

    let vector_file = tmp_root.join("search_index/vector_index.bin");
    assert!(vector_file.exists(), "vector index file should exist");

    let _ = fs::remove_dir_all(&tmp_root);
}
