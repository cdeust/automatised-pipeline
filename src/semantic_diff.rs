// semantic_diff — Stage 9: compare post-implementation graph against
// pre-implementation graph and flag regressions.
//
// Read-only with respect to both graphs. Never mutates the before- or
// after-graph. Callers supply absolute paths to two LadybugDB directories
// that were independently produced by indexer::index_codebase_with_language.
//
// Outputs a structured diff report:
//   - nodes_added / nodes_removed (by qualified_name within each label)
//   - edges_added / edges_removed (by (from, rel_kind, to) triple)
//   - dangling_references        (edges in `after` whose target vanished)
//   - new_unresolved_delta       (positive = resolution regressed)
//   - new_cycles                 (SCCs in `after` that weren't in `before`)
//
// The regression_score is a **heuristic** linear combination — the brief
// is explicit about this: "source comment: heuristic, not paper-backed."

use crate::graph_store::{GraphStore, REL_TABLES};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

// source: stage-9 brief — weights for regression_score components.
// "heuristic, not paper-backed." Each constant is named so tuning is a
// one-line edit rather than a scattered magic-number hunt.
pub const WEIGHT_DANGLING: f64 = 1.0;
pub const WEIGHT_NEW_CYCLE: f64 = 0.5;
pub const WEIGHT_UNRESOLVED_DELTA: f64 = 0.1;
pub const UNRESOLVED_DELTA_MAX: f64 = 5.0;
pub const REGRESSION_SCORE_CAP: f64 = 10.0;

// source: stage-9 brief — verdict thresholds on the capped score.
pub const VERDICT_CLEAN_MAX: f64 = 1.0;
pub const VERDICT_CONCERNING_MAX: f64 = 5.0;

// source: stage-9 brief — details list truncation to keep the report compact.
pub const DETAILS_TRUNCATION: usize = 100;

// source: stage-9 brief — labels whose qualified_name identity defines
// "the same symbol" across two indexing runs. File/Directory/Import are
// content-addressed and noisy; we restrict diffing to semantic symbols.
const DIFFABLE_LABELS: &[&str] = &[
    "Function", "Method", "Struct", "Enum", "Trait",
    "Module", "Constant", "TypeAlias",
];

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

pub struct SemanticDiffArgs {
    pub before_graph_path: PathBuf,
    pub after_graph_path: PathBuf,
}

pub struct SemanticDiffOutcome {
    pub summary: DiffSummary,
    pub regression_score: f64,
    pub verdict: &'static str,
    pub report: Value,
}

#[derive(Clone, Copy, Default)]
pub struct DiffSummary {
    pub nodes_added: u64,
    pub nodes_removed: u64,
    pub edges_added: u64,
    pub edges_removed: u64,
    pub dangling_references: u64,
    pub new_unresolved_delta: i64,
    pub new_cycles: u64,
}

// ---------------------------------------------------------------------------
// Orchestration
// ---------------------------------------------------------------------------

/// Diffs two graphs and returns the full report.
pub fn diff(args: &SemanticDiffArgs, verified_at: String) -> Result<SemanticDiffOutcome, String> {
    check_paths_exist(args)?;
    let before = Snapshot::collect(&args.before_graph_path)?;
    let after = Snapshot::collect(&args.after_graph_path)?;

    let node_diff = diff_nodes(&before.nodes, &after.nodes);
    let edge_diff = diff_edges(&before.edges, &after.edges);
    let dangling = compute_dangling(&after.nodes, &after.edges);
    let unresolved_delta = after.unresolved as i64 - before.unresolved as i64;
    let cycles_before = strongly_connected_cycles(&before.nodes, &before.edges);
    let cycles_after = strongly_connected_cycles(&after.nodes, &after.edges);
    let new_cycles = diff_cycle_sets(&cycles_before, &cycles_after);

    let summary = DiffSummary {
        nodes_added: node_diff.added.len() as u64,
        nodes_removed: node_diff.removed.len() as u64,
        edges_added: edge_diff.added.len() as u64,
        edges_removed: edge_diff.removed.len() as u64,
        dangling_references: dangling.len() as u64,
        new_unresolved_delta: unresolved_delta,
        new_cycles: new_cycles.len() as u64,
    };
    let score = regression_score(&summary);
    let verdict = classify_verdict(score);
    let report = build_report(
        args, &verified_at, &summary, score, verdict,
        &node_diff, &edge_diff, &dangling, &new_cycles,
    );
    Ok(SemanticDiffOutcome { summary, regression_score: score, verdict, report })
}

