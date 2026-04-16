// clustering — Stage 3c: community detection + process tracing.
//
// Clustering: Louvain algorithm (Blondel et al. 2008, J Stat Mech P10008)
// implemented in Rust inline (Branch C — lbug 0.15.3 algo extension
// requires network download, unavailable in-process).
// Post-processing: C2 repair pass splits disconnected communities
// (Traag et al. 2019, Scientific Reports 9(1), 5233, section 3.2).
//
// Process tracing: BFS from entry points along Calls edges.
// source: stages/stage-3c.md §2, §3

use crate::graph_store::GraphStore;
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Instant;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

pub struct ClusteringResult {
    pub communities: u64,
    pub modularity: f64,
    pub processes: u64,
    pub elapsed_ms: u64,
}

pub struct ProcessInfo {
    pub name: String,
    pub entry_point: String,
    pub entry_kind: String,
    pub depth: u64,
    pub node_count: u64,
}

pub struct ImpactResult {
    pub communities: Vec<String>,
    pub processes: Vec<String>,
}

// ---------------------------------------------------------------------------
// Edge weight table — source: stages/stage-3c.md §2.4
// ---------------------------------------------------------------------------

fn edge_weight(rel_name: &str) -> f64 {
    if rel_name.starts_with("Calls_") {
        3.0
    } else if rel_name.starts_with("Implements_") || rel_name.starts_with("Extends_") {
        2.0
    } else if rel_name.starts_with("Imports_") || rel_name.starts_with("Uses_") {
        1.0
    } else if rel_name.starts_with("HasMethod_")
        || rel_name.starts_with("HasField_")
        || rel_name.starts_with("HasVariant_")
    {
        5.0
    } else {
        0.0
    }
}

// ---------------------------------------------------------------------------
// Symbol labels eligible for clustering — source: stage-3c.md §2.4
// ---------------------------------------------------------------------------

const SYMBOL_LABELS: &[&str] = &[
    "Function", "Method", "Struct", "Enum", "Trait",
    "Constant", "TypeAlias", "Module",
];

// ---------------------------------------------------------------------------
// Adjacency extraction — source: stages/stage-3c.md §2.4
// ---------------------------------------------------------------------------

struct Adjacency {
    node_ids: Vec<String>,
    node_labels: Vec<String>,
    #[allow(dead_code)] // used by tests for constructing test adjacencies
    id_to_idx: HashMap<String, usize>,
    neighbors: Vec<Vec<(usize, f64)>>,
    total_weight: f64,
}

fn extract_adjacency(store: &GraphStore) -> Result<Adjacency, String> {
    let (node_ids, node_labels, id_to_idx) = collect_symbol_nodes(store)?;
    let n = node_ids.len();
    let (neighbors, total_weight) = collect_weighted_edges(store, &id_to_idx, n)?;
    Ok(Adjacency { node_ids, node_labels, id_to_idx, neighbors, total_weight })
}

fn collect_symbol_nodes(
    store: &GraphStore,
) -> Result<(Vec<String>, Vec<String>, HashMap<String, usize>), String> {
    let mut ids = Vec::new();
    let mut labels = Vec::new();
    let mut map: HashMap<String, usize> = HashMap::new();
    for label in SYMBOL_LABELS {
        let cypher = format!("MATCH (n:{label}) RETURN n.id");
        let qr = match store.execute_query(&cypher) {
            Ok(q) => q,
            Err(_) => continue,
        };
        for row in &qr.rows {
            if row.is_empty() { continue; }
            let id = &row[0];
            if !map.contains_key(id) {
                map.insert(id.clone(), ids.len());
                ids.push(id.clone());
                labels.push(label.to_string());
            }
        }
    }
    Ok((ids, labels, map))
}

