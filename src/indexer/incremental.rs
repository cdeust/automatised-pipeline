// incremental — changed-files-only re-indexing (issue #62).
//
// Layer: application/use-case within the indexer. This module owns the *policy*
// of "index only what changed": it classifies the current file tree against the
// persisted manifest (`super::manifest`), purges the graph nodes of the files
// that changed/vanished, re-parses only those files through the same
// walk→parse→persist machinery the full index uses, and re-derives the
// light-link edges out of them. It depends inward on `graph_store` (the store
// port), `parser`, and its sibling indexer submodules; nothing depends on it
// except the composition root (`do_index_codebase`) and its tests.
//
// Reference: DeusData/codebase-memory-mcp `src/pipeline/pipeline_incremental.c`.
// Their design, adapted to AP's store and improved:
//   * Classify by (mtime, size) against a stored per-file manifest — added /
//     modified / deleted / unchanged (their `classify_files` +
//     `find_deleted_files`). AP mirrors this on the hot path.
//   * The C reference purges a changed file's nodes INCLUDING its file node,
//     then snapshots inbound cross-file edges before the purge so cross-file
//     references survive (their `incr_capture_inbound_edge`). AP improves on
//     this for the common case: a *modified* file keeps its File node (its id —
//     the repo-relative path — is stable across an edit), so every inbound
//     File→File / Dir→File edge survives structurally with no snapshot needed.
//     Only the file's *symbols* are purged and re-parsed. AP still snapshots
//     inbound cross-file edges into a modified file's *symbols* (the case the C
//     reference targets) so a resolved graph's cross-file edges are preserved.
//   * Rename detection: the C reference has NONE (a rename is delete+add). AP's
//     documented improvement — the manifest stores a content hash per file, so
//     a (deleted, added) pair with an identical hash is reported as a rename.
//     The store makes an in-place primary-key rewrite unavailable (lbug/Kuzu
//     forbid SET on a PK), so a rename is executed as purge-old + parse-new
//     (equivalent to a full index of the renamed tree) but REPORTED as a
//     rename in the response counts.

use crate::graph_store::{cypher_str, GraphStore, REL_TABLES};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

use super::coverage::{self, CoverageCollector, FileCoverage};
use super::manifest::{self, FileManifest, FileState};
use super::persist::ParseOutcome;
use super::walk::{collect_source_files, is_dependency_path, DependencyScope, WalkOptions};
use super::{light_link, persist, relative_path, IncrementalResult, IndexOptions, SymbolBatch};
use std::collections::BTreeMap;

// Every symbol the parser emits carries a file-scoped qualified-name id
// (`<rel_path>::…`): the top-level extraction scope is the file path and nested
// scopes recurse through it, while CallSite/Import/Module ids are all
// `caller_qn::…` or `qual(scope, …)` — verified across the language extractors.
// So `starts_with(id, "<rel>::")` selects exactly one file's symbols across
// every symbol label (Module, Function, Method, Struct, Enum, Variant, Trait,
// Field, Constant, TypeAlias, Import, CallSite). `File` and `Directory` are
// keyed by the bare path (no `::`), so the same predicate never matches them —
// which is why the purge can run label-agnostically (see `purge_file_symbols`).

/// The cross-file link tables re-derived from a modified file's own text on
/// re-parse. Their OUTBOUND edges from a changed file are purged and rebuilt;
/// their INBOUND edges into a changed file survive because the File node is kept.
const LIGHT_LINK_TABLES: &[&str] = &["Imports_File_File", "References_File_File"];

/// A file discovered on disk during the incremental scan, with the cheap change
/// signal (mtime, size) already read.
#[derive(Debug, Clone)]
struct Discovered {
    /// Repo-relative, forward-slash id — identical to the File node's `id`.
    rel: String,
    abs: PathBuf,
    mtime_ns: i64,
    size: u64,
}

/// A rename: the old file's nodes are purged, the new file is parsed fresh.
/// The pair is keyed on an identical content hash at detection time; the hash
/// itself is not retained past classification.
#[derive(Debug, Clone)]
struct Rename {
    old_rel: String,
    new_file: Discovered,
}

/// The set of files to act on, independent of HOW they were classified
/// (manifest diff or git diff). This is exactly what `apply_changes` consumes.
#[derive(Debug, Default)]
struct ChangeSet {
    /// Modified files: File node kept, symbols re-parsed.
    changed: Vec<Discovered>,
    /// New files: inserted from scratch.
    added: Vec<Discovered>,
    /// Files gone from disk: File node + symbols purged.
    deleted: Vec<String>,
    /// Rename pairs (old purged, new parsed), reported distinctly.
    renamed: Vec<Rename>,
}

/// The classification of the current tree against the prior manifest: the
/// `ChangeSet` to apply, plus the unchanged set (for the count) and the manifest
/// to persist after the pass succeeds.
struct Plan {
    change_set: ChangeSet,
    /// Files whose (mtime, size) — or content hash — is unchanged: no work.
    unchanged: Vec<Discovered>,
    /// The manifest for the current tree, written on success. Carries every
    /// current file's next state (hashes reused for unchanged files).
    next_manifest: FileManifest,
}

/// Runs a changed-files-only re-index of `codebase` into the existing graph at
/// `graph_dir`, using `prior` (the loaded manifest) as the change baseline.
///
/// Preconditions: `graph_dir` is an existing, openable graph previously built by
/// a full index of (an ancestor state of) `codebase`; `manifest_path` is where
/// the refreshed manifest is written; `prior` is the manifest that graph was
/// left with. Postconditions on `Ok`: the graph equals what a full index of the
/// current `codebase` tree would produce at the index stage (File/Directory +
/// symbol nodes, structural + light-link edges), the manifest at
/// `manifest_path` reflects the current tree, and the returned counts sum the
/// files by class. Invariant preserved across the pass: for every unchanged
/// file, none of its nodes or edges are touched (asserted by the integration
/// test's per-file count check).
pub fn index_incremental(
    codebase: &Path,
    graph_dir: &Path,
    manifest_path: &Path,
    options: &IndexOptions,
    prior: &FileManifest,
) -> Result<IncrementalResult, String> {
    let start = Instant::now();
    let store = GraphStore::open_or_create(graph_dir)?;
    store.require_entry_metadata()?;
    // No create_schema() here: the graph already exists (this path is reached
    // only when a prior full index built it with the current schema), and the
    // DDL pass is ~0.4s of pure fixed cost that would defeat the whole point of
    // an incremental re-index. A schema mismatch is instead the caller's cue to
    // pass `full: true` (documented on the tool). source: measured — skipping
    // the redundant CREATE TABLE IF NOT EXISTS pass is the single largest
    // incremental speedup on small change sets.

    let dependency_scope = options.dependency_scope;
    let walk_opts = WalkOptions {
        language_filter: options.language_filter,
        dependency_scope,
        exclude_dirs: options.exclude_dirs.clone(),
    };
    let (current, walk_gaps) = discover(codebase, walk_opts)?;
    let plan = classify(prior, &current);

    let (reparsed, reparsed_gaps) = apply_changes(
        &store,
        codebase,
        &plan.change_set,
        &current,
        dependency_scope,
    )?;

    // Persist the refreshed manifest (built by classify, hashes reused).
    manifest::save(manifest_path, &plan.next_manifest)?;

    // Coverage (issue #57/#249): carry forward unchanged files' gaps, overlay
    // the freshly-reparsed files' gaps (clearing any that are now clean), plus
    // the walk-level excluded/unreadable directories discovered this pass,
    // save.
    let mut merged_gaps = reparsed_gaps;
    merged_gaps.extend(walk_gaps);
    save_incremental_coverage(
        graph_dir,
        &current,
        &plan.change_set,
        merged_gaps,
        "incremental",
    );

    // Intentionally NO node_count()/edge_count() here — see `IncrementalResult`.
    Ok(IncrementalResult {
        graph_path: graph_dir.to_path_buf(),
        changed: plan.change_set.changed.len() as u64,
        added: plan.change_set.added.len() as u64,
        deleted: plan.change_set.deleted.len() as u64,
        renamed: plan.change_set.renamed.len() as u64,
        unchanged: plan.unchanged.len() as u64,
        files_reparsed: reparsed,
        elapsed_ms: start.elapsed().as_millis() as u64,
    })
}

