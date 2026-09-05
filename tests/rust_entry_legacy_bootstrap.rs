//! Legacy entry metadata must be refused at every artifact bootstrap boundary.
use ai_architect_mcp::{artifact, clustering, graph_store::GraphStore, indexer};
use serde_json::{json, Value};
use std::{
    fs,
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

fn git(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn bootstrap_over_stdio(source: &Path, output: &Path, accept_stale: bool) -> Value {
    let mut child = Command::new(env!("CARGO_BIN_EXE_ai-architect-mcp-codebase"))
        .args(["--profile", "full"])
        .env_remove("AP_PROFILE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let request = json!({"jsonrpc":"2.0", "id":1, "method":"tools/call", "params":{
        "name":"index_codebase", "arguments":{
            "path":source,"output_dir":output,"cochange":false,
            "bootstrap":true,"accept_stale":accept_stale
        }
    }});
    writeln!(child.stdin.take().unwrap(), "{request}").unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let envelope: Value = serde_json::from_slice(&result.stdout).unwrap();
    let response: Value =
        serde_json::from_str(envelope["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(response["status"], "ok", "{envelope}");
    response
}

fn export_legacy_fixture(tmp: &Path) -> std::path::PathBuf {
    let repo = tmp.join("repo");
    fs::create_dir(&repo).unwrap();
    fs::write(repo.join("lib.rs"), "#[test]\nfn arbitrary_case() {}\n").unwrap();
    git(&repo, &["init", "-q"]);
    git(&repo, &["config", "user.email", "test@example.invalid"]);
    git(&repo, &["config", "user.name", "Test"]);
    git(&repo, &["add", "lib.rs"]);
    git(&repo, &["commit", "-q", "--no-gpg-sign", "-m", "initial"]);
    let indexed = tmp.join("indexed");
    fs::create_dir(&indexed).unwrap();
    let graph = indexed.join("graph");
    let manifest = indexed.join("file_manifest.json");
    let result = indexer::index_codebase(&repo, &graph).unwrap();
    indexer::write_full_manifest(&repo, &manifest, &indexer::IndexOptions::default()).unwrap();
    {
        let store = GraphStore::open_or_create(&graph).unwrap();
        store
            .execute_query("ALTER TABLE Function DROP entry_kind")
            .unwrap();
    }
    artifact::export_artifact(
        &graph,
        &repo,
        result.node_count,
        result.edge_count,
        Some(&manifest),
        None,
    )
    .unwrap();
    repo
}

fn bootstrap_rebuilds_legacy_artifact(stale: bool, accept_stale: bool) {
    let tmp = tempfile::tempdir().unwrap();
    let repo = export_legacy_fixture(tmp.path());
    if stale {
        // Unrelated source change makes the snapshot stale, leaving the original
        // annotated file unchanged: incremental fill cannot recover its marker.
        fs::write(repo.join("added.rs"), "fn later() {}\n").unwrap();
        git(&repo, &["add", "added.rs"]);
        git(&repo, &["commit", "-q", "--no-gpg-sign", "-m", "later"]);
    }
    let artifact_meta = artifact::read_artifact_meta(&repo).unwrap();
    assert_eq!(
        artifact::artifact_staleness(&repo, &artifact_meta.commit).is_some(),
        stale
    );
    let output = tmp.path().join("output");
    let response = bootstrap_over_stdio(&repo, &output, accept_stale);
    assert_ne!(response["source"], "artifact_bootstrap", "{response}");
    assert_ne!(response["source"], "artifact_bootstrap_fill", "{response}");
    let store = GraphStore::open_or_create(&output.join("graph")).unwrap();
    clustering::cluster_graph(&store, 1.0).unwrap();
    let entries = clustering::get_processes(&store).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].entry_kind, "test");
    assert!(entries[0].name.ends_with("::arbitrary_case"));
}

#[test]
fn fresh_legacy_artifact_rebuilds_before_publication() {
    bootstrap_rebuilds_legacy_artifact(false, false);
}

#[test]
fn stale_legacy_artifact_rebuilds_before_fill() {
    bootstrap_rebuilds_legacy_artifact(true, false);
}

#[test]
fn accepting_stale_does_not_accept_incompatible_entry_schema() {
    bootstrap_rebuilds_legacy_artifact(true, true);
}

fn graph_snapshot(store: &GraphStore) -> Vec<Vec<Vec<String>>> {
    [
        "MATCH (f:Function) RETURN f.id, f.name ORDER BY f.id",
        "MATCH (f:File) RETURN f.id ORDER BY f.id",
        "MATCH (f:File)-[:Defines_File_Function]->(n:Function) RETURN f.id, n.id ORDER BY f.id, n.id",
        "MATCH (f:Function)-[:Defines_Function_CallSite]->(c:CallSite) RETURN f.id, c.id ORDER BY f.id, c.id",
        "MATCH ()-[r]->() RETURN count(r)",
    ].iter().map(|query| store.execute_query(query).unwrap().rows).collect()
}

#[test]
fn direct_legacy_fill_refuses_before_mutating_graph_or_manifest() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(
        source.join("lib.rs"),
        "#[test]\nfn arbitrary_case() { helper(); }\nfn helper() {}\n",
    )
    .unwrap();
    let graph = tmp.path().join("graph");
    let manifest = tmp.path().join("file_manifest.json");
    let options = indexer::IndexOptions::default();
    indexer::index_codebase(&source, &graph).unwrap();
    indexer::write_full_manifest(&source, &manifest, &options).unwrap();
    let prior = indexer::manifest::load(&manifest).unwrap();
    let manifest_before = fs::read(&manifest).unwrap();
    let snapshot = {
        let store = GraphStore::open_or_create(&graph).unwrap();
        store
            .execute_query("ALTER TABLE Function DROP entry_kind")
            .unwrap();
        graph_snapshot(&store)
    };
    fs::write(source.join("lib.rs"), "#[test]\nfn replacement_case() {}\n").unwrap();
    let error =
        indexer::fill_after_bootstrap(&source, &graph, &manifest, "", Some(&prior), &options)
            .err()
            .expect("incompatible fill must fail before mutation");
    let store = GraphStore::open_or_create(&graph).unwrap();
    assert_eq!(
        graph_snapshot(&store),
        snapshot,
        "refused fill must preserve all prior graph entities and edges"
    );
    assert_eq!(fs::read(&manifest).unwrap(), manifest_before);
    assert!(error.contains("full reindex"), "{error}");
}