fn collect_weighted_edges(
    store: &GraphStore, id_to_idx: &HashMap<String, usize>, n: usize,
) -> Result<(Vec<Vec<(usize, f64)>>, f64), String> {
    let mut neighbors: Vec<Vec<(usize, f64)>> = vec![Vec::new(); n];
    let mut total_weight = 0.0;
    for &(rel, from_label, to_label) in edge_rel_tables() {
        let w = edge_weight(rel);
        if w == 0.0 { continue; }
        let cypher = format!(
            "MATCH (a:{from_label})-[:{rel}]->(b:{to_label}) RETURN a.id, b.id"
        );
        let qr = match store.execute_query(&cypher) { Ok(q) => q, Err(_) => continue };
        for row in &qr.rows {
            if row.len() < 2 { continue; }
            if let (Some(&a), Some(&b)) = (id_to_idx.get(&row[0]), id_to_idx.get(&row[1])) {
                neighbors[a].push((b, w));
                neighbors[b].push((a, w));
                total_weight += w;
            }
        }
    }
    Ok((neighbors, total_weight))
}

fn edge_rel_tables() -> &'static [(&'static str, &'static str, &'static str)] {
    &[
        ("Calls_Function_Function", "Function", "Function"),
        ("Calls_Function_Method", "Function", "Method"),
        ("Calls_Method_Function", "Method", "Function"),
        ("Calls_Method_Method", "Method", "Method"),
        ("Imports_File_Function", "File", "Function"),
        ("Imports_File_Struct", "File", "Struct"),
        ("Imports_File_Enum", "File", "Enum"),
        ("Imports_File_Trait", "File", "Trait"),
        ("Implements_Struct_Trait", "Struct", "Trait"),
        ("Implements_Enum_Trait", "Enum", "Trait"),
        ("Extends_Trait_Trait", "Trait", "Trait"),
        ("Uses_Function_Struct", "Function", "Struct"),
        ("Uses_Function_Enum", "Function", "Enum"),
        ("Uses_Function_Trait", "Function", "Trait"),
        ("Uses_Method_Struct", "Method", "Struct"),
        ("Uses_Method_Enum", "Method", "Enum"),
        ("Uses_Method_Trait", "Method", "Trait"),
        ("HasMethod_Struct_Method", "Struct", "Method"),
        ("HasMethod_Enum_Method", "Enum", "Method"),
        ("HasMethod_Trait_Method", "Trait", "Method"),
        ("HasField_Struct_Field", "Struct", "Field"),
        ("HasField_Enum_Field", "Enum", "Field"),
        ("HasVariant_Enum_Variant", "Enum", "Variant"),
    ]
}

// ---------------------------------------------------------------------------
// Louvain algorithm — Blondel et al. 2008
// source: "Fast unfolding of communities in large networks"
// ---------------------------------------------------------------------------

fn louvain(adj: &Adjacency, gamma: f64) -> (Vec<usize>, f64) {
    let n = adj.node_ids.len();
    if n == 0 {
        return (vec![], 0.0);
    }
    let m = adj.total_weight; // sum of edge weights (each undirected edge once)
    if m == 0.0 {
        return ((0..n).collect(), 0.0);
    }
    let two_m = 2.0 * m; // Newman's 2m: sum of degrees = 2 * sum of edge weights

    // k[i] = sum of neighbor weights for node i (undirected degree)
    let k: Vec<f64> = adj.neighbors.iter()
        .map(|nbrs| nbrs.iter().map(|(_, w)| w).sum())
        .collect();

    let mut comm: Vec<usize> = (0..n).collect();
    // sigma_tot[c] = sum of degrees of nodes in community c
    let mut sigma_tot: Vec<f64> = k.clone();
    let max_passes = 100;

    for _ in 0..max_passes {
        let mut improved = false;
        for i in 0..n {
            let old_c = comm[i];
            let ki = k[i];

            // Weights from i to each neighboring community
            let mut ki_in: HashMap<usize, f64> = HashMap::new();
            for &(nbr, w) in &adj.neighbors[i] {
                *ki_in.entry(comm[nbr]).or_insert(0.0) += w;
            }

            // Remove i from its community for gain computation
            sigma_tot[old_c] -= ki;

            // Gain = ki_in_c - gamma * sigma_tot_c * ki / (2m)
            // source: Blondel 2008 eq. from section III
            let ki_in_old = ki_in.get(&old_c).copied().unwrap_or(0.0);
            let mut best_c = old_c;
            let mut best_gain = ki_in_old - gamma * sigma_tot[old_c] * ki / two_m;

            for (&c, &ki_in_c) in &ki_in {
                let gain = ki_in_c - gamma * sigma_tot[c] * ki / two_m;
                if gain > best_gain {
                    best_gain = gain;
                    best_c = c;
                }
            }

            comm[i] = best_c;
            sigma_tot[best_c] += ki;
            if best_c != old_c { improved = true; }
        }
        if !improved { break; }
    }

    let comm = renumber_communities(&comm);
    let q = compute_modularity(&adj.neighbors, &comm, &k, m);
    (comm, q)
}