/// Applies a classified `ChangeSet` to the open graph: snapshot inbound
/// cross-file edges into modified files, purge the changed/deleted/renamed-old
/// nodes, re-parse the changed/added/renamed-new files, prune orphaned
/// directories, re-derive light-link edges out of the re-parsed files, and
/// re-link the snapshot. Returns the number of files re-parsed
/// (changed + added + renamed-new).
///
/// This is the shared core of both incremental entry points: the local
/// manifest-driven re-index (`index_incremental`) and the artifact bootstrap
/// fill (`fill_after_bootstrap`), which differ only in HOW they classify.
/// `all_current` is the full current file discovery, used so a re-parsed file's
/// light links resolve against every File node that exists, not just the changed
/// ones.
///
/// Postcondition: the graph equals what a full index of the current tree would
/// produce at the index stage, given a correct `changes` classification.
/// Invariant: unchanged files' nodes and edges are never touched.
fn apply_changes(
    store: &GraphStore,
    codebase: &Path,
    changes: &ChangeSet,
    all_current: &[Discovered],
    dependency_scope: DependencyScope,
) -> Result<(u64, BTreeMap<String, FileCoverage>), String> {
    // Coverage gaps for the reparsed files only (issue #57). The caller merges
    // this with the carried-forward coverage of unchanged files.
    let mut collector = CoverageCollector::default();
    // The set of files whose nodes are being purged-and-reparsed. Used both to
    // scope the inbound-edge snapshot (source must be OUTSIDE this set) and to
    // re-run light-linking only for these sources.
    let mut reparsed_set: HashSet<String> = HashSet::new();
    for d in &changes.changed {
        reparsed_set.insert(d.rel.clone());
    }
    for d in &changes.added {
        reparsed_set.insert(d.rel.clone());
    }
    for r in &changes.renamed {
        reparsed_set.insert(r.new_file.rel.clone());
    }

    // ---- 1. Snapshot inbound cross-file edges into MODIFIED files' symbols --
    // (Deleted/renamed-old files are NOT snapshotted: their inbound edges must
    // die, exactly as a full re-index would drop an edge to a vanished target.)
    let changed_rels: Vec<&str> = changes.changed.iter().map(|d| d.rel.as_str()).collect();
    let saved_edges = snapshot_inbound_edges(store, &changed_rels, &reparsed_set)?;

    // ---- 2. Purge -----------------------------------------------------------
    for d in &changes.changed {
        purge_file_symbols(store, &d.rel)?;
        purge_outbound_light_links(store, &d.rel)?;
        purge_file_content(store, &d.rel)?;
    }
    for rel in &changes.deleted {
        purge_file_symbols(store, rel)?;
        purge_file_node(store, rel)?;
        purge_file_content(store, rel)?;
    }
    for r in &changes.renamed {
        purge_file_symbols(store, &r.old_rel)?;
        purge_file_node(store, &r.old_rel)?;
        purge_file_content(store, &r.old_rel)?;
    }

    // ---- 3. Re-parse changed/added/renamed-new files ------------------------
    // Seed the "already inserted" directory set with the Directory nodes the
    // graph already holds, so inserting a NEW file under an existing directory
    // does not try to re-CREATE that directory (a primary-key violation). Only
    // genuinely new directories are created, matching a full index.
    let mut dir_nodes_inserted: HashSet<PathBuf> = existing_directory_ids(store)?;
    for d in &changes.changed {
        let outcome = reparse_modified_file(store, codebase, d, dependency_scope)?;
        record_reparse_outcome(&mut collector, &d.rel, outcome);
    }
    for d in &changes.added {
        let outcome = reparse_new_file(
            store,
            codebase,
            d,
            dependency_scope,
            &mut dir_nodes_inserted,
        )?;
        record_reparse_outcome(&mut collector, &d.rel, outcome);
    }
    for r in &changes.renamed {
        let outcome = reparse_new_file(
            store,
            codebase,
            &r.new_file,
            dependency_scope,
            &mut dir_nodes_inserted,
        )?;
        record_reparse_outcome(&mut collector, &r.new_file.rel, outcome);
    }

    // A deletion or rename can empty a directory. A full index never creates a
    // Directory node with no descendant file, so prune any that were orphaned to
    // keep the graph identical to a from-scratch index of the current tree.
    if !changes.deleted.is_empty() || !changes.renamed.is_empty() {
        prune_orphan_directories(store)?;
    }

    // ---- 4. Re-derive light-link edges OUT of the re-parsed files -----------
    // Resolve against the full current file set so a changed file can still link
    // to an unchanged target's File node.
    let all_files: Vec<PathBuf> = all_current.iter().map(|d| d.abs.clone()).collect();
    let reparsed_files: Vec<PathBuf> = all_current
        .iter()
        .filter(|d| reparsed_set.contains(&d.rel))
        .map(|d| d.abs.clone())
        .collect();
    if !reparsed_files.is_empty() {
        match light_link::link_file_imports_for(store, codebase, &reparsed_files, &all_files) {
            Ok(_) => {}
            Err(e) => eprintln!("incremental: light-link pass skipped: {e}"),
        }
    }

    // ---- 4b. Re-derive IaC nodes/edges OUT of the re-parsed files -----------
    // (issue #63 incremental integration). The changed files' stale `<rel>::`
    // IaC nodes were already reclaimed by `purge_file_symbols` above (they share
    // the symbol id-prefix), so this re-emits from the current text and resolves
    // references against every File node — exactly like the light-link step. IaC
    // reference edges target stable File nodes, so an unchanged referencer's edge
    // into a reparsed file is not disturbed. Parse gaps fold into the collector
    // so the coverage sidecar carries the IaC honesty signal too.
    if !reparsed_files.is_empty() {
        match super::iac::run_iac_pass_for(store, codebase, &reparsed_files, &all_files) {
            Ok(gaps) => super::fold_iac_gaps(&mut collector, gaps),
            Err(e) => eprintln!("incremental: IaC pass skipped: {e}"),
        }
    }

    // ---- 5. Re-link the snapshotted inbound cross-file edges -----------------
    relink_inbound_edges(store, &saved_edges)?;

    Ok((reparsed_set.len() as u64, collector.into_files()))
}

