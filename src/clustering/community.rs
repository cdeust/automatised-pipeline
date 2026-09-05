// clustering::community — Louvain + C2 functional-community detection.
// source: stages/stage-3c.md.
//
// This file is the composition root: the public `ClusteringResult` type and
// the `cluster_graph` entry point that wires adjacency extraction, Louvain,
// C2 repair, and persistence together. The mechanics live in sibling
// modules (Fowler "Move Function", split to keep every file under the
// §4.1 500-line cap — this file was 668 lines before the split).

use crate::graph_store::{cypher_str, GraphStore, PropEdgeList};
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Instant;

use super::process::trace_processes;

#[path = "community_adjacency.rs"]
mod community_adjacency;
#[path = "community_louvain.rs"]
mod community_louvain;
#[path = "community_membership.rs"]
mod community_membership;
#[path = "community_persist.rs"]
mod community_persist;
#[path = "community_repair.rs"]
mod community_repair;

use community_adjacency::*;
use community_louvain::*;
pub use community_membership::*;
use community_persist::*;
use community_repair::*;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

pub struct ClusteringResult {
    pub communities: u64,
    pub modularity: f64,
    pub processes: u64,
    pub elapsed_ms: u64,
}

// ---------------------------------------------------------------------------
// Entry point: cluster_graph — source: stages/stage-3c.md §5
// ---------------------------------------------------------------------------

pub fn cluster_graph(store: &GraphStore, gamma: f64) -> Result<ClusteringResult, String> {
    let start = Instant::now();
    store.require_entry_metadata()?;
    purge_prior_clustering(store)?;
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
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "community_tests.rs"]
mod tests;
