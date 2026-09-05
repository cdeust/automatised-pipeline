//! Regression: Rust entry attributes are source evidence, not proof execution.
use ai_architect_mcp::{
    clustering,
    graph_store::GraphStore,
    indexer,
    parser::{parse_file, Language},
    resolver,
};
use std::fs;

const SOURCE: &str = r##"
fn helper() {}
fn test_helper() {}
fn helperTest() {}
#[test]
// Comments do not detach an outer attribute.
fn arbitrary_case() { helper(); }
#[kani::proof]
fn test_named_harness() { helper(); }
#[kani :: proof]
fn spaced_harness() { helper(); }
#[cfg_attr(feature = "optional", test)]
fn conditional() {}
#[other::test]
fn unrelated() {}
#[doc = "#[kani::proof]"]
fn documentation() {}
#[test]
const MARKER: bool = true;
fn after_marker() {}
mod nested {
    #[test]
    fn nested_case() {}
}
"##;

#[test]
fn parser_preserves_exact_rust_entry_attributes() {
    let parsed = parse_file(SOURCE, "src/lib.rs", Language::Rust).unwrap();
    for (name, expected) in [
        ("arbitrary_case", "test"),
        ("test_named_harness", "proof"),
        ("spaced_harness", "proof"),
        ("nested_case", "test"),
        ("helper", ""),
        ("test_helper", ""),
        ("helperTest", ""),
        ("conditional", ""),
        ("unrelated", ""),
        ("documentation", ""),
        ("after_marker", ""),
    ] {
        let node = parsed.nodes.iter().find(|n| n.name == name).unwrap();
        let actual = node
            .properties
            .iter()
            .find(|(key, _)| key == "entry_kind")
            .map(|(_, value)| value.as_str())
            .unwrap_or("");
        assert_eq!(actual, expected, "attribute classification for {name}");
    }
}

#[test]
fn indexed_attributes_create_distinct_test_and_proof_processes() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("lib.rs"), SOURCE).unwrap();
    let graph = tmp.path().join("graph");
    indexer::index_codebase(&source, &graph).unwrap();
    let store = GraphStore::open_or_create(&graph).unwrap();
    resolver::resolve_graph(&store).unwrap();
    clustering::trace_processes(&store).unwrap();
    let processes = clustering::get_processes(&store).unwrap();
    for (name, kind) in [
        ("arbitrary_case", "test"),
        ("nested_case", "test"),
        ("test_named_harness", "proof"),
        ("spaced_harness", "proof"),
    ] {
        let process = processes
            .iter()
            .find(|p| p.name.ends_with(&format!("::{name}")))
            .unwrap();
        assert_eq!(process.entry_kind, kind, "entry kind for {name}");
    }
    assert_eq!(processes.len(), 4, "only explicit test/proof entries");
    let rows = store
        .execute_query("MATCH (f:Function) WHERE f.name = 'test_named_harness' RETURN f.entry_kind")
        .unwrap();
    assert_eq!(rows.rows[0][0], "proof");
}

#[test]
fn legacy_graph_requires_full_reindex_even_for_unchanged_source() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("lib.rs"), "#[test]\nfn arbitrary_case() {}\n").unwrap();
    let graph = tmp.path().join("graph");
    indexer::index_codebase(&source, &graph).unwrap();
    let manifest = tmp.path().join("file_manifest.json");
    let options = indexer::IndexOptions::default();
    indexer::write_full_manifest(&source, &manifest, &options).unwrap();
    let prior = indexer::manifest::load(&manifest).unwrap();
    {
        let store = GraphStore::open_or_create(&graph).unwrap();
        // This is a no-op on the pre-fix schema; remove the field on fixed builds.
        let info = store
            .execute_query("CALL table_info('Function') RETURN *")
            .unwrap();
        if info
            .rows
            .iter()
            .any(|r| r.get(1).is_some_and(|v| v == "entry_kind"))
        {
            store
                .execute_query("ALTER TABLE Function DROP entry_kind")
                .unwrap();
        }
        let error = clustering::trace_processes(&store)
            .expect_err("legacy clustering must require reindex");
        assert!(error.contains("full reindex"), "{error}");
    }
    let error = indexer::index_incremental(&source, &graph, &manifest, &options, &prior)
        .err()
        .expect("unchanged legacy source must require reindex");
    assert!(error.contains("full reindex"), "{error}");
}

fn index_over_stdio(source: &std::path::Path, output: &std::path::Path) {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let mut child = Command::new(env!("CARGO_BIN_EXE_ai-architect-mcp-codebase"))
        .args(["--profile", "full"])
        .env_remove("AP_PROFILE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let request = serde_json::json!({"jsonrpc":"2.0", "id":1, "method":"tools/call", "params":{
        "name":"index_codebase", "arguments":{"path":source,"output_dir":output,"cochange":false}
    }});
    writeln!(child.stdin.take().unwrap(), "{request}").unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let envelope: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    let text = envelope["result"]["content"][0]["text"].as_str().unwrap();
    let response: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(response["status"], "ok", "{envelope}");
}

#[test]
fn stdio_index_recovers_legacy_schema_and_refreshes_removed_attribute() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    let output = tmp.path().join("output");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("lib.rs"), "#[test]\nfn arbitrary_case() {}\n").unwrap();
    index_over_stdio(&source, &output);
    let graph = output.join("graph");
    {
        let store = GraphStore::open_or_create(&graph).unwrap();
        let info = store
            .execute_query("CALL table_info('Function') RETURN *")
            .unwrap();
        if info
            .rows
            .iter()
            .any(|r| r.get(1).is_some_and(|v| v == "entry_kind"))
        {
            store
                .execute_query("ALTER TABLE Function DROP entry_kind")
                .unwrap();
        }
    }
    // Source and manifest unchanged: the handler must replace the legacy graph.
    index_over_stdio(&source, &output);
    {
        let store = GraphStore::open_or_create(&graph).unwrap();
        clustering::cluster_graph(&store, 1.0).unwrap();
        let processes = clustering::get_processes(&store).unwrap();
        assert_eq!(processes.len(), 1);
        assert_eq!(processes[0].entry_kind, "test");
    }
    fs::write(source.join("lib.rs"), "fn arbitrary_case() {}\n").unwrap();
    index_over_stdio(&source, &output);
    let store = GraphStore::open_or_create(&graph).unwrap();
    clustering::cluster_graph(&store, 1.0).unwrap();
    assert!(clustering::get_processes(&store).unwrap().is_empty());
    let rows = store
        .execute_query("MATCH (f:Function) RETURN f.entry_kind")
        .unwrap();
    assert_eq!(rows.rows[0][0], "");
}