/// Records one reparsed file's outcome into the coverage collector, logging the
/// non-clean cases. Unlike the full-index recorder, `files_indexed` is not
/// tracked here (the incremental caller counts the whole current tree).
fn record_reparse_outcome(collector: &mut CoverageCollector, rel: &str, outcome: ParseOutcome) {
    match outcome {
        ParseOutcome::Indexed => {}
        ParseOutcome::Partial(ranges) => collector.record_partial(rel, ranges),
        ParseOutcome::Skipped(reason) => {
            eprintln!("incremental: skipping {rel}: {reason}");
            collector.record_skipped(rel, reason);
        }
        ParseOutcome::Quarantined(reason) => {
            eprintln!("incremental: QUARANTINED {rel}: {reason} (isolated; index continues)");
            collector.record_quarantined(rel, reason);
        }
    }
}

/// Builds the next coverage report by carrying forward `prior` gaps for files
/// that were NOT reparsed (and still exist), overlaid with the fresh gaps for
/// the reparsed files. A reparsed file that is now clean simply has no fresh
/// entry, so its stale gap is DROPPED — this is how an incomplete/quarantined
/// file clears its flag once it becomes parseable (issue #57 item 5). Deleted
/// and renamed-old files are absent from `current_rels`, so their gaps drop too.
fn merge_coverage(
    prior: Option<&coverage::CoverageReport>,
    reparsed_gaps: BTreeMap<String, FileCoverage>,
    reparsed_rels: &HashSet<String>,
    current_rels: &HashSet<String>,
    index_mode: &str,
    files_indexed: u64,
) -> coverage::CoverageReport {
    let mut report = coverage::CoverageReport::new(index_mode, files_indexed);
    // Carry forward prior gaps for files that still exist and were not reparsed.
    if let Some(prior) = prior {
        for (rel, cov) in &prior.files {
            if current_rels.contains(rel) && !reparsed_rels.contains(rel) {
                report.files.insert(rel.clone(), cov.clone());
            }
        }
    }
    // Overlay fresh gaps for the reparsed files (clean reparsed files add nothing).
    for (rel, cov) in reparsed_gaps {
        report.files.insert(rel, cov);
    }
    // files_indexed = files with a covered surface = total minus not-indexed
    // (skipped/quarantined). Parse-partial files WERE indexed, so they count.
    let (_partial, skipped, quarantined) = report.counts();
    report.files_indexed = files_indexed.saturating_sub(skipped + quarantined);
    report
}

/// Builds and writes the coverage sidecar for an incremental pass or bootstrap
/// fill: carry forward unchanged files' gaps, overlay the reparsed files' fresh
/// gaps, drop gaps for vanished/now-clean files. Best-effort — a failed write
/// only degrades the honesty signal, never the index.
fn save_incremental_coverage(
    graph_dir: &Path,
    current: &[Discovered],
    changes: &ChangeSet,
    reparsed_gaps: BTreeMap<String, FileCoverage>,
    index_mode: &str,
) {
    let output_dir = match graph_dir.parent() {
        Some(p) => p,
        None => return,
    };
    let cov_path = coverage::coverage_path(output_dir);
    let prior = coverage::load(&cov_path);
    let current_rels: HashSet<String> = current.iter().map(|d| d.rel.clone()).collect();
    let mut reparsed_rels: HashSet<String> = HashSet::new();
    for d in &changes.changed {
        reparsed_rels.insert(d.rel.clone());
    }
    for d in &changes.added {
        reparsed_rels.insert(d.rel.clone());
    }
    for r in &changes.renamed {
        reparsed_rels.insert(r.new_file.rel.clone());
    }
    let report = merge_coverage(
        prior.as_ref(),
        reparsed_gaps,
        &reparsed_rels,
        &current_rels,
        index_mode,
        current.len() as u64,
    );
    if let Err(e) = coverage::save(&cov_path, &report) {
        eprintln!("[ap] coverage sidecar write failed: {e}");
    }
}

/// Builds the manifest for a freshly full-indexed `codebase` and writes it to
/// `manifest_path`, so the NEXT `index_codebase` call can run incrementally.
///
/// Preconditions: `codebase` is the tree just indexed with the same
/// `options` (`language_filter`/`dependency_scope`/`exclude_dirs`);
/// `manifest_path`'s parent exists. Postcondition on `Ok`: the manifest
/// records (mtime, size, content_hash) for every file the full index visited,
/// keyed by the File node id. Best-effort per file: a stat/read failure
/// records a zeroed/empty state rather than aborting the whole index (the
/// file simply re-classifies as changed next time).
///
/// Walk-level gaps (excluded/unreadable directories, issue #249) are
/// discarded here — the caller (a just-completed full index) already wrote
/// the coverage sidecar from its own walk; this re-walk exists only to build
/// the manifest baseline for the NEXT incremental call.
pub fn write_full_manifest(
    codebase: &Path,
    manifest_path: &Path,
    options: &IndexOptions,
) -> Result<(), String> {
    let walk_opts = WalkOptions {
        language_filter: options.language_filter,
        dependency_scope: options.dependency_scope,
        exclude_dirs: options.exclude_dirs.clone(),
    };
    let (current, _walk_gaps) = discover(codebase, walk_opts)?;
    let mut m = FileManifest::new();
    for d in &current {
        m.files.insert(
            d.rel.clone(),
            FileState {
                mtime_ns: d.mtime_ns,
                size: d.size,
                content_hash: manifest::hash_file(&d.abs).unwrap_or_default(),
            },
        );
    }
    manifest::save(manifest_path, &m)
}

// ---------------------------------------------------------------------------
// Artifact bootstrap fill (issue #55 completion)
// ---------------------------------------------------------------------------

/// How the bootstrap fill classified the diff — surfaced in the tool response so
/// the caller always knows which signal drove the fill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FillMethod {
    /// `git diff <artifact_sha> <working tree>` — the precise, rename-aware path.
    GitDiff,
    /// Manifest content-hash classification — the fallback when git cannot diff
    /// (not a git tree, or the artifact sha is unknown to this clone).
    ContentHash,
}

/// The result of an artifact-bootstrap incremental fill: the same change
/// partition an incremental re-index reports, plus the method used.
pub struct FillResult {
    pub result: IncrementalResult,
    pub method: FillMethod,
}

