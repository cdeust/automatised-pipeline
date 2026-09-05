//! Stage 3 — `analyze_codebase` (index+resolve+cluster in one call),
//! Stage 3b-v2 `lsp_resolve`, and Stage 3e `detect_changes` handler logic.
//! Extracted from `main.rs` per issue #151 (Fowler: Extract Class).

use crate::epistemic;
use crate::search;
use serde_json::{json, Value};
use std::fs;
use std::path::Path;

use crate::clustering;
use crate::git_diff;
use crate::graph_cache;
use crate::graph_store;
use crate::handler_util::*;
use crate::indexer;
use crate::lsp_client;
use crate::lsp_resolver;
use crate::query_handlers::*;
use crate::resolver;

// ---------------------------------------------------------------------------
// Stage 3 — analyze_codebase (all-in-one: index + resolve + cluster)
// ---------------------------------------------------------------------------

pub(crate) fn run_analyze_codebase(arguments: &Value) -> Value {
    match do_analyze_codebase(arguments) {
        Ok(v) => v,
        Err(msg) => json!({
            "stage": 3, "status": "error", "reason": "analyze_failed", "message": msg
        }),
    }
}

/// The caller-supplied half of an `analyze_codebase` call, validated once and
/// its output directory prepared.
///
/// A parameter object (§4.4): the four settings and the three derived paths are
/// resolved together and used together by all four phases.
struct AnalyzeRequest {
    codebase: std::path::PathBuf,
    output_dir: std::path::PathBuf,
    graph_dir: std::path::PathBuf,
    options: indexer::IndexOptions,
    lang_filter: Option<crate::parser::Language>,
    gamma: f64,
    enable_lsp: bool,
}

impl AnalyzeRequest {
    /// Touches the filesystem deliberately — hence `prepare`, not `parse`:
    /// `validate_graph_path_safe` must run before the stale-artifact removal
    /// below it (source: H4 fix — see do_index_codebase).
    fn prepare(args: &serde_json::Map<String, Value>) -> Result<Self, String> {
        let path_str = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or("missing required field 'path'")?;
        let output_str = args
            .get("output_dir")
            .and_then(|v| v.as_str())
            .ok_or("missing required field 'output_dir'")?;
        let lang_filter = parse_language_filter(args)?;
        let codebase = require_absolute(path_str, "path")?;
        if !codebase.exists() {
            return Err(format!("path does not exist: {}", codebase.display()));
        }
        let output_dir = require_absolute(output_str, "output_dir")?;
        fs::create_dir_all(&output_dir).map_err(|e| format!("create output dir: {e}"))?;
        let graph_dir = output_dir.join("graph");
        validate_graph_path_safe(&graph_dir)?;
        Ok(AnalyzeRequest {
            codebase,
            output_dir,
            graph_dir,
            options: indexer::IndexOptions {
                language_filter: lang_filter,
                dependency_scope: parse_dependency_scope(args)?,
                exclude_dirs: parse_exclude_dirs(args)?,
            },
            lang_filter,
            gamma: args
                .get("resolution_param")
                .and_then(|v| v.as_f64())
                .unwrap_or(1.0),
            enable_lsp: args.get("lsp").and_then(|v| v.as_bool()).unwrap_or(false),
        })
    }
}

/// Phase 2b, optional. Graceful fallback by contract: any LSP error yields
/// `None` and the pipeline continues on the static resolution alone.
fn lsp_phase(
    req: &AnalyzeRequest,
    store: &graph_store::GraphStore,
) -> Option<crate::lsp_client::LspResolutionResult> {
    if !req.enable_lsp {
        return None;
    }
    let effective_lang = match req.lang_filter {
        Some(lang) => lang.as_str().to_string(),
        None => detect_dominant_language(&req.codebase),
    };
    lsp_resolver::resolve_with_lsp(
        store,
        &req.codebase,
        &effective_lang,
        None,
        std::time::Duration::from_secs(30),
    )
    .ok()
}