fn renumber_communities(comm: &[usize]) -> Vec<usize> {
    let mut map: HashMap<usize, usize> = HashMap::new();
    let mut next = 0;
    let mut result = Vec::with_capacity(comm.len());
    for &c in comm {
        let new_c = *map.entry(c).or_insert_with(|| {
            let v = next;
            next += 1;
            v
        });
        result.push(new_c);
    }
    result
}

/// Newman 2004: Q = (1/2m) * sum_ij [A_ij - ki*kj/(2m)] * delta(ci,cj)
/// `m` = sum of undirected edge weights (each edge counted once).
fn compute_modularity(
    neighbors: &[Vec<(usize, f64)>],
    comm: &[usize],
    k: &[f64],
    m: f64,
) -> f64 {
    if m == 0.0 { return 0.0; }
    let two_m = 2.0 * m;
    let mut q = 0.0;
    // neighbors stores both directions, so the loop sums each pair (i,j) twice.
    // This cancels with the 1/(2m) factor, leaving division by two_m once.
    for (i, nbrs) in neighbors.iter().enumerate() {
        for &(j, w) in nbrs {
            if comm[i] == comm[j] {
                q += w - k[i] * k[j] / two_m;
            }
        }
    }
    q / two_m
}

// ---------------------------------------------------------------------------
// C2 repair: split disconnected communities — Traag 2019 §3.2
// ---------------------------------------------------------------------------

fn repair_c2(adj: &Adjacency, comm: &mut Vec<usize>) {
    let n = comm.len();
    let num_comms = comm.iter().copied().max().map_or(0, |m| m + 1);
    let mut next_comm = num_comms;

    for c in 0..num_comms {
        let members: Vec<usize> = (0..n).filter(|&i| comm[i] == c).collect();
        if members.len() <= 1 { continue; }

        let components = connected_components_within(&members, &adj.neighbors, comm, c);
        if components.len() <= 1 { continue; }

        // Keep first component as c, assign rest new IDs
        for component in components.iter().skip(1) {
            for &node in component {
                comm[node] = next_comm;
            }
            next_comm += 1;
        }
    }
    *comm = renumber_communities(comm);
}

fn connected_components_within(
    members: &[usize],
    neighbors: &[Vec<(usize, f64)>],
    comm: &[usize],
    community: usize,
) -> Vec<Vec<usize>> {
    let member_set: HashSet<usize> = members.iter().copied().collect();
    let mut visited = HashSet::new();
    let mut components = Vec::new();

    for &start in members {
        if visited.contains(&start) { continue; }
        let mut component = Vec::new();
        let mut queue = VecDeque::new();
        queue.push_back(start);
        visited.insert(start);
        while let Some(node) = queue.pop_front() {
            component.push(node);
            for &(nbr, _) in &neighbors[node] {
                if member_set.contains(&nbr)
                    && comm[nbr] == community
                    && visited.insert(nbr)
                {
                    queue.push_back(nbr);
                }
            }
        }
        components.push(component);
    }
    components
}