/// Fills the just-bootstrapped graph at `graph_dir` (imported from an artifact
/// exported at `artifact_sha`) up to the current working tree, re-parsing only
/// the files that changed since `artifact_sha`.
///
/// Preconditions: `graph_dir` holds the freshly imported artifact graph (as-of
/// `artifact_sha`); `imported_manifest` is the manifest bundled in that artifact
/// (or `None` if the artifact carried none). Postconditions on `Ok`: the graph
/// equals what a full index of the current tree would produce at the index
/// stage, a fresh local manifest reflecting the current tree is written to
/// `manifest_path`, and the returned `FillResult` reports the change partition
/// and the classification method. Never a silent path — the caller reports the
/// counts and method so no one mistakes a filled graph for a cold one.
///
/// Classification: primary is `git diff <artifact_sha>` against the working tree
/// (committed AND uncommitted, renames via `-M`, untracked via `ls-files`).
/// Fallback when git cannot diff (not a git tree / unknown sha): the imported
/// manifest's content hashes. If neither is available, returns `Err` so the
/// caller falls back to a full index explicitly.
pub fn fill_after_bootstrap(
    codebase: &Path,
    graph_dir: &Path,
    manifest_path: &Path,
    artifact_sha: &str,
    imported_manifest: Option<&FileManifest>,
    options: &IndexOptions,
) -> Result<FillResult, String> {
    let start = Instant::now();
    let dependency_scope = options.dependency_scope;
    // No create_schema(): the imported artifact graph already carries the schema.
    let store = GraphStore::open_or_create(graph_dir)?;
    store.require_entry_metadata()?;
    let walk_opts = WalkOptions {
        language_filter: options.language_filter,
        dependency_scope,
        exclude_dirs: options.exclude_dirs.clone(),
    };
    let (current, walk_gaps) = discover(codebase, walk_opts)?;

    // Choose the classification signal. git diff is precise and cheap on a fresh
    // clone (no mtimes to trust); the manifest hash fallback is correct but scans
    // the tree, so it is used only when git is unavailable.
    let (changes, unchanged_count, next_manifest, method) =
        match git_changes(codebase, artifact_sha, &current) {
            Some(cs) => {
                let reparsed = cs.changed.len() + cs.added.len() + cs.renamed.len();
                let unchanged = current.len().saturating_sub(reparsed);
                let manifest = manifest_from_discovered(&current);
                (cs, unchanged as u64, manifest, FillMethod::GitDiff)
            }
            None => {
                let prior = imported_manifest.ok_or_else(|| {
                    "artifact fill: cannot diff (git unavailable AND no bundled manifest)"
                        .to_string()
                })?;
                let plan = classify(prior, &current);
                let unchanged = plan.unchanged.len() as u64;
                (
                    plan.change_set,
                    unchanged,
                    plan.next_manifest,
                    FillMethod::ContentHash,
                )
            }
        };

    let changed = changes.changed.len() as u64;
    let added = changes.added.len() as u64;
    let deleted = changes.deleted.len() as u64;
    let renamed = changes.renamed.len() as u64;

    let (reparsed, reparsed_gaps) =
        apply_changes(&store, codebase, &changes, &current, dependency_scope)?;
    // Persist a local manifest reflecting the CURRENT tree so subsequent local
    // incrementals classify against this machine's mtimes/hashes, not the
    // artifact's (whose mtimes are meaningless on a fresh clone).
    manifest::save(manifest_path, &next_manifest)?;

    // Coverage (issue #57/#249): the artifact bundled the exporter's coverage
    // (loaded from the sidecar unpacked beside the graph). Carry forward the
    // files the fill did not touch, overlay the reparsed files' fresh gaps
    // plus this pass's walk-level excluded/unreadable directories, save.
    let mut merged_gaps = reparsed_gaps;
    merged_gaps.extend(walk_gaps);
    save_incremental_coverage(graph_dir, &current, &changes, merged_gaps, "bootstrap_fill");

    Ok(FillResult {
        result: IncrementalResult {
            graph_path: graph_dir.to_path_buf(),
            changed,
            added,
            deleted,
            renamed,
            unchanged: unchanged_count,
            files_reparsed: reparsed,
            elapsed_ms: start.elapsed().as_millis() as u64,
        },
        method,
    })
}

/// Builds a `FileManifest` for the current tree from an already-completed
/// discovery, hashing each file's bytes. Avoids a second walk (we already have
/// the paths + sizes); the hash is the only thing not carried by `Discovered`.
fn manifest_from_discovered(current: &[Discovered]) -> FileManifest {
    let mut m = FileManifest::new();
    for d in current {
        m.files.insert(
            d.rel.clone(),
            FileState {
                mtime_ns: d.mtime_ns,
                size: d.size,
                content_hash: manifest::hash_file(&d.abs).unwrap_or_default(),
            },
        );
    }
    m
}

/// Shared read-only context `collect_tracked_changes` and
/// `collect_untracked_adds` both need: the codebase root (to shell out to
/// git), the indexed-subtree prefix (to relativize git's paths), and the
/// rel→Discovered lookup (to map a git path to its working-tree metadata).
/// Bundled per §4.4 (param-object remedy — coding-standards.md, the same
/// pattern this repo already uses elsewhere for >4-parameter siblings):
/// `git_changes` builds one and passes it to both collectors instead of
/// each threading the same three values individually.
struct DiffContext<'a> {
    codebase: &'a Path,
    prefix: &'a str,
    by_rel: &'a HashMap<&'a str, &'a Discovered>,
}

/// Classifies the artifact→working-tree diff with git, or `None` when git cannot
/// be used (not a git working tree, or `artifact_sha` is unknown to this clone —
/// e.g. a shallow clone). `current` is the working-tree discovery, used to map
/// git paths to indexed File ids and to fetch (mtime, size) for re-parse.
///
/// Orchestrates the three phases (Fowler "Extract Function", §4.2 — this used
/// to be one 66-line function): setup/validation below, then
/// `collect_tracked_changes` for the committed+uncommitted tracked diff, then
/// `collect_untracked_adds` for new-but-never-`git add`-ed files. Paths are
/// made relative to the indexed subtree via `--show-prefix`; anything outside
/// it is ignored (both collectors read `ctx.prefix` for this).
fn git_changes(codebase: &Path, artifact_sha: &str, current: &[Discovered]) -> Option<ChangeSet> {
    if !is_hex_sha(artifact_sha) {
        return None;
    }
    // The indexed tree's path within the git repo (empty when codebase is the
    // repo root). `None` → not a git working tree → caller uses the fallback.
    let prefix = git_output(codebase, &["rev-parse", "--show-prefix"])?;
    let prefix = prefix.trim().to_string();
    // The artifact's commit must be present in this clone to diff against it.
    git_output(
        codebase,
        &["cat-file", "-e", &format!("{artifact_sha}^{{commit}}")],
    )?;

    let by_rel: HashMap<&str, &Discovered> = current.iter().map(|d| (d.rel.as_str(), d)).collect();
    let ctx = DiffContext {
        codebase,
        prefix: &prefix,
        by_rel: &by_rel,
    };
    let mut cs = ChangeSet::default();

    collect_tracked_changes(&ctx, artifact_sha, &mut cs);
    collect_untracked_adds(&ctx, &mut cs);

    Some(cs)
}

/// Fills `cs` with committed + uncommitted TRACKED changes: artifact commit
/// vs working tree. Uses `git diff <artifact_sha>` (artifact commit → WORKING
/// TREE, not just HEAD) so both committed and uncommitted tracked changes are
/// captured; rename detection is git's own `-M`. A `None` from `git_output`
/// (git failure) leaves `cs` untouched rather than erroring — `git_changes`
/// as a whole has already committed to `Some` by this point.
fn collect_tracked_changes(ctx: &DiffContext, artifact_sha: &str, cs: &mut ChangeSet) {
    let Some(diff) = git_output(
        ctx.codebase,
        &[
            "-c",
            "core.quotepath=false",
            "diff",
            "--name-status",
            "-M",
            artifact_sha,
            "--",
        ],
    ) else {
        return;
    };
    for line in diff.lines() {
        parse_diff_line(line, ctx.prefix, ctx.by_rel, cs);
    }
}

/// Fills `cs` with UNTRACKED new files (never `git add`-ed) — these are
/// invisible to `git diff`, so they're added here as `cs.added` entries to
/// make sure an uncommitted new file still gets (re)parsed. Dedups against
/// what `collect_tracked_changes` already recorded (added/changed) so a file
/// that appears in both listings — e.g. a rename target — isn't double-added.
fn collect_untracked_adds(ctx: &DiffContext, cs: &mut ChangeSet) {
    let Some(others) = git_output(
        ctx.codebase,
        &[
            "-c",
            "core.quotepath=false",
            "ls-files",
            "--others",
            "--exclude-standard",
        ],
    ) else {
        return;
    };
    let already: HashSet<String> = cs
        .added
        .iter()
        .map(|d| d.rel.clone())
        .chain(cs.changed.iter().map(|d| d.rel.clone()))
        .collect();
    for line in others.lines() {
        let Some(rel) = strip_prefix_path(line.trim(), ctx.prefix) else {
            continue;
        };
        if already.contains(&rel) {
            continue;
        }
        if let Some(d) = ctx.by_rel.get(rel.as_str()) {
            cs.added.push((*d).clone());
        }
    }
}