/// The four phases' counts, as one response.
fn analyze_envelope(
    index_result: &indexer::IndexResult,
    resolve_result: &resolver::ResolutionResult,
    cluster_result: &clustering::ClusteringResult,
    search_index_result: &search::SearchIndexResult,
    lsp_result: Option<&crate::lsp_client::LspResolutionResult>,
    total_ms: u64,
) -> Value {
    json!({
        "stage": 3,
        "status": "ok",
        "tool": "analyze_codebase",
        "graph_path": index_result.graph_path.to_string_lossy(),
        "index": {
            "node_count": index_result.node_count,
            "edge_count": index_result.edge_count,
            "files_indexed": index_result.files_indexed,
        },
        "resolve": {
            "total_edges": resolve_result.total_edges,
            "resolution_rate": format!("{:.2}",
                if resolve_result.total_refs > 0 {
                    resolve_result.total_edges as f64 / resolve_result.total_refs as f64
                } else { 0.0 }),
        },
        "cluster": {
            "community_count": cluster_result.communities,
            "modularity": format!("{:.6}", cluster_result.modularity),
            "process_count": cluster_result.processes,
        },
        "search_index": {
            "bm25_doc_count": search_index_result.bm25_doc_count,
            "vector_doc_count": search_index_result.vector_doc_count,
            "elapsed_ms": search_index_result.elapsed_ms,
        },
        "lsp_resolve": match lsp_result {
            Some(r) => json!({
                "resolved_count": r.resolved_count,
                "failed_count": r.failed_count,
                "skipped_count": r.skipped_count,
                "elapsed_ms": r.elapsed_ms,
            }),
            None => json!(null),
        },
        "total_elapsed_ms": total_ms,
    })
}

pub(crate) fn do_analyze_codebase(arguments: &Value) -> Result<Value, String> {
    let args = arguments.as_object().ok_or("arguments must be an object")?;
    let req = AnalyzeRequest::prepare(args)?;
    if req.graph_dir.exists() {
        // Prior run may have left a dir OR a single-file Kuzu db; remove either.
        remove_stale_graph_artifact(&req.graph_dir)?;
    }
    let total_start = std::time::Instant::now();

    // Phase 1: index, then coverage + manifest before the metadata commit point.
    let index_result =
        indexer::index_codebase_with_language(&req.codebase, &req.graph_dir, &req.options)?;
    let sidecar_err = persist_analyze_sidecars(&req, &index_result.coverage)?;
    // Phase 2: resolve, then optionally refine with an LSP.
    let store = graph_store::GraphStore::open_or_create(&index_result.graph_path)?;
    let resolve_result = resolver::resolve_graph(&store)?;
    let lsp_result = lsp_phase(&req, &store);
    // Phase 3: cluster. Phase 4: build the BM25 + TF-IDF search index.
    let cluster_result = clustering::cluster_graph(&store, req.gamma)?;
    let search_index_result = search::build_search_index(&store, &req.output_dir, &req.codebase)?;

    let mut response = analyze_envelope(
        &index_result,
        &resolve_result,
        &cluster_result,
        &search_index_result,
        lsp_result.as_ref(),
        total_start.elapsed().as_millis() as u64,
    );
    response["coverage"] = crate::indexing_handlers::coverage_summary(&index_result.coverage);
    report_sidecar_error(&mut response, sidecar_err);
    Ok(response)
}

/// Writes coverage and the manifest before `meta.json`, the commit point.
/// Coverage is replaced on every full analysis, including an empty gap map.
///
/// `analyze_codebase` used to write `meta.json` and NO manifest at all
/// (fleet-watch#112 review round 6). It and `index_codebase` are documented as
/// interchangeable entry points over the same `output_dir`, so running analyze
/// where a previous index had left a manifest froze that manifest in place:
/// every file added afterwards stayed permanently invisible to `count_dirty`,
/// and the graph reported fresh while missing them. Writing no manifest at all
/// is equally wrong in the other direction — the freshness check then has
/// nothing to compare and can never answer.
///
/// Manifest/meta writes remain best-effort and surface their error. A coverage
/// write failure propagates so a stale or missing receipt cannot be reported
/// as a successful analysis.
fn persist_analyze_sidecars(
    req: &AnalyzeRequest,
    coverage: &indexer::coverage::CoverageReport,
) -> Result<Option<String>, String> {
    // Source: dy-wcet stdio reproduction (2026-09-06): analyze discarded
    // IndexResult.coverage, leaving query_graph(graph="missed") unavailable
    // or stale. A failed write must not return a successful coverage receipt.
    indexer::coverage::save(&indexer::coverage::coverage_path(&req.output_dir), coverage)?;
    let manifest_path = indexer::manifest::manifest_path(&req.output_dir);
    if let Err(e) = indexer::write_full_manifest(&req.codebase, &manifest_path, &req.options) {
        eprintln!("[ap] file manifest write failed (analyze succeeded): {e}");
        return Ok(Some(e));
    }
    Ok(write_graph_meta(&req.output_dir, &req.codebase)
        .err()
        .inspect(|e| {
            eprintln!("[ap] graph meta sidecar write failed (analyze succeeded): {e}");
        }))
}

