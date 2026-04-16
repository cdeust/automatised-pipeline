// stage4_integration — end-to-end test for prepare_prd_input.
//
// Builds a small Rust fixture, runs the full stage-3 pipeline (index →
// resolve → cluster → search index), stages a fake verified finding on
// disk, invokes stage 4, and asserts the artifact's shape.

use ai_architect_mcp::clustering;
use ai_architect_mcp::graph_store::GraphStore;
use ai_architect_mcp::indexer;
use ai_architect_mcp::prd_input::{self, PrdInputArgs};
use ai_architect_mcp::resolver;
use ai_architect_mcp::search;
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;

const FIXTURE_MAIN: &str = r#"
fn main() {
    handle_tool_call("probe");
}

fn handle_tool_call(name: &str) -> String {
    let resolved = resolve_name(name);
    format!("{}", resolved)
}

fn resolve_name(name: &str) -> String {
    name.to_string()
}

pub struct Tool {
    pub name: String,
}
"#;

fn tmp(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("stage4_{tag}_{}", std::process::id()))
}

fn build_fixture_graph(fixture_dir: &std::path::Path, graph_dir: &std::path::Path) {
    fs::create_dir_all(fixture_dir.join("src")).unwrap();
    fs::write(fixture_dir.join("src/main.rs"), FIXTURE_MAIN).unwrap();
    let result = indexer::index_codebase(&fixture_dir.join("src"), graph_dir)
        .expect("index");
    assert!(result.node_count > 0);
    let store = GraphStore::open_or_create(graph_dir).unwrap();
    let _ = resolver::resolve_graph(&store);
    let _ = clustering::cluster_graph(&store, 1.0);
    let output_dir = graph_dir.parent().unwrap();
    let _ = search::build_search_index(&store, output_dir);
}

fn stage_fake_verified(output_dir: &std::path::Path, run_id: &str, finding_id: &str) {
    let finding_dir = output_dir
        .join("runs")
        .join(run_id)
        .join("findings")
        .join(finding_id);
    fs::create_dir_all(&finding_dir).unwrap();

    // stage-1.refined.json — the full refined artifact (stages/stage-1.md §4.2).
    let refined = json!({
        "extracted": {
            "finding_id": finding_id,
            "title": "handle tool call should reject unknown tools",
            "description": "The handle_tool_call function must reject unknown tool names before dispatch.",
            "source_url": null,
            "relevance_category": "bug",
            "relevance_score": 0.9,
            "raw_data": null,
            "extracted_at": "2026-04-11T00:00:00Z",
            "extractor_version": "1.0.0",
            "source_form": "inline",
            "source_path": null
        },
        "refined_prompt": {
            "text": "Investigate handle_tool_call and reject unknown tools.",
            "role_hint": "engineer",
            "token_estimate": 42
        },
        "refinement": {
            "added_context": [],
            "orchestrator_version": "1.0.0",
            "refined_at": "2026-04-11T00:00:01Z"
        }
    });
    fs::write(
        finding_dir.join("stage-1.refined.json"),
        serde_json::to_vec_pretty(&refined).unwrap(),
    )
    .unwrap();

    // stage-2.verified.json with verified: true.
    let verified = json!({
        "run_id": run_id,
        "finding_id": finding_id,
        "verified": true,
        "verified_kind": {
            "schema_ok": true,
            "completeness_ok": true,
            "user_acknowledged": true
        },
        "finalized_at": "2026-04-11T00:01:00Z",
        "stage1_refined_path": format!("findings/{}/stage-1.refined.json", finding_id),
        "session_path": format!("findings/{}/stage-2.session.json", finding_id),
        "transcript_digest": "0".repeat(64),
        "digest_algorithm": "sha256",
        "transcript_bytes_at_finalize": 0,
        "turn_count": 0,
        "verifier_version": "1.0.0",
        "completeness_checklist": {}
    });
    fs::write(
        finding_dir.join("stage-2.verified.json"),
        serde_json::to_vec_pretty(&verified).unwrap(),
    )
    .unwrap();

    // index.json with a stub entry — stage 4 only adds top-level markers.
    let index = json!({
        "run_id": run_id,
        "started_at": "2026-04-11T00:00:00Z",
        "last_updated_at": "2026-04-11T00:01:00Z",
        "findings": {
            finding_id: {
                "artifact_path": format!("findings/{}/stage-1.refined.json", finding_id),
                "extractor_version": "1.0.0",
                "verified": true
            }
        }
    });
    fs::write(
        output_dir
            .join("runs")
            .join(run_id)
            .join("index.json"),
        serde_json::to_vec_pretty(&index).unwrap(),
    )
    .unwrap();
}