// ---------------------------------------------------------------------------
// Persist communities to graph — source: stages/stage-3c.md §4
// ---------------------------------------------------------------------------

fn persist_communities(
    store: &GraphStore,
    adj: &Adjacency,
    comm: &[usize],
    modularity: f64,
    gamma: f64,
) -> Result<u64, String> {
    let num_comms = comm.iter().copied().max().map_or(0, |m| m + 1);

    // Count members per community
    let mut counts: HashMap<usize, u64> = HashMap::new();
    for &c in comm {
        *counts.entry(c).or_insert(0) += 1;
    }

    // Create Community nodes (bulk-insert).
    // source: Fermi audit April 2026 — was per-row CREATE, now batched.
    let community_rows: Vec<Vec<(String, String)>> = (0..num_comms)
        .map(|c| {
            let count = counts.get(&c).copied().unwrap_or(0);
            let cid = format!("community::louvain::{gamma}::{c}");
            let cid_esc = cid.replace('\'', "\\'");
            vec![
                ("id".into(), format!("'{cid_esc}'")),
                ("name".into(), format!("'community_{c}'")),
                ("algorithm".into(), "'louvain+c2'".into()),
                ("resolution_param".into(), gamma.to_string()),
                ("member_count".into(), count.to_string()),
                ("modularity_contribution".into(), format!("{:.6}", modularity)),
            ]
        })
        .collect();
    store.bulk_insert_nodes("Community", &community_rows)?;

    // Create MemberOf edges grouped per rel table.
    let mut by_rel: HashMap<String, Vec<(String, String, Vec<(String, String)>)>> =
        HashMap::new();
    for (idx, &c) in comm.iter().enumerate() {
        let node_id = &adj.node_ids[idx];
        let label = &adj.node_labels[idx];
        let cid = format!("community::louvain::{gamma}::{c}");
        let rel = format!("MemberOf_{label}_Community");
        by_rel.entry(rel).or_default().push((node_id.clone(), cid, Vec::new()));
    }
    for (rel, edges) in &by_rel {
        store.bulk_insert_edges(rel, edges)?;
    }
    Ok(num_comms as u64)
}

// ---------------------------------------------------------------------------
// Entry point: cluster_graph — source: stages/stage-3c.md §5
// ---------------------------------------------------------------------------

pub fn cluster_graph(
    store: &GraphStore,
    gamma: f64,
) -> Result<ClusteringResult, String> {
    let start = Instant::now();
    let adj = extract_adjacency(store)?;

    let (mut comm, modularity) = louvain(&adj, gamma);
    repair_c2(&adj, &mut comm);

    let communities = persist_communities(store, &adj, &comm, modularity, gamma)?;
    let processes = trace_processes(store)?;

    Ok(ClusteringResult {
        communities,
        modularity,
        processes,
        elapsed_ms: start.elapsed().as_millis() as u64,
    })
}

// ---------------------------------------------------------------------------
// Entry point detection — source: stages/stage-3c.md §3.1
// ---------------------------------------------------------------------------

struct EntryPoint {
    id: String,
    label: String,
    #[allow(dead_code)]
    name: String,
    qualified_name: String,
    kind: String,
    confidence: f64,
}

fn detect_entry_points(store: &GraphStore) -> Result<Vec<EntryPoint>, String> {
    let mut entries = Vec::new();
    query_entries(store, "f.name = 'main'", "main", 1.0, &mut entries)?;
    query_entries(store, "f.name STARTS WITH 'test_'", "test", 1.0, &mut entries)?;
    query_entries(store, "f.name STARTS WITH 'do_'", "handler", 0.8, &mut entries)?;
    detect_lib_entries(store, &mut entries)?;
    Ok(entries)
}