fn check_paths_exist(args: &SemanticDiffArgs) -> Result<(), String> {
    if !args.before_graph_path.exists() {
        return Err(format!(
            "before_graph_path_missing: {}",
            args.before_graph_path.display()
        ));
    }
    if !args.after_graph_path.exists() {
        return Err(format!(
            "after_graph_path_missing: {}",
            args.after_graph_path.display()
        ));
    }
    Ok(())
}

/// Everything we need to know about one of the two graphs, loaded once.
struct Snapshot {
    nodes: NodeSet,
    edges: EdgeSet,
    unresolved: u64,
}

impl Snapshot {
    fn collect(path: &Path) -> Result<Self, String> {
        let store = GraphStore::open_or_create(path)?;
        Ok(Snapshot {
            nodes: collect_nodes(&store),
            edges: collect_edges(&store),
            unresolved: count_unresolved(&store),
        })
    }
}

// ---------------------------------------------------------------------------
// Node collection
// ---------------------------------------------------------------------------

type NodeSet = BTreeMap<String, BTreeSet<String>>; // label -> { qualified_name }

fn collect_nodes(store: &GraphStore) -> NodeSet {
    let mut out: NodeSet = BTreeMap::new();
    for label in DIFFABLE_LABELS {
        let cypher = format!(
            "MATCH (n:{label}) RETURN n.qualified_name"
        );
        let qr = match store.execute_query(&cypher) {
            Ok(q) => q,
            Err(_) => continue,
        };
        let entry = out.entry((*label).into()).or_default();
        for row in &qr.rows {
            if let Some(qn) = row.first() {
                if !qn.is_empty() {
                    entry.insert(qn.clone());
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Edge collection — keyed by (from_qn, rel_kind, to_qn)
// ---------------------------------------------------------------------------

type EdgeTriple = (String, String, String);
type EdgeSet = HashSet<EdgeTriple>;

fn collect_edges(store: &GraphStore) -> EdgeSet {
    let mut out: EdgeSet = HashSet::new();
    for &(rel, from_label, to_label) in REL_TABLES {
        let kind = rel_kind(rel);
        let cypher = format!(
            "MATCH (a:{from_label})-[:{rel}]->(b:{to_label}) \
             RETURN a.qualified_name, b.qualified_name"
        );
        let qr = match store.execute_query(&cypher) {
            Ok(q) => q,
            Err(_) => continue,
        };
        for row in &qr.rows {
            if row.len() < 2 {
                continue;
            }
            let from = &row[0];
            let to = &row[1];
            if from.is_empty() || to.is_empty() {
                continue;
            }
            out.insert((from.clone(), kind.to_string(), to.clone()));
        }
    }
    out
}

/// Collapses table-name (e.g. `Calls_Function_Method`) to the semantic kind.
fn rel_kind(rel: &str) -> &str {
    // Split on `_` and keep the first component. Table naming convention is
    // `{Kind}_{FromLabel}_{ToLabel}` (graph_store.rs comment §rel_table_ddl).
    rel.split('_').next().unwrap_or(rel)
}

// ---------------------------------------------------------------------------
// Node/edge diffing
// ---------------------------------------------------------------------------

struct NodeDiff {
    added: Vec<(String, String)>,   // (label, qualified_name)
    removed: Vec<(String, String)>,
}

fn diff_nodes(before: &NodeSet, after: &NodeSet) -> NodeDiff {
    let mut added = Vec::new();
    let mut removed = Vec::new();
    let all_labels: BTreeSet<&String> = before.keys().chain(after.keys()).collect();
    for label in all_labels {
        let empty = BTreeSet::new();
        let b = before.get(label).unwrap_or(&empty);
        let a = after.get(label).unwrap_or(&empty);
        for qn in a.difference(b) {
            added.push((label.clone(), qn.clone()));
        }
        for qn in b.difference(a) {
            removed.push((label.clone(), qn.clone()));
        }
    }
    NodeDiff { added, removed }
}

struct EdgeDiff {
    added: Vec<EdgeTriple>,
    removed: Vec<EdgeTriple>,
}

fn diff_edges(before: &EdgeSet, after: &EdgeSet) -> EdgeDiff {
    let added: Vec<EdgeTriple> = after.difference(before).cloned().collect();
    let removed: Vec<EdgeTriple> = before.difference(after).cloned().collect();
    EdgeDiff { added, removed }
}

// ---------------------------------------------------------------------------
// Dangling reference detection — edges in `after` whose target isn't a node
// ---------------------------------------------------------------------------

fn compute_dangling(after_nodes: &NodeSet, after_edges: &EdgeSet) -> Vec<EdgeTriple> {
    let mut all_qns: HashSet<&str> = HashSet::new();
    for set in after_nodes.values() {
        for qn in set {
            all_qns.insert(qn.as_str());
        }
    }
    let mut dangling = Vec::new();
    for edge in after_edges {
        if !all_qns.contains(edge.2.as_str()) {
            dangling.push(edge.clone());
        }
    }
    dangling
}

// ---------------------------------------------------------------------------
// Unresolved count — reads the Import node count as a resolution proxy
// ---------------------------------------------------------------------------

/// Counts nodes with label "Import" that remain after resolution. An Import
/// node survives iff the static resolver could not rewrite it into a
/// concrete edge — so count(Import) is the canonical unresolved metric.
///
/// source: stages/stage-3b.md §"Unresolved" — Import nodes are the
/// post-resolution fallback for references the resolver could not place.
fn count_unresolved(store: &GraphStore) -> u64 {
    let cypher = "MATCH (i:Import) RETURN count(i)";
    match store.execute_query(cypher) {
        Ok(qr) => qr
            .rows
            .first()
            .and_then(|r| r.first())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0),
        Err(_) => 0,
    }
}

// ---------------------------------------------------------------------------
// Cycle detection — Tarjan's SCC on the qualified_name graph
// ---------------------------------------------------------------------------
//
// source: Tarjan, R. "Depth-First Search and Linear Graph Algorithms",
// SIAM J. Comput. 1(2):146-160 (1972). We treat every edge as directed
// and build adjacency from the `EdgeSet`. A cycle in this graph is an
// SCC of size ≥ 2, OR a self-loop (size 1 with an edge to itself).
//
// Why Tarjan over Kosaraju: single DFS pass, no reverse graph needed.
// Correctness of the canonical form (sorted qualified_name tuple) lets us
// compare before/after cycles as plain strings.

fn strongly_connected_cycles(
    nodes: &NodeSet,
    edges: &EdgeSet,
) -> BTreeSet<String> {
    let (node_list, adj, self_loops) = build_adjacency(nodes, edges);
    let sccs = tarjan_scc(&adj);
    let mut out: BTreeSet<String> = BTreeSet::new();
    for scc in sccs {
        if scc.len() < 2 {
            continue;
        }
        let mut names: Vec<String> =
            scc.iter().map(|&i| node_list[i].clone()).collect();
        names.sort();
        out.insert(names.join(","));
    }
    for &i in &self_loops {
        out.insert(node_list[i].clone());
    }
    out
}

/// Builds the flat adjacency list + self-loop set from the graph snapshots.
/// Returned tuple: (ordered qualified_names, adj[i] = neighbor indices,
/// self-loops as indices into the ordered list).
fn build_adjacency(
    nodes: &NodeSet,
    edges: &EdgeSet,
) -> (Vec<String>, Vec<Vec<usize>>, BTreeSet<usize>) {
    let mut node_list: Vec<String> = Vec::new();
    let mut idx_by_qn: HashMap<String, usize> = HashMap::new();
    for set in nodes.values() {
        for qn in set {
            if !idx_by_qn.contains_key(qn) {
                idx_by_qn.insert(qn.clone(), node_list.len());
                node_list.push(qn.clone());
            }
        }
    }
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); node_list.len()];
    let mut self_loops: BTreeSet<usize> = BTreeSet::new();
    for (from, _kind, to) in edges {
        let (fi, ti) = match (idx_by_qn.get(from), idx_by_qn.get(to)) {
            (Some(&a), Some(&b)) => (a, b),
            _ => continue,
        };
        if fi == ti {
            self_loops.insert(fi);
        } else {
            adj[fi].push(ti);
        }
    }
    (node_list, adj, self_loops)
}

/// Iterative Tarjan SCC. Returns each SCC as a Vec of node indices.
///
/// source: Pearce, David J. "An Improved Algorithm for Finding the
/// Strongly Connected Components of a Directed Graph", Technical
/// Report, Victoria University of Wellington (2005). Iterative variant
/// avoids Rust's default-small recursion stack on deep call graphs.
fn tarjan_scc(adj: &[Vec<usize>]) -> Vec<Vec<usize>> {
    let n = adj.len();
    let mut index: Vec<i64> = vec![-1; n];
    let mut lowlink: Vec<i64> = vec![-1; n];
    let mut on_stack: Vec<bool> = vec![false; n];
    let mut stack: Vec<usize> = Vec::new();
    let mut result: Vec<Vec<usize>> = Vec::new();
    let mut counter: i64 = 0;

    for v in 0..n {
        if index[v] != -1 {
            continue;
        }
        dfs_iterative(
            v, adj, &mut index, &mut lowlink, &mut on_stack,
            &mut stack, &mut result, &mut counter,
        );
    }
    result
}

fn dfs_iterative(
    start: usize,
    adj: &[Vec<usize>],
    index: &mut [i64],
    lowlink: &mut [i64],
    on_stack: &mut [bool],
    stack: &mut Vec<usize>,
    result: &mut Vec<Vec<usize>>,
    counter: &mut i64,
) {
    // Work stack: (node, next_neighbor_idx).
    let mut work: Vec<(usize, usize)> = Vec::new();
    visit(start, index, lowlink, on_stack, stack, counter);
    work.push((start, 0));

    while let Some(&(v, i)) = work.last() {
        if i < adj[v].len() {
            let w = adj[v][i];
            work.last_mut().unwrap().1 += 1;
            if index[w] == -1 {
                visit(w, index, lowlink, on_stack, stack, counter);
                work.push((w, 0));
            } else if on_stack[w] {
                lowlink[v] = lowlink[v].min(index[w]);
            }
        } else {
            work.pop();
            if lowlink[v] == index[v] {
                pop_scc(v, on_stack, stack, result);
            }
            if let Some(&(parent, _)) = work.last() {
                lowlink[parent] = lowlink[parent].min(lowlink[v]);
            }
        }
    }
}

fn visit(
    v: usize,
    index: &mut [i64],
    lowlink: &mut [i64],
    on_stack: &mut [bool],
    stack: &mut Vec<usize>,
    counter: &mut i64,
) {
    index[v] = *counter;
    lowlink[v] = *counter;
    *counter += 1;
    stack.push(v);
    on_stack[v] = true;
}

fn pop_scc(
    v: usize,
    on_stack: &mut [bool],
    stack: &mut Vec<usize>,
    result: &mut Vec<Vec<usize>>,
) {
    let mut component = Vec::new();
    while let Some(w) = stack.pop() {
        on_stack[w] = false;
        component.push(w);
        if w == v {
            break;
        }
    }
    result.push(component);
}

fn diff_cycle_sets(
    before: &BTreeSet<String>,
    after: &BTreeSet<String>,
) -> Vec<String> {
    after.difference(before).cloned().collect()
}

// ---------------------------------------------------------------------------
// Score + verdict
// ---------------------------------------------------------------------------

/// heuristic, not paper-backed — combines the four regression signals.
fn regression_score(s: &DiffSummary) -> f64 {
    let dangling = WEIGHT_DANGLING * s.dangling_references as f64;
    let cycles = WEIGHT_NEW_CYCLE * s.new_cycles as f64;
    let unresolved = WEIGHT_UNRESOLVED_DELTA
        * s.new_unresolved_delta.max(0) as f64;
    let unresolved = unresolved.min(UNRESOLVED_DELTA_MAX * WEIGHT_UNRESOLVED_DELTA * 10.0);
    (dangling + cycles + unresolved).min(REGRESSION_SCORE_CAP)
}

fn classify_verdict(score: f64) -> &'static str {
    if score < VERDICT_CLEAN_MAX {
        "clean"
    } else if score < VERDICT_CONCERNING_MAX {
        "concerning"
    } else {
        "regression"
    }
}

// ---------------------------------------------------------------------------
// Report builder
// ---------------------------------------------------------------------------

fn build_report(
    args: &SemanticDiffArgs,
    verified_at: &str,
    summary: &DiffSummary,
    score: f64,
    verdict: &str,
    node_diff: &NodeDiff,
    edge_diff: &EdgeDiff,
    dangling: &[EdgeTriple],
    new_cycles: &[String],
) -> Value {
    json!({
        "verified_at": verified_at,
        "before_graph_path": args.before_graph_path.to_string_lossy(),
        "after_graph_path": args.after_graph_path.to_string_lossy(),
        "summary": {
            "nodes_added": summary.nodes_added,
            "nodes_removed": summary.nodes_removed,
            "edges_added": summary.edges_added,
            "edges_removed": summary.edges_removed,
            "dangling_references": summary.dangling_references,
            "new_unresolved_delta": summary.new_unresolved_delta,
            "new_cycles": summary.new_cycles,
        },
        "regression_score": score,
        "verdict": verdict,
        "details": {
            "nodes_added": truncate_labeled(&node_diff.added),
            "nodes_removed": truncate_labeled(&node_diff.removed),
            "edges_added": truncate_triples(&edge_diff.added),
            "edges_removed": truncate_triples(&edge_diff.removed),
            "dangling_references": truncate_triples(dangling),
            "new_cycles": truncate_strings(new_cycles),
        }
    })
}

fn truncate_labeled(items: &[(String, String)]) -> Vec<Value> {
    items
        .iter()
        .take(DETAILS_TRUNCATION)
        .map(|(l, q)| json!({"label": l, "qualified_name": q}))
        .collect()
}

fn truncate_triples(items: &[EdgeTriple]) -> Vec<Value> {
    items
        .iter()
        .take(DETAILS_TRUNCATION)
        .map(|(f, k, t)| json!({"from": f, "kind": k, "to": t}))
        .collect()
}

fn truncate_strings(items: &[String]) -> Vec<Value> {
    items
        .iter()
        .take(DETAILS_TRUNCATION)
        .map(|s| Value::String(s.clone()))
        .collect()
}

/// Writes the report to disk. Optional — the MCP caller may request the
/// report inline via the receipt without a persisted file.
pub fn write_report(path: &Path, report: &Value) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("mkdir {:?}: {}", parent, e))?;
    }
    let bytes = serde_json::to_vec_pretty(report)
        .map_err(|e| format!("serialize: {}", e))?;
    fs::write(path, bytes).map_err(|e| format!("write: {}", e))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests — cover verdict classification + SCC detection without a DB
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_regression_score_clean() {
        let s = DiffSummary::default();
        assert_eq!(regression_score(&s), 0.0);
        assert_eq!(classify_verdict(0.0), "clean");
    }

    #[test]
    fn test_regression_score_dangling_dominates() {
        let s = DiffSummary {
            dangling_references: 3,
            ..Default::default()
        };
        let score = regression_score(&s);
        assert!((score - 3.0).abs() < 1e-9, "got {score}");
        assert_eq!(classify_verdict(score), "concerning");
    }

    #[test]
    fn test_regression_score_cap() {
        let s = DiffSummary {
            dangling_references: 100,
            new_cycles: 100,
            ..Default::default()
        };
        let score = regression_score(&s);
        assert!(
            (score - REGRESSION_SCORE_CAP).abs() < 1e-9,
            "cap not enforced: got {score}"
        );
        assert_eq!(classify_verdict(score), "regression");
    }

    #[test]
    fn test_verdict_thresholds() {
        assert_eq!(classify_verdict(0.0), "clean");
        assert_eq!(classify_verdict(0.999), "clean");
        assert_eq!(classify_verdict(1.0), "concerning");
        assert_eq!(classify_verdict(4.999), "concerning");
        assert_eq!(classify_verdict(5.0), "regression");
        assert_eq!(classify_verdict(10.0), "regression");
    }

    #[test]
    fn test_tarjan_detects_self_loop() {
        let adj = vec![vec![0]];
        let sccs = tarjan_scc(&adj);
        // Single node with self-loop — tarjan returns it as its own SCC.
        assert_eq!(sccs.len(), 1);
        assert_eq!(sccs[0], vec![0]);
    }

    #[test]
    fn test_tarjan_detects_two_node_cycle() {
        // 0 -> 1 -> 0 — one SCC of size 2.
        let adj = vec![vec![1], vec![0]];
        let sccs = tarjan_scc(&adj);
        let big: Vec<_> = sccs.iter().filter(|s| s.len() == 2).collect();
        assert_eq!(big.len(), 1);
    }

    #[test]
    fn test_strongly_connected_cycles_finds_pair() {
        let mut nodes: NodeSet = BTreeMap::new();
        let mut fns: BTreeSet<String> = BTreeSet::new();
        fns.insert("a".into());
        fns.insert("b".into());
        nodes.insert("Function".into(), fns);
        let mut edges: EdgeSet = HashSet::new();
        edges.insert(("a".into(), "Calls".into(), "b".into()));
        edges.insert(("b".into(), "Calls".into(), "a".into()));
        let cycles = strongly_connected_cycles(&nodes, &edges);
        assert_eq!(cycles.len(), 1);
        assert!(cycles.iter().any(|c| c.contains("a,b") || c.contains("b,a")));
    }

    #[test]
    fn test_diff_cycle_sets_detects_new() {
        let mut before: BTreeSet<String> = BTreeSet::new();
        before.insert("a,b".into());
        let mut after: BTreeSet<String> = BTreeSet::new();
        after.insert("a,b".into());
        after.insert("c,d".into());
        let new = diff_cycle_sets(&before, &after);
        assert_eq!(new, vec!["c,d".to_string()]);
    }

    #[test]
    fn test_compute_dangling_flags_missing_target() {
        let mut nodes: NodeSet = BTreeMap::new();
        let mut fns: BTreeSet<String> = BTreeSet::new();
        fns.insert("a".into());
        nodes.insert("Function".into(), fns);
        let mut edges: EdgeSet = HashSet::new();
        edges.insert(("a".into(), "Calls".into(), "ghost".into()));
        let dangling = compute_dangling(&nodes, &edges);
        assert_eq!(dangling.len(), 1);
    }
}
