//! Runtime-trace enrichment support (`ingest_traces` internals) and the
//! git-history bootstrap-import path (`analyze_codebase`'s incremental
//! bootstrap). Extracted from `main.rs` per issue #151
//! (Fowler: Extract Class).

use crate::artifact;
use serde_json::{json, Value};
use std::path::Path;

use crate::cochange;
use crate::graph_store;
use crate::indexer;
use crate::indexing_handlers::*;
use crate::query_handlers::*;

/// Node labels a runtime trace endpoint may resolve to (callables).
pub(crate) const CALLABLE_LABELS: &[&str] = &["Function", "Method"];

/// The callable label ("Function"/"Method") of the node with id `qn`, or `None`
/// if no such callable node exists.
pub(crate) fn callable_label(store: &graph_store::GraphStore, qn: &str) -> Option<&'static str> {
    for &label in CALLABLE_LABELS {
        let q = format!(
            "MATCH (n:{label}) WHERE n.id = {} RETURN count(n)",
            graph_store::cypher_str(qn)
        );
        if let Ok(qr) = store.execute_query(&q) {
            let n = qr
                .rows
                .first()
                .and_then(|r| r.first())
                .and_then(|c| c.parse::<u64>().ok())
                .unwrap_or(0);
            if n > 0 {
                return Some(label);
            }
        }
    }
    None
}

/// Annotates an existing static Calls edge with a runtime `observed_count`
/// (accumulating). Returns `true` if such an edge existed (and was annotated),
/// `false` if none exists (so the caller creates an OBSERVED_CALLS edge instead).
pub(crate) fn annotate_static_call(
    store: &graph_store::GraphStore,
    table: &str,
    caller: &str,
    callee: &str,
    count: i64,
) -> Result<bool, String> {
    let (from_label, to_label) = table
        .strip_prefix("Calls_")
        .and_then(|s| s.split_once('_'))
        .ok_or_else(|| format!("bad calls table {table}"))?;
    let a = graph_store::cypher_str(caller);
    let b = graph_store::cypher_str(callee);
    let set = format!(
        "MATCH (x:{from_label})-[r:{table}]->(y:{to_label}) \
         WHERE x.id = {a} AND y.id = {b} \
         SET r.observed_count = coalesce(r.observed_count, 0) + {count}"
    );
    // SET on zero matches is a no-op; detect existence separately so we know
    // whether to fall through to an OBSERVED_CALLS edge.
    let exists_q = format!(
        "MATCH (x:{from_label})-[r:{table}]->(y:{to_label}) \
         WHERE x.id = {a} AND y.id = {b} RETURN count(r)"
    );
    let exists = store
        .execute_query(&exists_q)?
        .rows
        .first()
        .and_then(|r| r.first())
        .and_then(|c| c.parse::<u64>().ok())
        .map(|n| n > 0)
        .unwrap_or(false);
    if exists {
        store.execute_query(&set)?;
    }
    Ok(exists)
}

/// Creates or accumulates an OBSERVED_CALLS edge (the runtime-only divergence).
pub(crate) fn upsert_observed_call(
    store: &graph_store::GraphStore,
    table: &str,
    caller: &str,
    callee: &str,
    count: i64,
) -> Result<(), String> {
    let (from_label, to_label) = table
        .strip_prefix("OBSERVED_CALLS_")
        .and_then(|s| s.split_once('_'))
        .ok_or_else(|| format!("bad observed table {table}"))?;
    let a = graph_store::cypher_str(caller);
    let b = graph_store::cypher_str(callee);
    let exists_q = format!(
        "MATCH (x:{from_label})-[r:{table}]->(y:{to_label}) \
         WHERE x.id = {a} AND y.id = {b} RETURN count(r)"
    );
    let exists = store
        .execute_query(&exists_q)?
        .rows
        .first()
        .and_then(|r| r.first())
        .and_then(|c| c.parse::<u64>().ok())
        .map(|n| n > 0)
        .unwrap_or(false);
    if exists {
        let set = format!(
            "MATCH (x:{from_label})-[r:{table}]->(y:{to_label}) \
             WHERE x.id = {a} AND y.id = {b} \
             SET r.observed_count = coalesce(r.observed_count, 0) + {count}"
        );
        store.execute_query(&set)?;
    } else {
        store.insert_edge(
            table,
            caller,
            callee,
            &[("observed_count", &count.to_string())],
        )?;
    }
    Ok(())
}