fn query_entries(
    store: &GraphStore, filter: &str, kind: &str,
    confidence: f64, out: &mut Vec<EntryPoint>,
) -> Result<(), String> {
    let cypher = format!(
        "MATCH (f:Function) WHERE {filter} RETURN f.id, f.name, f.qualified_name"
    );
    let qr = store.execute_query(&cypher)?;
    for row in &qr.rows {
        if row.len() < 3 { continue; }
        out.push(EntryPoint {
            id: row[0].clone(), label: "Function".into(),
            name: row[1].clone(), qualified_name: row[2].clone(),
            kind: kind.into(), confidence,
        });
    }
    Ok(())
}

fn detect_lib_entries(
    store: &GraphStore, entries: &mut Vec<EntryPoint>,
) -> Result<(), String> {
    let qr = store.execute_query(
        "MATCH (f:Function) WHERE f.visibility = 'pub' \
         RETURN f.id, f.name, f.qualified_name"
    )?;
    for row in &qr.rows {
        if row.len() < 3 { continue; }
        let qn = &row[2];
        let segments: Vec<&str> = qn.split("::").collect();
        if segments.len() == 2 && segments[0].ends_with(".rs") {
            let already = entries.iter().any(|e| e.id == row[0]);
            if !already {
                entries.push(EntryPoint {
                    id: row[0].clone(), label: "Function".into(),
                    name: row[1].clone(), qualified_name: qn.clone(),
                    kind: "lib_entry".into(), confidence: 0.6,
                });
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// BFS process tracing — source: stages/stage-3c.md §3.2
// ---------------------------------------------------------------------------

const MAX_BFS_DEPTH: u64 = 20;

fn trace_one_process(
    store: &GraphStore,
    entry: &EntryPoint,
    call_edges: &HashMap<String, Vec<String>>,
    id_to_label: &HashMap<String, String>,
) -> Result<ProcessInfo, String> {
    let process_id = format!("process::{}", entry.qualified_name);
    let (visited, max_depth) = bfs_from_entry(&entry.id, call_edges);
    persist_process_node(store, entry, &process_id, &visited, max_depth)?;
    persist_participates_in(store, &visited, &process_id, id_to_label)?;

    Ok(ProcessInfo {
        name: process_id,
        entry_point: entry.qualified_name.clone(),
        entry_kind: entry.kind.clone(),
        depth: max_depth,
        node_count: visited.len() as u64,
    })
}

fn bfs_from_entry(
    start_id: &str,
    call_edges: &HashMap<String, Vec<String>>,
) -> (HashSet<String>, u64) {
    let mut visited = HashSet::new();
    let mut queue: VecDeque<(String, u64)> = VecDeque::new();
    let mut max_depth: u64 = 0;

    visited.insert(start_id.to_string());
    queue.push_back((start_id.to_string(), 0));

    while let Some((node_id, depth)) = queue.pop_front() {
        if depth > max_depth { max_depth = depth; }
        if depth >= MAX_BFS_DEPTH { continue; }
        if let Some(targets) = call_edges.get(&node_id) {
            for target in targets {
                if visited.insert(target.clone()) {
                    queue.push_back((target.clone(), depth + 1));
                }
            }
        }
    }
    (visited, max_depth)
}

fn persist_process_node(
    store: &GraphStore, entry: &EntryPoint, process_id: &str,
    visited: &HashSet<String>, max_depth: u64,
) -> Result<(), String> {
    let proc_esc = process_id.replace('\'', "\\'");
    store.insert_node("Process", &[
        ("id", &format!("'{proc_esc}'")),
        ("name", &format!("'{proc_esc}'")),
        ("entry_point_id", &format!("'{}'", entry.id.replace('\'', "\\'"))),
        ("entry_kind", &format!("'{}'", entry.kind)),
        ("entry_confidence", &entry.confidence.to_string()),
        ("depth", &max_depth.to_string()),
        ("symbol_count", &visited.len().to_string()),
    ])?;
    let ep_rel = format!("EntryPointOf_{}_Process", entry.label);
    store.insert_edge(&ep_rel, &entry.id, process_id, &[
        ("confidence", &entry.confidence.to_string()),
    ])
}

fn persist_participates_in(
    store: &GraphStore,
    visited: &HashSet<String>,
    process_id: &str,
    id_to_label: &HashMap<String, String>,
) -> Result<(), String> {
    // Group edges by rel table, bulk-insert per group.
    // source: Fermi audit April 2026 — was 2 probe queries per visited node
    //         plus a per-row CREATE; now zero probes and one bulk call per rel.
    let mut by_rel: HashMap<String, Vec<(String, String, Vec<(String, String)>)>> =
        HashMap::new();
    for node_id in visited {
        let lbl = match id_to_label.get(node_id) {
            Some(l) => l,
            None => continue,
        };
        let rel = format!("ParticipatesIn_{lbl}_Process");
        by_rel
            .entry(rel)
            .or_default()
            .push((
                node_id.clone(),
                process_id.to_string(),
                vec![("depth".into(), "0".into())],
            ));
    }
    for (rel, edges) in &by_rel {
        // Ignore errors: some labels may lack a ParticipatesIn table.
        let _ = store.bulk_insert_edges(rel, edges);
    }
    Ok(())
}

/// Loads id -> label for every Function and Method in the graph. One scan
/// per label (two total) replaces the per-node probes that used to fire
/// inside persist_participates_in.
fn build_call_target_labels(
    store: &GraphStore,
) -> Result<HashMap<String, String>, String> {
    let mut map = HashMap::new();
    for label in &["Function", "Method"] {
        let cypher = format!("MATCH (n:{label}) RETURN n.id");
        let qr = match store.execute_query(&cypher) {
            Ok(q) => q,
            Err(_) => continue,
        };
        for row in &qr.rows {
            if !row.is_empty() {
                map.insert(row[0].clone(), label.to_string());
            }
        }
    }
    Ok(map)
}

fn collect_call_edges(
    store: &GraphStore,
) -> Result<HashMap<String, Vec<String>>, String> {
    let mut edges: HashMap<String, Vec<String>> = HashMap::new();
    let call_rels = &[
        ("Calls_Function_Function", "Function", "Function"),
        ("Calls_Function_Method", "Function", "Method"),
        ("Calls_Method_Function", "Method", "Function"),
        ("Calls_Method_Method", "Method", "Method"),
    ];
    for &(rel, from_label, to_label) in call_rels {
        let cypher = format!(
            "MATCH (a:{from_label})-[:{rel}]->(b:{to_label}) RETURN a.id, b.id"
        );
        let qr = match store.execute_query(&cypher) {
            Ok(q) => q,
            Err(_) => continue,
        };
        for row in &qr.rows {
            if row.len() >= 2 {
                edges.entry(row[0].clone()).or_default().push(row[1].clone());
            }
        }
    }
    Ok(edges)
}

// ---------------------------------------------------------------------------
// Entry point: trace_processes — source: stages/stage-3c.md §3
// ---------------------------------------------------------------------------

pub fn trace_processes(store: &GraphStore) -> Result<u64, String> {
    let entries = detect_entry_points(store)?;
    let call_edges = collect_call_edges(store)?;
    // Single scan: build id -> label once so persist_participates_in needs
    // zero database probes. source: Fermi audit April 2026.
    let id_to_label = build_call_target_labels(store)?;
    let mut count = 0u64;
    for entry in &entries {
        trace_one_process(store, entry, &call_edges, &id_to_label)?;
        count += 1;
    }
    Ok(count)
}

// ---------------------------------------------------------------------------
// get_processes — list all processes
// ---------------------------------------------------------------------------

pub fn get_processes(store: &GraphStore) -> Result<Vec<ProcessInfo>, String> {
    let qr = store.execute_query(
        "MATCH (p:Process) RETURN p.id, p.name, p.entry_point_id, \
         p.entry_kind, p.depth, p.symbol_count"
    )?;
    let mut result = Vec::new();
    for row in &qr.rows {
        if row.len() < 6 { continue; }
        result.push(ProcessInfo {
            name: row[1].clone(),
            entry_point: row[2].clone(),
            entry_kind: row[3].clone(),
            depth: row[4].parse().unwrap_or(0),
            node_count: row[5].parse().unwrap_or(0),
        });
    }
    Ok(result)
}

// ---------------------------------------------------------------------------
// get_impact — blast radius for a symbol
// source: stages/stage-3c.md §5 get_impact
// ---------------------------------------------------------------------------

pub fn get_impact(
    store: &GraphStore,
    qualified_name: &str,
) -> Result<ImpactResult, String> {
    let esc = qualified_name.replace('\'', "\\'");

    // Find communities this symbol belongs to
    let mut communities = Vec::new();
    for label in SYMBOL_LABELS {
        let rel = format!("MemberOf_{label}_Community");
        let cypher = format!(
            "MATCH (n:{label})-[:{rel}]->(c:Community) \
             WHERE n.id = '{esc}' OR n.qualified_name = '{esc}' \
             RETURN c.id"
        );
        if let Ok(qr) = store.execute_query(&cypher) {
            for row in &qr.rows {
                if !row.is_empty() { communities.push(row[0].clone()); }
            }
        }
    }

    // Find processes this symbol participates in
    let mut processes = Vec::new();
    for label in &["Function", "Method"] {
        let rel = format!("ParticipatesIn_{label}_Process");
        let cypher = format!(
            "MATCH (n:{label})-[:{rel}]->(p:Process) \
             WHERE n.id = '{esc}' OR n.qualified_name = '{esc}' \
             RETURN p.name"
        );
        if let Ok(qr) = store.execute_query(&cypher) {
            for row in &qr.rows {
                if !row.is_empty() { processes.push(row[0].clone()); }
            }
        }
    }

    Ok(ImpactResult { communities, processes })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_louvain_two_cliques() {
        // Two 3-node cliques connected by one bridge edge
        let node_ids: Vec<String> = (0..6).map(|i| format!("n{i}")).collect();
        let node_labels: Vec<String> = vec!["Function".into(); 6];
        let id_to_idx: HashMap<String, usize> = node_ids.iter()
            .enumerate().map(|(i, id)| (id.clone(), i)).collect();

        let mut neighbors: Vec<Vec<(usize, f64)>> = vec![Vec::new(); 6];
        let edges = [(0,1), (1,2), (0,2), (3,4), (4,5), (3,5), (2,3)];
        let mut total_weight = 0.0;
        for &(a, b) in &edges {
            neighbors[a].push((b, 3.0));
            neighbors[b].push((a, 3.0));
            total_weight += 3.0;
        }

        let adj = Adjacency {
            node_ids, node_labels, id_to_idx, neighbors, total_weight,
        };
        let (mut comm, q) = louvain(&adj, 1.0);
        repair_c2(&adj, &mut comm);

        // Should find 2 communities
        let unique: HashSet<usize> = comm.iter().copied().collect();
        assert!(
            unique.len() == 2,
            "expected 2 communities, got {} (comm: {:?})",
            unique.len(), comm
        );
        // Nodes 0,1,2 in same community
        assert_eq!(comm[0], comm[1]);
        assert_eq!(comm[1], comm[2]);
        // Nodes 3,4,5 in same community
        assert_eq!(comm[3], comm[4]);
        assert_eq!(comm[4], comm[5]);
        // Different communities
        assert_ne!(comm[0], comm[3]);
        assert!(q > 0.0, "modularity should be positive, got {q}");
    }

    #[test]
    fn test_renumber_communities() {
        let comm = vec![5, 5, 3, 3, 5, 10];
        let renumbered = renumber_communities(&comm);
        assert_eq!(renumbered[0], renumbered[1]);
        assert_eq!(renumbered[2], renumbered[3]);
        assert_eq!(renumbered[0], renumbered[4]);
        let unique: HashSet<usize> = renumbered.iter().copied().collect();
        assert_eq!(unique.len(), 3);
    }
}