#[test]
fn test_prepare_prd_input_end_to_end() {
    let root = tmp("e2e");
    let _ = fs::remove_dir_all(&root);

    let fixture_dir = root.join("fixture");
    let graph_dir = root.join("graph");
    build_fixture_graph(&fixture_dir, &graph_dir);

    let output_dir = root.join("out");
    let run_id = "run-abc";
    let finding_id = "f-001";
    stage_fake_verified(&output_dir, run_id, finding_id);

    let args = PrdInputArgs {
        run_id: run_id.to_string(),
        finding_id: finding_id.to_string(),
        output_dir: output_dir.clone(),
        graph_path: graph_dir.clone(),
    };
    let outcome = prd_input::prepare(&args, "2026-04-11T00:02:00Z".into())
        .expect("prepare must succeed");

    assert!(outcome.artifact_path.exists(), "artifact must be on disk");

    let raw = fs::read_to_string(&outcome.artifact_path).unwrap();
    let v: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(v["run_id"], run_id);
    assert_eq!(v["finding_id"], finding_id);
    assert_eq!(v["preparer_version"], "1.0.0");
    assert!(v["prd_context"]["finding_summary"]
        .as_str()
        .unwrap()
        .contains("handle_tool_call"));

    // matched_symbols should contain at least handle_tool_call.
    let ms = v["prd_context"]["matched_symbols"].as_array().unwrap();
    assert!(
        ms.iter()
            .any(|m| m["name"].as_str() == Some("handle_tool_call")),
        "expected handle_tool_call in matched_symbols; got {:?}",
        ms.iter().map(|m| m["name"].clone()).collect::<Vec<_>>()
    );

    // index.json must have stage4 markers.
    let idx_raw = fs::read_to_string(
        output_dir
            .join("runs")
            .join(run_id)
            .join("index.json"),
    )
    .unwrap();
    let idx: Value = serde_json::from_str(&idx_raw).unwrap();
    assert_eq!(idx["stage4_prepared_at"], "2026-04-11T00:02:00Z");
    assert!(idx["stage4_path"]
        .as_str()
        .unwrap()
        .ends_with("stage-4.prd_input.json"));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_prepare_prd_input_rejects_unverified() {
    let root = tmp("unverified");
    let _ = fs::remove_dir_all(&root);

    let fixture_dir = root.join("fixture");
    let graph_dir = root.join("graph");
    build_fixture_graph(&fixture_dir, &graph_dir);

    let output_dir = root.join("out");
    let run_id = "run-xyz";
    let finding_id = "f-002";
    let finding_dir = output_dir
        .join("runs")
        .join(run_id)
        .join("findings")
        .join(finding_id);
    fs::create_dir_all(&finding_dir).unwrap();
    // verified: false -> must reject.
    let verified = json!({
        "verified": false,
        "finalized_at": "2026-04-11T00:01:00Z",
        "stage1_refined_path": format!("findings/{}/stage-1.refined.json", finding_id)
    });
    fs::write(
        finding_dir.join("stage-2.verified.json"),
        serde_json::to_vec_pretty(&verified).unwrap(),
    )
    .unwrap();

    let args = PrdInputArgs {
        run_id: run_id.to_string(),
        finding_id: finding_id.to_string(),
        output_dir: output_dir.clone(),
        graph_path: graph_dir.clone(),
    };
    let err = prd_input::prepare(&args, "2026-04-11T00:02:00Z".into())
        .err()
        .expect("must reject unverified finding");
    assert!(
        err.contains("stage_2_not_verified"),
        "expected stage_2_not_verified, got {err}"
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_prepare_prd_input_missing_stage2() {
    let root = tmp("missing");
    let _ = fs::remove_dir_all(&root);

    let fixture_dir = root.join("fixture");
    let graph_dir = root.join("graph");
    build_fixture_graph(&fixture_dir, &graph_dir);

    let output_dir = root.join("out");
    let args = PrdInputArgs {
        run_id: "run-missing".to_string(),
        finding_id: "f-missing".to_string(),
        output_dir: output_dir.clone(),
        graph_path: graph_dir.clone(),
    };
    let err = prd_input::prepare(&args, "2026-04-11T00:02:00Z".into())
        .err()
        .expect("must reject missing stage2");
    assert!(err.contains("stage_2_not_verified"), "got {err}");

    let _ = fs::remove_dir_all(&root);
}