/// Parses one `git diff --name-status -M` line into `cs`. Statuses: `M`/`T`
/// (modified/type-changed) → changed; `A`/`C` (added/copied) → added; `D` →
/// deleted; `R` (renamed) → renamed. Paths outside the indexed subtree, or
/// targets the indexer doesn't visit, are skipped (a full index wouldn't include
/// them either).
fn parse_diff_line(
    line: &str,
    prefix: &str,
    by_rel: &HashMap<&str, &Discovered>,
    cs: &mut ChangeSet,
) {
    let mut fields = line.split('\t');
    let status = match fields.next() {
        Some(s) if !s.is_empty() => s,
        _ => return,
    };
    let code = status.as_bytes()[0];
    match code {
        b'R' | b'C' => {
            let old = fields.next();
            let new = fields.next();
            let (old, new) = match (old, new) {
                (Some(o), Some(n)) => (o, n),
                _ => return,
            };
            let new_rel = match strip_prefix_path(new, prefix) {
                Some(r) => r,
                None => return, // new path outside the indexed subtree
            };
            match by_rel.get(new_rel.as_str()) {
                Some(d) => {
                    if code == b'R' {
                        if let Some(old_rel) = strip_prefix_path(old, prefix) {
                            cs.renamed.push(Rename {
                                old_rel,
                                new_file: (*d).clone(),
                            });
                        } else {
                            cs.added.push((*d).clone());
                        }
                    } else {
                        cs.added.push((*d).clone());
                    }
                }
                None => {
                    // New end isn't indexed; drop the old end if this was a rename.
                    if code == b'R' {
                        if let Some(old_rel) = strip_prefix_path(old, prefix) {
                            cs.deleted.push(old_rel);
                        }
                    }
                }
            }
        }
        _ => {
            let path = match fields.next() {
                Some(p) => p,
                None => return,
            };
            let rel = match strip_prefix_path(path, prefix) {
                Some(r) => r,
                None => return,
            };
            match code {
                b'D' => cs.deleted.push(rel),
                b'A' => {
                    if let Some(d) = by_rel.get(rel.as_str()) {
                        cs.added.push((*d).clone());
                    }
                }
                // M, T, and any other in-place edit → changed.
                _ => {
                    if let Some(d) = by_rel.get(rel.as_str()) {
                        cs.changed.push((*d).clone());
                    }
                }
            }
        }
    }
}

/// Strips the repo-subtree `prefix` from a repo-relative git path, yielding the
/// indexed File id (forward-slash). Returns `None` when the path is outside the
/// indexed subtree.
fn strip_prefix_path(path: &str, prefix: &str) -> Option<String> {
    if prefix.is_empty() {
        return Some(path.to_string());
    }
    path.strip_prefix(prefix).map(|s| s.to_string())
}

/// Runs `git -C <codebase> <args>` and returns trimmed stdout on success, or
/// `None` on any failure (spawn error, non-zero exit). No shell — args are
/// passed directly (injection-safe, matching `artifact.rs`).
fn git_output(codebase: &Path, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(codebase)
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// True for a plausible git object sha (non-empty ASCII hex) — guards the sha
/// against `git` arg-injection, mirroring `artifact::is_hex_sha`.
fn is_hex_sha(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_hexdigit())
}

// ---------------------------------------------------------------------------
// Discovery + classification
// ---------------------------------------------------------------------------

/// Walks `codebase` and reads each file's (mtime, size), plus the walk-level
/// coverage gaps (issue #249: directories excluded by `exclude_dirs`, or
/// skipped because they were unreadable). Postcondition: one `Discovered` per
/// source file the full index would visit, with `rel` equal to the File node
/// id the indexer assigns; the returned map holds one `FileCoverage` entry per
/// pruned directory, keyed by its walk-root-relative path.
fn discover(
    codebase: &Path,
    walk_opts: WalkOptions,
) -> Result<(Vec<Discovered>, BTreeMap<String, FileCoverage>), String> {
    let outcome = collect_source_files(codebase, walk_opts)?;
    let mut collector = CoverageCollector::default();
    for rel in &outcome.excluded_dirs {
        collector.record_skipped(rel, "user_excluded".to_string());
    }
    for rel in &outcome.unreadable_dirs {
        collector.record_skipped(rel, "unreadable".to_string());
    }
    let mut out = Vec::with_capacity(outcome.files.len());
    for abs in outcome.files {
        let rel = light_link::rel_id(codebase, &abs);
        let (mtime_ns, size) = match std::fs::metadata(&abs) {
            Ok(m) => (manifest::mtime_ns(&m), m.len()),
            Err(_) => (0, 0),
        };
        out.push(Discovered {
            rel,
            abs,
            mtime_ns,
            size,
        });
    }
    Ok((out, collector.into_files()))
}

/// Classifies `current` against `prior`. See `Plan`. Rename detection pairs a
/// deleted file with an added file that carries an identical content hash (the
/// deleted file's hash comes from the manifest — the file is gone from disk, so
/// it cannot be re-hashed; the added file's hash is read fresh).
///
/// Orchestrates three phases (Fowler "Extract Function", §4.2 — this used to
/// be one 124-line function): `diff_current_against_prior` (mtime+size, hash
/// fallback), `index_deleted_by_hash` (indexes prior-only files for the
/// rename lookup), then `match_renames_and_adds` (matches added candidates
/// against that index). `next` — the manifest state to persist — accumulates
/// across all three phases, so it's threaded through and mutated in place
/// rather than rebuilt at the end.
fn classify(prior: &FileManifest, current: &[Discovered]) -> Plan {
    let current_ids: HashSet<&str> = current.iter().map(|d| d.rel.as_str()).collect();
    let mut next = FileManifest::new();

    let (changed, added_candidates, unchanged) =
        diff_current_against_prior(prior, current, &mut next);

    let (deleted_candidates, mut deleted_by_hash) = index_deleted_by_hash(prior, &current_ids);
    let (renamed, added, consumed_deleted) =
        match_renames_and_adds(added_candidates, &mut deleted_by_hash, &mut next);

    // Truly-deleted = deleted candidates not consumed by a rename pairing.
    let deleted: Vec<String> = deleted_candidates
        .into_iter()
        .filter(|rel| !consumed_deleted.contains(rel))
        .collect();

    Plan {
        change_set: ChangeSet {
            changed,
            added,
            deleted,
            renamed,
        },
        unchanged,
        next_manifest: next,
    }
}