/// Builds the tool response for a successful incremental re-index (issue #62)
/// and runs the optional artifact export against the updated graph. The
/// response carries the {changed, added, deleted, renamed, unchanged} partition
/// plus files_reparsed and wall time, and `mode: "incremental"` so a caller can
/// tell an incremental pass from a full one at a glance.
pub(crate) fn finish_incremental_response(
    inc: indexer::IncrementalResult,
    graph_dir: &Path,
    codebase: &Path,
    manifest_path: &Path,
    want_export: bool,
    want_cochange: bool,
) -> Value {
    let mut response = json!({
        "stage": 3,
        "status": "ok",
        "tool": "index_codebase",
        "mode": "incremental",
        "graph_path": inc.graph_path.to_string_lossy(),
        "changed": inc.changed,
        "added": inc.added,
        "deleted": inc.deleted,
        "renamed": inc.renamed,
        "unchanged": inc.unchanged,
        "files_reparsed": inc.files_reparsed,
        "elapsed_ms": inc.elapsed_ms,
    });
    // Coverage (issue #57) was refreshed by the incremental pass; load + report it.
    response["coverage"] = coverage_summary_for_graph(graph_dir);
    // Temporal coupling (issue #58): EXTEND the mined aggregates with the commits
    // since the last mine, then re-derive edges. Git history is append-only, so
    // an incremental index need not re-walk the window.
    if want_cochange {
        if let Some(output_dir) = graph_dir.parent() {
            let summary =
                run_cochange(graph_dir, codebase, output_dir, cochange::Mode::Incremental);
            if !summary.is_null() {
                response["cochange"] = summary;
            }
        }
    }
    if want_export {
        // Whole-graph totals are needed only for the export sidecar; compute
        // them lazily here (a full table scan the incremental pass itself skips)
        // and surface them in the response alongside the artifact stats.
        let (node_count, edge_count) = graph_counts(graph_dir);
        response["node_count"] = json!(node_count);
        response["edge_count"] = json!(edge_count);
        let coverage_path = graph_dir.parent().map(indexer::coverage::coverage_path);
        match artifact::export_artifact(
            graph_dir,
            codebase,
            node_count,
            edge_count,
            Some(manifest_path),
            coverage_path.as_deref(),
        ) {
            Ok(stats) => {
                response["artifact_path"] = json!(stats.artifact_path.to_string_lossy());
                response["artifact_compressed_bytes"] = json!(stats.compressed_bytes);
                response["artifact_original_bytes"] = json!(stats.original_bytes);
            }
            Err(e) => {
                eprintln!("[ap] artifact export failed (incremental index succeeded): {e}");
                response["artifact_error"] = json!(e);
            }
        }
    }
    response
}

/// Outcome of a bootstrap attempt (issue #55 staleness contract).
#[derive(Debug)]
pub(crate) enum BootstrapOutcome {
    /// The snapshot was imported; this is the finished tool response.
    Imported(Value),
    /// Do a full index instead. Carries an assertable `bootstrap_skipped` note
    /// when the reason was a refused stale artifact (`None` otherwise).
    Reindex(Option<Value>),
}

/// Decides how to bootstrap from the committed artifact. Never hard-errors:
/// every non-import path logs its reason and returns `Reindex` so the caller
/// falls back to a full index EXPLICITLY (§13 — no silent path).
///
/// Evolved staleness contract (issue #62 completes #55) — the state table:
///
/// | condition | action | response |
/// |---|---|---|
/// | no artifact / unreadable sidecar | full index | `bootstrap_skipped=None` |
/// | fresh (sha == HEAD, or repo not git) | import as-is | `source=artifact_bootstrap` |
/// | stale + `accept_stale=true` | import as-is, SKIP fill | `source=artifact_bootstrap`, `stale_artifact{…}` |
/// | stale (DEFAULT) | import THEN incremental fill to the working tree | `source=artifact_bootstrap_fill`, `fill_method`, fill counts |
/// | stale, fill fails (no git diff AND no bundled manifest) | full index | `bootstrap_skipped{reason:"stale_artifact_fill_failed"}` |
/// | import fails | full index | `bootstrap_skipped=None` |
///
/// The DEFAULT for a stale artifact is now bootstrap-then-fill (it replaces the
/// old refuse-and-full-reindex): a one-commit-stale snapshot pays a cheap diff
/// fill instead of a full cold index. `accept_stale` is repurposed from "import
/// the stale snapshot anyway" to "import it AND SKIP the fill" — a deliberate
/// fast path when HEAD-accuracy is not needed. Per PR #61's no-silent-staleness
/// principle, an accepted-stale graph ALWAYS carries a `stale_artifact` report,
/// and a filled graph always carries its fill counts + method, so the response
/// states the graph's exact provenance in every case.
pub(crate) fn attempt_bootstrap(
    codebase: &Path,
    output_dir: &Path,
    graph_dir: &Path,
    manifest_path: &Path,
    accept_stale: bool,
    options: &indexer::IndexOptions,
) -> BootstrapOutcome {
    if !artifact::artifact_exists(codebase) {
        return BootstrapOutcome::Reindex(None);
    }
    let meta = match artifact::read_artifact_meta(codebase) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("[ap] artifact sidecar unreadable ({e}); running a full index");
            return BootstrapOutcome::Reindex(None);
        }
    };
    match artifact::artifact_staleness(codebase, &meta.commit) {
        // Fresh: import as-is (nothing to fill).
        None => bootstrap_import(codebase, output_dir, graph_dir, &meta, None),
        // accept_stale: import as-is, SKIP the fill, report the staleness.
        Some(info) if accept_stale => {
            bootstrap_import(codebase, output_dir, graph_dir, &meta, Some(info))
        }
        // DEFAULT: import THEN incrementally fill up to the working tree.
        Some(info) => bootstrap_import_and_fill(
            codebase,
            output_dir,
            graph_dir,
            manifest_path,
            &meta,
            info,
            options,
        ),
    }
}

