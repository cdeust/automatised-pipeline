// stage3b_integration — end-to-end test for the resolution pipeline.
//
// Creates a fixture Rust project with known cross-file references,
// indexes it (3a), resolves it (3b), and verifies resolution edges.

use ai_architect_mcp::graph_store::GraphStore;
use ai_architect_mcp::indexer;
use ai_architect_mcp::resolver;
use std::fs;

// ---------------------------------------------------------------------------
// Fixture source files — cross-file references for resolution testing
// ---------------------------------------------------------------------------

const FIXTURE_MAIN: &str = r#"
use crate::models::Config;
use crate::models::Processor;

fn main() {
    let cfg = Config::new("test".to_string());
    helper();
    init();
}

fn helper() -> i32 {
    42
}

fn init() -> bool {
    true
}
"#;

const FIXTURE_MODELS: &str = r#"
pub struct Config {
    pub name: String,
    pub max_retries: u32,
}

pub enum Status {
    Active,
    Inactive,
}

pub trait Processor {
    fn process(&self, input: &str) -> String;
}

impl Config {
    pub fn new(name: String) -> Self {
        Config { name, max_retries: 3 }
    }
}

impl Processor for Config {
    fn process(&self, input: &str) -> String {
        input.to_string()
    }
}
"#;

// ---------------------------------------------------------------------------
// Test
// ---------------------------------------------------------------------------

#[test]
fn test_resolution_pipeline() {
    let tmp_root = std::env::temp_dir().join(format!(
        "stage3b_integration_{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&tmp_root);

    // -- Set up fixture project --
    let fixture_dir = tmp_root.join("fixture");
    fs::create_dir_all(fixture_dir.join("src")).expect("create fixture/src");
    fs::write(fixture_dir.join("src/main.rs"), FIXTURE_MAIN).unwrap();
    fs::write(fixture_dir.join("src/models.rs"), FIXTURE_MODELS).unwrap();

    // -- Index the fixture (3a) --
    let graph_dir = tmp_root.join("graph");
    let idx_result = indexer::index_codebase(
        &fixture_dir.join("src"),
        &graph_dir,
    ).expect("index_codebase should succeed");

    assert_eq!(idx_result.files_indexed, 2);
    assert!(idx_result.node_count > 0);
    assert!(idx_result.edge_count > 0);

    // -- Resolve the graph (3b) --
    let store = GraphStore::open_or_create(&graph_dir).expect("open graph");
    let res = resolver::resolve_graph(&store).expect("resolve should succeed");

    // Should have resolved at least some imports
    assert!(
        res.imports_resolved > 0 || res.calls_resolved > 0 || res.uses_resolved > 0,
        "expected at least one resolution edge, got: imports={}, calls={}, uses={}",
        res.imports_resolved, res.calls_resolved, res.uses_resolved
    );
    assert!(res.total_edges > 0, "expected total_edges > 0");

    // -- Verify idempotency: resolve again --
    let edge_count_before = store.edge_count().expect("edge_count");
    let res2 = resolver::resolve_graph(&store).expect("second resolve");
    let edge_count_after = store.edge_count().expect("edge_count after");
    // Idempotent: same edge count after second run
    assert_eq!(
        edge_count_before, edge_count_after,
        "resolution should be idempotent: before={}, after={}",
        edge_count_before, edge_count_after
    );

    // -- Verify call-site nodes exist --
    let cs_qr = store.execute_query(
        "MATCH (cs:CallSite) RETURN count(cs)"
    ).expect("count call sites");
    let cs_count: u64 = cs_qr.rows[0][0].parse().unwrap_or(0);
    assert!(
        cs_count > 0,
        "expected call site nodes to be extracted, got {cs_count}"
    );

    // -- Verify unresolved tracking works (may be empty if all resolve) --
    // The unresolved list tracks refs that failed resolution.
    // For this small fixture, most refs should resolve.
    eprintln!(
        "resolution stats: imports={}, calls={}, impls={}, extends={}, uses={}, unresolved={}",
        res.imports_resolved, res.calls_resolved, res.impls_resolved,
        res.extends_resolved, res.uses_resolved, res.unresolved.len()
    );

    // -- Cleanup --
    let _ = fs::remove_dir_all(&tmp_root);
}

#[test]
fn test_field_type_uses_resolution() {
    let tmp_root = std::env::temp_dir().join(format!(
        "stage3b_uses_{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&tmp_root);

    let fixture_dir = tmp_root.join("fixture");
    fs::create_dir_all(fixture_dir.join("src")).unwrap();
    fs::write(fixture_dir.join("src/lib.rs"), r#"
pub struct Inner {
    pub value: i32,
}

pub struct Outer {
    pub child: Inner,
    pub name: String,
}
"#).unwrap();

    let graph_dir = tmp_root.join("graph");
    indexer::index_codebase(
        &fixture_dir.join("src"),
        &graph_dir,
    ).expect("index");

    let store = GraphStore::open_or_create(&graph_dir).expect("open");
    let res = resolver::resolve_graph(&store).expect("resolve");

    // The `child: Inner` field should create a Uses_Field_Struct edge
    let qr = store.execute_query(
        "MATCH (f:Field)-[r:Uses_Field_Struct]->(s:Struct) \
         RETURN f.id, s.name"
    ).expect("query uses edges");

    assert!(
        !qr.rows.is_empty(),
        "expected Uses_Field_Struct edge for child: Inner, got none. \
         Resolution stats: uses={}", res.uses_resolved
    );

    let _ = fs::remove_dir_all(&tmp_root);
}

// Regression test for issue #1:
//   "analyze_codebase fails with 'unknown relationship type:
//    Uses_Field_TypeAlias' on real Rust codebases"
//
// Before the fix, indexing a struct whose field's type is a type alias
// caused the resolver to emit a Uses_Field_TypeAlias edge, which was
// never declared in the schema (graph_store.rs), in the indexer KNOWN
// whitelist (indexer.rs), or in the main.rs dispatch table. The whole
// analyze_codebase call aborted with status="error".
#[test]
fn test_field_type_alias_uses_resolution() {
    let tmp_root = std::env::temp_dir().join(format!(
        "stage3b_uses_typealias_{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&tmp_root);

    let fixture_dir = tmp_root.join("fixture");
    fs::create_dir_all(fixture_dir.join("src")).unwrap();
    fs::write(fixture_dir.join("src/lib.rs"), r#"
pub type Payload = Vec<u8>;

pub struct Envelope {
    pub body: Payload,
    pub label: String,
}
"#).unwrap();

    let graph_dir = tmp_root.join("graph");
    indexer::index_codebase(
        &fixture_dir.join("src"),
        &graph_dir,
    ).expect("index");

    let store = GraphStore::open_or_create(&graph_dir).expect("open");
    let res = resolver::resolve_graph(&store).expect("resolve");

    // The `body: Payload` field must produce a Uses_Field_TypeAlias edge.
    // If the schema / indexer / main.rs dispatch don't register that
    // edge table, resolve_graph returns an error before this point.
    let qr = store.execute_query(
        "MATCH (f:Field)-[r:Uses_Field_TypeAlias]->(t:TypeAlias) \
         RETURN f.id, t.name"
    ).expect("query Uses_Field_TypeAlias edges");

    assert!(
        !qr.rows.is_empty(),
        "expected Uses_Field_TypeAlias edge for body: Payload, got none. \
         Resolution stats: uses={}", res.uses_resolved
    );

    let _ = fs::remove_dir_all(&tmp_root);
}