/// Phase 1: classifies every CURRENT file against `prior` by cheap signal
/// (mtime+size) with a content-hash fallback when either moved — identical
/// bytes despite a moved mtime is the correctness fallback the C reference
/// lacks: a `touch` with no edit is NOT a change. Returns the (changed,
/// added_candidates, unchanged) partitions; `added_candidates` may still
/// resolve to a rename's new end in `match_renames_and_adds`. Fills `next`
/// with every current file's fresh state (a fresh hash for a moved
/// mtime/size, or the prior state carried forward unchanged).
fn diff_current_against_prior(
    prior: &FileManifest,
    current: &[Discovered],
    next: &mut FileManifest,
) -> (Vec<Discovered>, Vec<Discovered>, Vec<Discovered>) {
    let mut changed = Vec::new();
    let mut added_candidates = Vec::new();
    let mut unchanged = Vec::new();

    for d in current {
        match prior.files.get(&d.rel) {
            None => {
                // Not previously known → a new file (may still be a rename's
                // new end; resolved by match_renames_and_adds).
                added_candidates.push(d.clone());
            }
            Some(prev) if prev.mtime_ns == d.mtime_ns && prev.size == d.size => {
                // Cheap signal says unchanged: carry the prior hash forward.
                unchanged.push(d.clone());
                next.files.insert(d.rel.clone(), prev.clone());
            }
            Some(prev) => {
                let hash = manifest::hash_file(&d.abs).unwrap_or_default();
                let same_content = !hash.is_empty() && hash == prev.content_hash;
                if same_content {
                    unchanged.push(d.clone());
                } else {
                    changed.push(d.clone());
                }
                next.files.insert(
                    d.rel.clone(),
                    FileState {
                        mtime_ns: d.mtime_ns,
                        size: d.size,
                        content_hash: hash,
                    },
                );
            }
        }
    }

    (changed, added_candidates, unchanged)
}

/// Phase 2: indexes prior-only files (present before, absent from `current`)
/// by their stored content hash — the rename lookup `match_renames_and_adds`
/// probes against. A deleted file's hash comes from the manifest; the file no
/// longer exists on disk to re-hash.
fn index_deleted_by_hash(
    prior: &FileManifest,
    current_ids: &HashSet<&str>,
) -> (Vec<String>, HashMap<String, Vec<String>>) {
    // Deleted candidates: prior files absent from the current discovery.
    let deleted_candidates: Vec<String> = prior
        .files
        .keys()
        .filter(|rel| !current_ids.contains(rel.as_str()))
        .cloned()
        .collect();

    let mut deleted_by_hash: HashMap<String, Vec<String>> = HashMap::new();
    for rel in &deleted_candidates {
        if let Some(state) = prior.files.get(rel) {
            if !state.content_hash.is_empty() {
                deleted_by_hash
                    .entry(state.content_hash.clone())
                    .or_default()
                    .push(rel.clone());
            }
        }
    }

    (deleted_candidates, deleted_by_hash)
}

/// Phase 3: matches each `added_candidates` entry's freshly-read content hash
/// against `deleted_by_hash`; an unconsumed match becomes a `Rename` instead
/// of an add. Every added/renamed-new file gets its fresh state written into
/// `next` (mirrors `diff_current_against_prior`'s bookkeeping for the
/// current-file partitions). Returns the renamed pairs, the true adds, and
/// the set of prior-file rels consumed by a rename — `classify` uses that set
/// to filter its "truly deleted" list.
fn match_renames_and_adds(
    added_candidates: Vec<Discovered>,
    deleted_by_hash: &mut HashMap<String, Vec<String>>,
    next: &mut FileManifest,
) -> (Vec<Rename>, Vec<Discovered>, HashSet<String>) {
    let mut renamed = Vec::new();
    let mut added = Vec::new();
    let mut consumed_deleted: HashSet<String> = HashSet::new();

    for d in added_candidates {
        let hash = manifest::hash_file(&d.abs).unwrap_or_default();
        let matched_old = if hash.is_empty() {
            None
        } else {
            deleted_by_hash.get_mut(&hash).and_then(|olds| {
                olds.iter()
                    .position(|o| !consumed_deleted.contains(o))
                    .map(|i| olds[i].clone())
            })
        };
        // Every added/renamed-new file gets its fresh state in the manifest.
        next.files.insert(
            d.rel.clone(),
            FileState {
                mtime_ns: d.mtime_ns,
                size: d.size,
                content_hash: hash.clone(),
            },
        );
        match matched_old {
            Some(old_rel) => {
                consumed_deleted.insert(old_rel.clone());
                renamed.push(Rename {
                    old_rel,
                    new_file: d,
                });
            }
            None => added.push(d),
        }
    }

    (renamed, added, consumed_deleted)
}

// ---------------------------------------------------------------------------
// Graph mutation — purge
// ---------------------------------------------------------------------------

/// Deletes every symbol node of `rel` (id prefixed `"<rel>::"`), cascading their
/// edges, in a single label-agnostic pass.
///
/// A file-scoped symbol is exactly a node whose id begins with `"<rel>::"`; the
/// File node's id is the bare `"<rel>"` (no `::`) and Directory/Community/…/
/// Version ids never carry a `"<rel>::"` prefix, so this deletes precisely the
/// file's symbols and nothing else. Uses one unlabeled `MATCH (n) … DETACH
/// DELETE n` (verified supported by lbug 0.15) instead of one query per label —
/// ~12× fewer round-trips per changed file. source: `SYMBOL_LABELS` documents
/// which labels this covers; the unlabeled scan is the measured-fast equivalent.
fn purge_file_symbols(store: &GraphStore, rel: &str) -> Result<(), String> {
    let prefix = cypher_str(&format!("{rel}::"));
    let cypher = format!("MATCH (n) WHERE starts_with(n.id, {prefix}) DETACH DELETE n");
    store.execute_query(&cypher)?;
    Ok(())
}

/// Deletes the File node for `rel` and every edge touching it. Used for deleted
/// and renamed-old files only (a modified file keeps its File node).
fn purge_file_node(store: &GraphStore, rel: &str) -> Result<(), String> {
    let id = cypher_str(rel);
    let cypher = format!("MATCH (f:File) WHERE f.id = {id} DETACH DELETE f");
    store.execute_query(&cypher)?;
    Ok(())
}

/// Deletes the `FileContent` row for `rel`, when one exists.
///
/// `FileContent.id` equals the File node's bare `"<rel>"` id (by design —
/// `MATCH (fc:FileContent {id: file_id})` is a direct lookup), which is
/// exactly the shape `purge_file_symbols`'s `"<rel>::"`-PREFIX scan does NOT
/// cover (same reason `purge_file_node` needs its own exact-id delete rather
/// than reusing the prefix scan). Root cause of a real bug (found via CI,
/// not by inspection): a MODIFIED file keeps its File node across a reparse,
/// so without this call its stale FileContent row (from the file's PREVIOUS
/// content) was never purged; `persist_full_ast`'s
/// `insert_file_content` then hit lbug's primary-key uniqueness constraint
/// on the re-insert, the error was logged and swallowed (best-effort, by
/// design — a FileContent failure must not fail the file's indexing), and
/// the file was silently left with STALE content and a missing fresh
/// FileContent node — an incremental-fill graph that diverged from a
/// from-scratch full index by exactly one node per re-touched file.
/// Called for every purge site `purge_file_symbols`/`purge_file_node`
/// already runs (changed/deleted/renamed-old): safe as a no-op when no
/// FileContent row exists yet (added files, or a graph indexed before this
/// layer existed).
fn purge_file_content(store: &GraphStore, rel: &str) -> Result<(), String> {
    let id = cypher_str(rel);
    let cypher = format!("MATCH (fc:FileContent) WHERE fc.id = {id} DETACH DELETE fc");
    store.execute_query(&cypher)?;
    Ok(())
}

