// stage3a_integration — end-to-end test for the index → query pipeline.
//
// Creates a temporary fixture Rust project, indexes it, and verifies
// the graph contains expected nodes and edges via Cypher queries.

use ai_architect_mcp::graph_store::GraphStore;
use ai_architect_mcp::indexer;
use std::fs;

// ---------------------------------------------------------------------------
// Fixture source files — small Rust project with known structure
// ---------------------------------------------------------------------------

const FIXTURE_MAIN: &str = r#"
use std::io;

fn main() {
    println!("hello");
    helper();
}

fn helper() -> i32 {
    42
}

pub async fn serve() {
    loop {}
}
"#;

const FIXTURE_LIB: &str = r#"
pub mod models;

pub fn init() -> bool {
    true
}

pub const VERSION: &str = "1.0.0";

pub type Result<T> = std::result::Result<T, String>;
"#;

const FIXTURE_MODELS: &str = r#"
pub struct Config {
    pub name: String,
    pub max_retries: u32,
}

pub enum Status {
    Active,
    Inactive,
    Error(String),
}

pub trait Processor {
    fn process(&self, input: &str) -> String;
    fn reset(&mut self);
}

impl Config {
    pub fn new(name: String) -> Self {
        Config { name, max_retries: 3 }
    }
}
"#;

// ---------------------------------------------------------------------------
// Test
// ---------------------------------------------------------------------------

#[test]
fn test_full_pipeline_on_fixture() {
    let tmp_root = std::env::temp_dir().join(format!(
        "stage3a_integration_{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&tmp_root);

    // -- Set up fixture project --
    let fixture_dir = tmp_root.join("fixture");
    fs::create_dir_all(fixture_dir.join("src")).expect("create fixture/src");
    fs::write(fixture_dir.join("src/main.rs"), FIXTURE_MAIN).expect("write main.rs");
    fs::write(fixture_dir.join("src/lib.rs"), FIXTURE_LIB).expect("write lib.rs");
    fs::write(fixture_dir.join("src/models.rs"), FIXTURE_MODELS).expect("write models.rs");

    // -- Index the fixture --
    let graph_dir = tmp_root.join("graph");
    let result = indexer::index_codebase(
        &fixture_dir.join("src"),
        &graph_dir,
    )
    .expect("index_codebase should succeed");

    // 3 fixture files
    assert_eq!(
        result.files_indexed, 3,
        "should index exactly 3 files, got {}",
        result.files_indexed
    );
    assert!(result.node_count > 0, "should have nodes");
    assert!(result.edge_count > 0, "should have edges");

    // -- Open graph and run verification queries --
    let store = GraphStore::open_or_create(&graph_dir).expect("open graph");

    // 1. Count functions
    let qr = store
        .execute_query("MATCH (f:Function) RETURN count(f)")
        .expect("count functions");
    let fn_count: u64 = qr.rows[0][0].parse().unwrap_or(0);
    // main, helper, serve, init = 4 functions
    assert_eq!(fn_count, 4, "expected 4 functions, got {fn_count}");

    // 2. Check known struct names
    let qr = store
        .execute_query("MATCH (s:Struct) RETURN s.name ORDER BY s.name")
        .expect("struct names");
    let struct_names: Vec<&str> = qr.rows.iter().map(|r| r[0].as_str()).collect();
    assert!(
        struct_names.contains(&"Config"),
        "should find Config struct, got: {struct_names:?}"
    );

    // 3. File -> Function edges (Defines)
    let qr = store
        .execute_query(
            "MATCH (f:File)-[:Defines_File_Function]->(fn:Function) \
             RETURN f.name, fn.name ORDER BY fn.name",
        )
        .expect("file->function edges");
    let fn_names: Vec<&str> = qr.rows.iter().map(|r| r[1].as_str()).collect();
    assert!(
        fn_names.contains(&"main"),
        "should have File->Function edge for main, got: {fn_names:?}"
    );
    assert!(
        fn_names.contains(&"init"),
        "should have File->Function edge for init, got: {fn_names:?}"
    );

    // 4. Struct -> Field edges (HasField)
    let qr = store
        .execute_query(
            "MATCH (s:Struct)-[:HasField_Struct_Field]->(field:Field) \
             RETURN s.name, field.name ORDER BY field.name",
        )
        .expect("struct->field edges");
    let field_pairs: Vec<(&str, &str)> = qr
        .rows
        .iter()
        .map(|r| (r[0].as_str(), r[1].as_str()))
        .collect();
    assert!(
        field_pairs.contains(&("Config", "name")),
        "should have Config->name field edge, got: {field_pairs:?}"
    );
    assert!(
        field_pairs.contains(&("Config", "max_retries")),
        "should have Config->max_retries field edge, got: {field_pairs:?}"
    );

    // 5. Enum variants
    let qr = store
        .execute_query(
            "MATCH (e:Enum)-[:HasVariant_Enum_Variant]->(v:Variant) \
             RETURN e.name, v.name ORDER BY v.name",
        )
        .expect("enum->variant edges");
    let variant_names: Vec<&str> = qr.rows.iter().map(|r| r[1].as_str()).collect();
    assert!(
        variant_names.contains(&"Active"),
        "should have Active variant, got: {variant_names:?}"
    );
    assert!(
        variant_names.contains(&"Inactive"),
        "should have Inactive variant, got: {variant_names:?}"
    );

    // 6. Trait methods
    let qr = store
        .execute_query(
            "MATCH (t:Trait)-[:HasMethod_Trait_Method]->(m:Method) \
             RETURN t.name, m.name ORDER BY m.name",
        )
        .expect("trait->method edges");
    let method_names: Vec<&str> = qr.rows.iter().map(|r| r[1].as_str()).collect();
    assert!(
        method_names.contains(&"process"),
        "should have process method, got: {method_names:?}"
    );
    assert!(
        method_names.contains(&"reset"),
        "should have reset method, got: {method_names:?}"
    );

    // 7. Impl methods on struct
    let qr = store
        .execute_query(
            "MATCH (s:Struct)-[:HasMethod_Struct_Method]->(m:Method) \
             RETURN s.name, m.name",
        )
        .expect("struct->method edges");
    let impl_methods: Vec<(&str, &str)> = qr
        .rows
        .iter()
        .map(|r| (r[0].as_str(), r[1].as_str()))
        .collect();
    assert!(
        impl_methods.contains(&("Config", "new")),
        "should have Config::new method, got: {impl_methods:?}"
    );

    // -- Cleanup --
    let _ = fs::remove_dir_all(&tmp_root);
}