/// Surfaces a failed sidecar write on the response rather than leaving the
/// caller believing a complete, queryable graph landed (review round 6 finding
/// 4: three of five `write_graph_meta` call sites swallowed this).
fn report_sidecar_error(response: &mut Value, err: Option<String>) {
    if let Some(err) = err {
        response["meta_write_error"] = json!(err);
    }
}

// ---------------------------------------------------------------------------
// Stage 3e — detect_changes (git diff impact)
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Stage 3b-v2 — lsp_resolve (LSP-enhanced resolution)
// ---------------------------------------------------------------------------

pub(crate) fn run_lsp_resolve(arguments: &Value) -> Value {
    match do_lsp_resolve(arguments) {
        Ok(v) => v,
        Err(msg) => {
            // Distinguish specific failure reasons so callers can act on them.
            if msg.contains("lsp_command_not_allowed") {
                // source: C3 fix — surface the reason code plus the allowlist
                // so the caller knows which commands are accepted.
                json!({
                    "stage": 3,
                    "status": "error",
                    "reason": "lsp_command_not_allowed",
                    "message": msg,
                    "allowed": lsp_client::LSP_COMMAND_ALLOWLIST,
                })
            } else if msg.contains("lsp_not_found") {
                json!({
                    "stage": 3,
                    "status": "error",
                    "reason": "lsp_not_found",
                    "message": msg
                })
            } else if msg.contains("lsp_probe_failed") {
                // source: C-correctness bug 1 — binary on PATH but doesn't
                // speak LSP (rustup proxy, stub script, /bin/true, ...).
                // Distinct from lsp_not_found so callers can act on it.
                json!({
                    "stage": 3,
                    "status": "error",
                    "reason": "lsp_probe_failed",
                    "message": msg
                })
            } else {
                json!({
                    "stage": 3,
                    "status": "error",
                    "reason": "lsp_resolve_failed",
                    "message": msg
                })
            }
        }
    }
}