/// Deletes the light-link edges OUT of `rel` so the re-parse can re-derive them
/// from the file's current text (an import removed by the edit must disappear;
/// one added must appear). Inbound light-link edges are untouched — the File
/// node survives, so edges from unchanged files into `rel` are preserved.
fn purge_outbound_light_links(store: &GraphStore, rel: &str) -> Result<(), String> {
    let id = cypher_str(rel);
    for table in LIGHT_LINK_TABLES {
        let cypher = format!("MATCH (a:File)-[r:{table}]->(:File) WHERE a.id = {id} DELETE r");
        store.execute_query(&cypher)?;
    }
    Ok(())
}

/// The ids of every Directory node currently in the graph, as `PathBuf`s keyed
/// the same way `insert_ancestor_dirs` tracks them (the relative dir path).
fn existing_directory_ids(store: &GraphStore) -> Result<HashSet<PathBuf>, String> {
    let qr = store.execute_query("MATCH (d:Directory) RETURN d.id")?;
    Ok(qr
        .rows
        .into_iter()
        .filter_map(|row| row.into_iter().next())
        .map(PathBuf::from)
        .collect())
}

/// Deletes Directory nodes left with no children (no `Contains_Dir_File` and no
/// `Contains_Dir_Dir` out-edges) after a purge, to a fixpoint — deleting a leaf
/// directory can orphan its parent. A Directory's only out-edges are the two
/// containment kinds, so "zero out-edges" is exactly "childless". Bounded by
/// `MAX_DEPTH` iterations (the walker's own directory-depth cap), so a
/// pathological tree cannot loop unbounded.
fn prune_orphan_directories(store: &GraphStore) -> Result<(), String> {
    for _ in 0..super::MAX_DEPTH {
        let orphans = store.execute_query(
            "MATCH (d:Directory) OPTIONAL MATCH (d)-[e]->() \
             WITH d, count(e) AS c WHERE c = 0 RETURN d.id",
        )?;
        if orphans.rows.is_empty() {
            return Ok(());
        }
        for row in orphans.rows {
            if let Some(id) = row.into_iter().next() {
                let cypher = format!(
                    "MATCH (d:Directory) WHERE d.id = {} DETACH DELETE d",
                    cypher_str(&id)
                );
                store.execute_query(&cypher)?;
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Graph mutation — re-parse
// ---------------------------------------------------------------------------

/// Re-parses a MODIFIED file into the existing graph. The File node is kept
/// (its id is stable); its symbols were already purged. Resets the File node's
/// mutable columns (size, parse_errors) then re-inserts the parsed symbols and
/// intra-file edges. Postcondition: the file's symbol subgraph equals a fresh
/// parse; inbound File-targeted edges are untouched throughout.
fn reparse_modified_file(
    store: &GraphStore,
    codebase: &Path,
    d: &Discovered,
    dependency_scope: DependencyScope,
) -> Result<ParseOutcome, String> {
    // Reset the kept File node's mutable state. index_single_file re-bumps
    // parse_errors only when >0, so we clear it first (a fixed parse must drop
    // back to 0), and refresh size_bytes to the new length.
    let id = cypher_str(&d.rel);
    let reset = format!(
        "MATCH (f:File) WHERE f.id = {id} SET f.parse_errors = 0, f.size_bytes = {}",
        d.size
    );
    store.execute_query(&reset)?;

    let mut batch = SymbolBatch::default();
    let mut label_by_qn: HashMap<String, HashSet<String>> = HashMap::new();
    label_by_qn
        .entry(d.rel.clone())
        .or_default()
        .insert("File".into());
    let mut seen_node_ids: HashSet<(String, String)> = HashSet::new();
    let restrict =
        dependency_scope == DependencyScope::PublicApi && is_dependency_path(codebase, &d.abs);
    let outcome = persist::index_single_file(
        store,
        &mut batch,
        &d.abs,
        &d.rel,
        &mut label_by_qn,
        &mut seen_node_ids,
        restrict,
    );
    batch.flush(store)?;
    Ok(outcome)
}

/// Indexes a NEW (or renamed-new) file from scratch: ancestor Directory nodes,
/// the File node, the Dir→File containment edge, then the parsed symbols and
/// intra-file edges — exactly the per-file body of the full index.
fn reparse_new_file(
    store: &GraphStore,
    codebase: &Path,
    d: &Discovered,
    dependency_scope: DependencyScope,
    dir_nodes_inserted: &mut HashSet<PathBuf>,
) -> Result<ParseOutcome, String> {
    let mut batch = SymbolBatch::default();
    let mut label_by_qn: HashMap<String, HashSet<String>> = HashMap::new();
    let mut seen_node_ids: HashSet<(String, String)> = HashSet::new();

    persist::insert_ancestor_dirs(
        store,
        &mut batch,
        codebase,
        &d.abs,
        dir_nodes_inserted,
        &mut label_by_qn,
    )?;
    persist::insert_file_node(store, &d.abs, &d.rel)?;
    label_by_qn
        .entry(d.rel.clone())
        .or_default()
        .insert("File".into());
    let rel_path = relative_path(codebase, &d.abs);
    persist::insert_dir_file_edge(&mut batch, &rel_path);

    let restrict =
        dependency_scope == DependencyScope::PublicApi && is_dependency_path(codebase, &d.abs);
    let outcome = persist::index_single_file(
        store,
        &mut batch,
        &d.abs,
        &d.rel,
        &mut label_by_qn,
        &mut seen_node_ids,
        restrict,
    );
    batch.flush(store)?;
    Ok(outcome)
}

// ---------------------------------------------------------------------------
// Inbound cross-file edge snapshot + re-link
// ---------------------------------------------------------------------------

/// One captured inbound edge whose target sits in a modified file and whose
/// source sits elsewhere, saved so the purge does not orphan it.
struct SavedEdge {
    table: &'static str,
    source_id: String,
    target_id: String,
    confidence: String,
    resolution_method: String,
}

/// Prefixes of the resolution rel tables — the only tables that carry cross-file
/// edges (structural Defines/HasMethod/… are intra-file and re-emitted by the
/// re-parse). Mirrors `graph_store::is_resolution_rel`, kept local so this
/// module reasons from the public `REL_TABLES` list alone.
const RESOLUTION_PREFIXES: &[&str] = &[
    "Imports_",
    "Calls_",
    "Implements_",
    "Extends_",
    "Uses_",
    "References_",
];

/// Snapshots inbound cross-file edges into the symbols of `changed_rels`.
///
/// Captures an edge iff its target is a symbol inside a changed file
/// (`starts_with(t.id, "<rel>::")`) and its source's owning file is NOT in
/// `reparsed_set` (an edge from another re-parsed file is regenerated by that
/// file's own re-parse/re-link, so re-linking it here would be redundant). At
/// the index stage AP produces no cross-file symbol-targeted edges, so this
/// captures nothing; it becomes load-bearing the moment the graph also carries
/// resolved edges (a resolve_graph pass, or a future resolved artifact), which
/// is exactly the case the C reference's snapshot exists for.
fn snapshot_inbound_edges(
    store: &GraphStore,
    changed_rels: &[&str],
    reparsed_set: &HashSet<String>,
) -> Result<Vec<SavedEdge>, String> {
    let mut saved = Vec::new();
    if changed_rels.is_empty() {
        return Ok(saved);
    }
    // One `starts_with(t.id, 'p1') OR starts_with(t.id, 'p2') …` predicate covers
    // every changed file at once, so the scan costs one query per resolution
    // table instead of one per (table × file).
    let target_predicate = changed_rels
        .iter()
        .map(|rel| format!("starts_with(t.id, {})", cypher_str(&format!("{rel}::"))))
        .collect::<Vec<_>>()
        .join(" OR ");

    for &(name, _from, to) in REL_TABLES.iter() {
        if !is_resolution_table(name) {
            continue;
        }
        // File/Directory-targeted edges never need snapshotting: those nodes
        // are kept across a modification, so inbound edges survive structurally.
        if to == "File" || to == "Directory" {
            continue;
        }
        let cypher = format!(
            "MATCH (s)-[r:{name}]->(t) WHERE {target_predicate} \
             RETURN s.id, t.id, r.confidence, r.resolution_method"
        );
        let qr = store.execute_query(&cypher)?;
        for row in qr.rows {
            if row.len() < 2 {
                continue;
            }
            let source_id = row[0].clone();
            let target_id = row[1].clone();
            // Skip edges whose source is itself being re-parsed.
            if reparsed_set.contains(&owning_file(&source_id)) {
                continue;
            }
            saved.push(SavedEdge {
                table: name,
                source_id,
                target_id,
                confidence: row.get(2).cloned().unwrap_or_default(),
                resolution_method: row.get(3).cloned().unwrap_or_default(),
            });
        }
    }
    Ok(saved)
}

/// Re-inserts each snapshotted edge whose endpoints both still exist. A missing
/// endpoint makes `insert_edge`'s MATCH…MATCH…CREATE a no-op (target symbol was
/// deleted/renamed by the edit) — matching full-reindex semantics, which would
/// also not produce an edge to a vanished symbol.
fn relink_inbound_edges(store: &GraphStore, saved: &[SavedEdge]) -> Result<(), String> {
    for e in saved {
        let conf = if e.confidence.trim().is_empty() {
            "1.0".to_string()
        } else {
            e.confidence.clone()
        };
        let method = cypher_str(&e.resolution_method);
        let props: Vec<(&str, &str)> = vec![("confidence", &conf), ("resolution_method", &method)];
        // Best-effort: a schema mismatch on one edge must not abort the pass.
        if let Err(err) = store.insert_edge(e.table, &e.source_id, &e.target_id, &props) {
            eprintln!(
                "incremental: relink {} {} -> {} skipped: {err}",
                e.table, e.source_id, e.target_id
            );
        }
    }
    Ok(())
}

/// The repo-relative file that a node id belongs to: the substring before the
/// first `"::"` for a symbol, or the whole id for a File/Directory node.
fn owning_file(id: &str) -> String {
    match id.find("::") {
        Some(i) => id[..i].to_string(),
        None => id.to_string(),
    }
}

/// True for a resolution rel table (carries cross-file edges).
fn is_resolution_table(name: &str) -> bool {
    RESOLUTION_PREFIXES.iter().any(|p| name.starts_with(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owning_file_splits_on_first_separator() {
        assert_eq!(owning_file("src/a.rs::foo::bar"), "src/a.rs");
        assert_eq!(owning_file("src/a.rs"), "src/a.rs");
        assert_eq!(owning_file("src/a.rs::call@10:2#3-9"), "src/a.rs");
    }

    #[test]
    fn resolution_tables_recognised() {
        assert!(is_resolution_table("Calls_Function_Function"));
        assert!(is_resolution_table("Imports_File_File"));
        assert!(!is_resolution_table("Defines_File_Function"));
        assert!(!is_resolution_table("Contains_Dir_File"));
    }

    /// Builds the fixture `classify_detects_each_class_and_rename` exercises
    /// (Fowler "Extract Function", §4.2 — the test used to be one 75-line
    /// function): an on-disk current tree — keep.py (unchanged), edit.py
    /// (modified), new.py (added), moved_to.py (rename target) — plus a prior
    /// manifest covering every classification category: keep.py unchanged;
    /// edit.py with a stale hash (edited); old_name.py carrying moved_to.py's
    /// content hash (the rename); gone.py a plain deletion. Returns the prior
    /// manifest, the current discovery, and the tempdir guard — the caller
    /// must hold the guard for the test's duration (drop removes the tree).
    fn build_classify_fixture() -> (FileManifest, Vec<Discovered>, tempfile::TempDir) {
        let dir = tempfile::Builder::new()
            .prefix("incremental_classify_")
            .tempdir()
            .expect("temp dir");
        let root = dir.path();

        std::fs::write(root.join("keep.py"), "def keep():\n    return 1\n").unwrap();
        std::fs::write(root.join("edit.py"), "def edit():\n    return 2\n").unwrap();
        std::fs::write(root.join("new.py"), "def fresh():\n    return 3\n").unwrap();
        // Rename: moved_to.py holds the exact bytes old_name.py had.
        let moved_body = "def moved():\n    return 4\n";
        std::fs::write(root.join("moved_to.py"), moved_body).unwrap();

        let (current, _gaps) = discover(root, WalkOptions::default()).expect("discover");
        let hash_of = |rel: &str| manifest::hash_file(&root.join(rel)).unwrap();

        let keep = current.iter().find(|d| d.rel == "keep.py").unwrap();
        let mut prior = FileManifest::new();
        prior.files.insert(
            "keep.py".into(),
            FileState {
                mtime_ns: keep.mtime_ns,
                size: keep.size,
                content_hash: hash_of("keep.py"),
            },
        );
        prior.files.insert(
            "edit.py".into(),
            FileState {
                mtime_ns: 1,
                size: 999,
                content_hash: "stale".into(),
            },
        );
        prior.files.insert(
            "old_name.py".into(),
            FileState {
                mtime_ns: 1,
                size: moved_body.len() as u64,
                content_hash: hash_of("moved_to.py"),
            },
        );
        prior.files.insert(
            "gone.py".into(),
            FileState {
                mtime_ns: 1,
                size: 10,
                content_hash: "whatever".into(),
            },
        );

        (prior, current, dir)
    }

    #[test]
    fn classify_detects_each_class_and_rename() {
        let (prior, current, _dir) = build_classify_fixture();

        let plan = classify(&prior, &current);
        let cs = &plan.change_set;
        let changed: Vec<&str> = cs.changed.iter().map(|d| d.rel.as_str()).collect();
        let added: Vec<&str> = cs.added.iter().map(|d| d.rel.as_str()).collect();
        let unchanged: Vec<&str> = plan.unchanged.iter().map(|d| d.rel.as_str()).collect();

        assert_eq!(changed, vec!["edit.py"], "edited file is changed");
        assert_eq!(added, vec!["new.py"], "genuinely new file is added");
        assert_eq!(unchanged, vec!["keep.py"], "untouched file is unchanged");
        assert_eq!(cs.deleted, vec!["gone.py"], "removed file is deleted");
        assert_eq!(cs.renamed.len(), 1, "one rename detected");
        assert_eq!(cs.renamed[0].old_rel, "old_name.py");
        assert_eq!(cs.renamed[0].new_file.rel, "moved_to.py");
        // Manifest carries every current file, and no stale prior-only entries.
        assert_eq!(plan.next_manifest.files.len(), 4);
        assert!(plan.next_manifest.files.contains_key("moved_to.py"));
        assert!(!plan.next_manifest.files.contains_key("old_name.py"));
    }
}