/// A snapshot's archive format can be compatible while its graph columns are
/// obsolete. Refuse before publishing freshness or attempting an incremental
/// fill: only a full reparse can recover discarded Rust harness attributes.
fn import_compatible_artifact(codebase: &Path, graph_dir: &Path) -> Result<(), String> {
    artifact::import_artifact(codebase, graph_dir)?;
    let store = graph_store::GraphStore::open_or_create(graph_dir)?;
    store.require_entry_metadata()
}

/// Imports the snapshot into `graph_dir` and builds the bootstrap response
/// WITHOUT a fill (fresh, or accept_stale). On import failure, logs loudly and
/// returns `Reindex(None)`. When `stale` is set, the response carries a
/// `stale_artifact` object so a caller can never mistake an accepted-stale graph
/// for a fresh one. The bundled per-file manifest unpacked alongside the graph
/// is left in place: for a fresh graph it lets the next local `index_codebase`
/// run incrementally; for an accepted-stale graph it is the baseline the next
/// run diffs against to fill the skipped delta (so accepting stale never leaves
/// the graph permanently, silently stale).
pub(crate) fn bootstrap_import(
    codebase: &Path,
    output_dir: &Path,
    graph_dir: &Path,
    meta: &artifact::ArtifactMeta,
    stale: Option<artifact::StaleInfo>,
) -> BootstrapOutcome {
    if let Err(e) = import_compatible_artifact(codebase, graph_dir) {
        eprintln!(
            "[ap] artifact bootstrap failed ({e}); falling back to full index of {}",
            codebase.display()
        );
        return BootstrapOutcome::Reindex(None);
    }
    let meta_err = write_graph_meta(output_dir, codebase).err().inspect(|e| {
        eprintln!("[ap] graph meta sidecar write failed (bootstrap succeeded): {e}");
    });
    let (node_count, edge_count) = graph_counts(graph_dir);
    let mut resp = json!({
        "stage": 3,
        "status": "ok",
        "tool": "index_codebase",
        "source": "artifact_bootstrap",
        "graph_path": graph_dir.to_string_lossy(),
        "node_count": node_count,
        "edge_count": edge_count,
        "artifact_commit": meta.commit,
        "artifact_tool_version": meta.tool_version,
    });
    if let Some(info) = stale {
        resp["graph_state"] = json!("accepted_stale");
        resp["stale_artifact"] = serde_json::to_value(&info).unwrap_or_else(|_| json!({}));
    } else {
        resp["graph_state"] = json!("fresh");
    }
    // Coverage (issue #57): the bootstrapped graph inherits the exporter's
    // coverage sidecar (bundled in the artifact, unpacked beside the graph).
    resp["coverage"] = coverage_summary_for_graph(graph_dir);
    if let Some(e) = meta_err {
        resp["meta_write_error"] = json!(e);
    }
    BootstrapOutcome::Imported(resp)
}