pub(crate) fn do_lsp_resolve(arguments: &Value) -> Result<Value, String> {
    let args = arguments.as_object().ok_or("arguments must be an object")?;
    let graph_str = args
        .get("graph_path")
        .and_then(|v| v.as_str())
        .ok_or("missing required field 'graph_path'")?;
    let codebase_str = args
        .get("codebase_path")
        .and_then(|v| v.as_str())
        .ok_or("missing required field 'codebase_path'")?;
    let language = args
        .get("language")
        .and_then(|v| v.as_str())
        .unwrap_or("auto");
    let lsp_command = args.get("lsp_command").and_then(|v| v.as_str());
    let timeout_ms = args
        .get("timeout_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(30000);

    let graph_path = Path::new(graph_str);
    if !graph_path.exists() {
        return Err(format!("graph_path does not exist: {graph_str}"));
    }
    let codebase_path = Path::new(codebase_str);
    if !codebase_path.exists() {
        return Err(format!("codebase_path does not exist: {codebase_str}"));
    }

    // Auto-detect language from codebase if needed
    let effective_lang = if language == "auto" {
        detect_dominant_language(codebase_path)
    } else {
        language.to_string()
    };

    let store = graph_store::GraphStore::open_or_create(graph_path)?;
    let timeout = std::time::Duration::from_millis(timeout_ms);

    let result = lsp_resolver::resolve_with_lsp(
        &store,
        codebase_path,
        &effective_lang,
        lsp_command,
        timeout,
    )?;

    Ok(json!({
        "stage": 3,
        "status": "ok",
        "tool": "lsp_resolve",
        "resolved_count": result.resolved_count,
        "failed_count": result.failed_count,
        "skipped_count": result.skipped_count,
        "elapsed_ms": result.elapsed_ms,
    }))
}

/// Detect the dominant language from file extensions in a codebase.
pub(crate) fn detect_dominant_language(path: &Path) -> String {
    let mut rs_count = 0u32;
    let mut py_count = 0u32;
    let mut ts_count = 0u32;

    if let Ok(entries) = fs::read_dir(path) {
        for entry in entries.flatten() {
            let p = entry.path();
            match p.extension().and_then(|e| e.to_str()) {
                Some("rs") => rs_count += 1,
                Some("py") => py_count += 1,
                Some("ts") | Some("tsx") => ts_count += 1,
                _ => {}
            }
        }
    }

    if rs_count >= py_count && rs_count >= ts_count {
        "rust".to_string()
    } else if py_count >= ts_count {
        "python".to_string()
    } else {
        "typescript".to_string()
    }
}

pub(crate) fn run_detect_changes(arguments: &Value) -> Value {
    match do_detect_changes(arguments) {
        Ok(v) => v,
        Err(msg) => json!({
            "stage": 3, "status": "error", "reason": "detect_changes_failed", "message": msg
        }),
    }
}

pub(crate) fn do_detect_changes(arguments: &Value) -> Result<Value, String> {
    let args = arguments.as_object().ok_or("arguments must be an object")?;
    let graph_str = args
        .get("graph_path")
        .and_then(|v| v.as_str())
        .ok_or("missing required field 'graph_path'")?;
    let diff_text = args.get("diff_text").and_then(|v| v.as_str());
    let codebase_path = args.get("codebase_path").and_then(|v| v.as_str());
    let base_ref = args
        .get("base_ref")
        .and_then(|v| v.as_str())
        .unwrap_or("HEAD~1");
    let head_ref = args
        .get("head_ref")
        .and_then(|v| v.as_str())
        .unwrap_or("HEAD");

    let graph_path = Path::new(graph_str);
    if !graph_path.exists() {
        return Err(format!("graph_path does not exist: {graph_str}"));
    }

    // Read-only tool: reuse cached handle. source: graph_cache module docs.
    let store = graph_cache::open_cached(graph_path)?;

    let analysis = if let Some(text) = diff_text {
        git_diff::analyze_diff(&store, text)?
    } else if let Some(repo) = codebase_path {
        let repo_path = Path::new(repo);
        if !repo_path.exists() {
            return Err(format!("codebase_path does not exist: {repo}"));
        }
        git_diff::analyze_git_diff(&store, repo_path, base_ref, head_ref)?
    } else {
        return Err("either 'diff_text' or 'codebase_path' must be provided".to_string());
    };

    Ok(json!({
        "stage": 3,
        "status": "ok",
        "tool": "detect_changes",
        "files_changed": analysis.files_changed,
        "symbols_affected": analysis.symbols_affected,
        "symbols_affected_count": analysis.symbols_affected.len(),
        "communities_affected": analysis.communities_affected,
        "communities_affected_count": analysis.communities_affected.len(),
        "processes_affected": analysis.processes_affected,
        "processes_affected_count": analysis.processes_affected.len(),
        "risk_score": format!("{:.4}", analysis.risk_score),
        // Epistemic qualification of risk_score: the mean confidence of the
        // reverse-dependency edges the risk rests on, and whether any changed
        // symbol's blast radius is a lower bound (true risk may exceed score).
        // source: git_diff::assess_dependency_confidence.
        "mean_dependency_confidence": format!("{:.2}", analysis.mean_dependency_confidence),
        "epistemic": analysis.epistemic,
        "epistemic_reasons": analysis.epistemic_reasons,
        "next_steps": detect_changes_next_steps(&analysis),
    }))
}

/// Suggests follow-up tools after a `detect_changes` result. Graph-grounded:
/// each hint is gated on a present dimension of the analysis.
pub(crate) fn detect_changes_next_steps(analysis: &git_diff::DiffAnalysis) -> Value {
    let mut steps = Vec::new();
    if !analysis.symbols_affected.is_empty() {
        steps.push(
            "drill into a changed symbol's blast radius: get_impact on a \
             `symbols_affected[].qualified_name`"
                .to_string(),
        );
    }
    if analysis.epistemic == epistemic::Boundary::LowerBound.as_str() {
        steps.push(
            "risk is a lower bound (see `epistemic_reasons`) — run lsp_resolve to \
             tighten dynamic-dispatch edges before trusting the score"
                .to_string(),
        );
    }
    json!(steps)
}

// ---------------------------------------------------------------------------
// Stage 4 — prepare_prd_input (bundle verified finding + graph intel)
// ---------------------------------------------------------------------------