/// Imports the stale snapshot THEN incrementally fills the artifact→working-tree
/// diff (issue #62 machinery). On import failure or fill failure, returns
/// `Reindex` so the caller cold-indexes; on success the response reports the fill
/// method and the {changed, added, deleted, renamed, unchanged} partition, plus
/// `graph_state:"filled_to_working_tree"`, so the graph's provenance is explicit.
#[allow(clippy::too_many_arguments)]
pub(crate) fn bootstrap_import_and_fill(
    codebase: &Path,
    output_dir: &Path,
    graph_dir: &Path,
    manifest_path: &Path,
    meta: &artifact::ArtifactMeta,
    info: artifact::StaleInfo,
    options: &indexer::IndexOptions,
) -> BootstrapOutcome {
    if let Err(e) = import_compatible_artifact(codebase, graph_dir) {
        eprintln!(
            "[ap] artifact bootstrap failed ({e}); falling back to full index of {}",
            codebase.display()
        );
        return BootstrapOutcome::Reindex(None);
    }
    // The bundled manifest (unpacked alongside the graph) is the artifact-sha
    // baseline the content-hash fallback classifies against when git can't diff.
    let imported_manifest = indexer::manifest::load(manifest_path);
    let fill: indexer::FillResult = match indexer::fill_after_bootstrap(
        codebase,
        graph_dir,
        manifest_path,
        &meta.commit,
        imported_manifest.as_ref(),
        options,
    ) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("[ap] artifact bootstrap fill failed ({e}); falling back to a full index");
            let mut note = serde_json::to_value(&info).unwrap_or_else(|_| json!({}));
            note["reason"] = json!("stale_artifact_fill_failed");
            return BootstrapOutcome::Reindex(Some(note));
        }
    };
    // `meta.json` last, as everywhere else: the fill rewrites the manifest, so a
    // sidecar written before it would name the imported manifest rather than the
    // filled one and read as a torn pair forever (fleet-watch#112 review round 4).
    let meta_err = write_graph_meta(output_dir, codebase).err().inspect(|e| {
        eprintln!("[ap] graph meta sidecar write failed (bootstrap succeeded): {e}");
    });
    let fill_method = match fill.method {
        indexer::FillMethod::GitDiff => "git_diff",
        indexer::FillMethod::ContentHash => "content_hash",
    };
    let (node_count, edge_count) = graph_counts(graph_dir);
    let mut resp = json!({
        "stage": 3,
        "status": "ok",
        "tool": "index_codebase",
        "source": "artifact_bootstrap_fill",
        "graph_state": "filled_to_working_tree",
        "graph_path": graph_dir.to_string_lossy(),
        "node_count": node_count,
        "edge_count": edge_count,
        "artifact_commit": meta.commit,
        "artifact_tool_version": meta.tool_version,
        "head_sha": info.head_sha,
        "fill_method": fill_method,
        "changed": fill.result.changed,
        "added": fill.result.added,
        "deleted": fill.result.deleted,
        "renamed": fill.result.renamed,
        "unchanged": fill.result.unchanged,
        "files_reparsed": fill.result.files_reparsed,
        "fill_elapsed_ms": fill.result.elapsed_ms,
    });
    if let Some(behind) = info.commits_behind {
        resp["artifact_commits_behind"] = json!(behind);
    }
    // Coverage (issue #57): refreshed by the fill (carry-forward + reparsed gaps).
    resp["coverage"] = coverage_summary_for_graph(graph_dir);
    if let Some(e) = meta_err {
        resp["meta_write_error"] = json!(e);
    }
    BootstrapOutcome::Imported(resp)
}

/// Mines git temporal coupling into the graph at `graph_dir` (issue #58) and
/// returns a JSON summary, or `Value::Null` when there is nothing to report (not
/// a git tree, or a best-effort failure — never fatal to the index). `Full`
/// re-mines the window; `Incremental` extends the sidecar aggregates with new
/// commits. `output_dir` holds the `cochange.json` sidecar.
pub(crate) fn run_cochange(
    graph_dir: &Path,
    codebase: &Path,
    output_dir: &Path,
    mode: cochange::Mode,
) -> Value {
    let store = match graph_store::GraphStore::open_or_create(graph_dir) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[ap] cochange: open graph failed: {e}");
            return Value::Null;
        }
    };
    let prior = if mode == cochange::Mode::Incremental {
        cochange::load_state(&cochange::state_path(output_dir))
    } else {
        None
    };
    match cochange::mine(&store, codebase, output_dir, mode, prior) {
        Ok(Some(mr)) => json!({
            "mode": mr.mode,
            "commits_scanned": mr.commits_scanned,
            "edges_written": mr.edges_written,
            "mined_to_sha": mr.mined_to_sha,
        }),
        Ok(None) => Value::Null, // not a git working tree
        Err(e) => {
            eprintln!("[ap] cochange mining failed (index succeeded): {e}");
            Value::Null
        }
    }
}

/// Reads node/edge counts from an on-disk graph. Best-effort: an open/query
/// failure yields `(0, 0)` rather than aborting the bootstrap response — the
/// counts are informational and the graph is already in place.
pub(crate) fn graph_counts(graph_dir: &Path) -> (u64, u64) {
    match graph_store::GraphStore::open_or_create(graph_dir) {
        Ok(store) => {
            let n = store.node_count().unwrap_or(0);
            let e = store.edge_count().unwrap_or(0);
            (n, e)
        }
        Err(_) => (0, 0),
    }
}
