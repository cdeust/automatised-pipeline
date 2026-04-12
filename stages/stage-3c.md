# Stage 3c — Clustering + Processes

**Status:** spec (pre-implementation). Consumes 3b's resolved graph. Adds Community and Process nodes, MemberOf, ParticipatesIn, and EntryPointOf edges.

**Source:** stages/stage-3.md §3c, §5.1, §5.2, §11.5-11.7.

---

## 1. Shannon framing

**Question.** How do we make the graph *understand* — grouping related symbols into functional communities and tracing execution flows from entry points?

**Two sub-systems, two quantities:**

| Sub-system | Quantity | Operational definition |
|---|---|---|
| Clustering | Modularity Q | Q = (1/2m) * sum_ij [(A_ij - k_i*k_j/2m) * delta(c_i, c_j)]. Limit of the optimization procedure as the algorithm converges. Source: Newman 2004, "Finding and evaluating community structure in networks," Phys Rev E 69(2). |
| Process tracing | Process coverage P | P = |symbols in at least one process| / |total callable symbols|. Limit of BFS/DFS from all entry points as max_depth approaches the graph diameter. |

**Layer decomposition:**
- **Source:** the resolved graph from 3b — nodes with Calls, Imports, Implements, Extends, Uses edges.
- **Channel:** the LadybugDB graph store + Cypher query interface.
- **Code:** (A) the community detection algorithm mapping symbols to communities; (B) the BFS traversal mapping entry points to processes.

---

## 2. Clustering algorithm requirements

### 2.1 Axioms — what the algorithm MUST satisfy

The clustering algorithm is not prescribed. These properties constrain the choice:

| # | Property | Formal statement | Source |
|---|---|---|---|
| C1 | **Partition** | Every symbol node belongs to exactly one community. No symbol is unassigned. No symbol is in two communities. | stage-3.md §5.2 "N:1 per resolution level" |
| C2 | **Connectivity** | Every community is a connected subgraph when considering only intra-community edges. No community contains two disconnected components. | Traag, Waltman, van Eck 2019, "From Louvain to Leiden," Scientific Reports 9(1). Theorem 1: Leiden guarantees this; Louvain does not. |
| C3 | **Modularity optimization** | The algorithm must optimize modularity Q (Newman 2004) or a refinement thereof (e.g., the Constant Potts Model). The final Q must be reported. | Newman 2004 definition; Traag 2019 §2.1 |
| C4 | **Resolution parameter** | The algorithm must accept a resolution parameter gamma that controls community granularity. gamma=1.0 is the default. Higher gamma = more, smaller communities. | stage-3.md §11.5 input schema; Traag 2019 §2 |
| C5 | **Determinism** | Same graph + same gamma = same communities. Required for the pipeline's deterministic-indexing invariant (stage-3.md §13 invariant 1). | Pipeline invariant |
| C6 | **Scalability** | Must complete in O(n log n) time or better for graphs up to 50K nodes. | Fermi: a 500K-LOC Rust codebase has ~5K-15K symbol nodes; 50K is a 3x safety margin. |

### 2.2 What satisfies these axioms

| Algorithm | C1 | C2 | C3 | C4 | C5 | C6 | Source |
|---|---|---|---|---|---|---|---|
| **Leiden** | Yes | **Yes** | Yes | Yes | Yes (with fixed seed) | Yes — O(n log n) | Traag 2019 |
| **Louvain** | Yes | **No** — can produce disconnected communities | Yes | Yes | Yes (with fixed seed) | Yes — O(n log n) | Blondel et al. 2008, "Fast unfolding of communities in large networks," J Stat Mech P10008 |
| **Label propagation** | Yes | No guarantee | No — does not optimize Q | No | No — nondeterministic | Yes | Raghavan et al. 2007 |

**Conclusion:** Leiden satisfies all six axioms. Louvain fails C2. Label propagation fails C2, C3, C4, C5.

### 2.3 Algorithm decision boundary

The implementation must pick one method. The decision is:

1. **If lbug 0.15.x `algo` extension exposes Leiden:** use it. Zero new deps.
2. **If lbug `algo` exposes only Louvain:** use Louvain with a post-processing step that splits disconnected communities (restore C2). This adds ~30 LOC.
3. **If lbug `algo` exposes neither:** add `fa-leiden-cd` crate (MIT, pure Rust). Extract the adjacency list from the graph, run Leiden in-process, write results back.

**Current state (from dba evaluation):** Kuzu 0.11.3's algo extension has Louvain but NOT Leiden. lbug 0.15.3 inherits from Kuzu — likely same set, **unverified**. The engineer's first task is a smoke test: `CALL algo.louvain(...)` and `CALL algo.leiden(...)` against lbug 0.15.3.

**If Louvain only:** implement the C2 repair pass:
```
For each community C:
  Run connected-components on the subgraph induced by C.
  If components > 1:
    Split C into C_1, C_2, ..., C_k (one per component).
    Reassign MemberOf edges accordingly.
```
This is O(|C|) per community. Source: Traag 2019 §3.2 describes exactly this repair.

### 2.4 Input graph construction

Community detection operates on an **undirected, weighted** graph derived from the directed resolved graph:

| Directed edge kind | Weight | Rationale |
|---|---|---|
| Calls | 3.0 | Call edges are the strongest coupling signal. |
| Imports | 1.0 | Import indicates dependency but weaker coupling than a call. |
| Implements | 2.0 | Trait implementation binds a struct to its trait's community. |
| Uses | 1.0 | Type usage is structural coupling. |
| Extends | 2.0 | Supertrait relationship. |
| HasMethod, HasField, HasVariant | 5.0 | Containment edges — strongest affinity. |

**Directionality:** edges are treated as undirected for modularity computation. An edge A->B contributes weight w to both A's and B's adjacency.

**Node set:** all symbol nodes (Function, Method, Struct, Enum, Trait, Constant, TypeAlias, Module). File and Directory nodes are excluded — they are structural, not functional.

---

## 3. Process tracing algorithm

### 3.1 Entry point detection

Entry points are detected by pattern-matching node properties. For Rust:

| Entry kind | Detection rule | Source |
|---|---|---|
| `main` | Function node with name="main" and qualified_name ending in "::main" | Rust Reference §12.1 |
| `test` | Function node whose qualified_name is under a `#[cfg(test)]` module, OR whose name starts with "test_" in a test module | Rust Reference §11.1 |
| `async_main` | Function node with name="main" and is_async=true | tokio::main, actix_web::main patterns |
| `handler` | Public Function/Method whose qualified_name matches `do_*` pattern (project-specific: our MCP tool handlers) | Project convention from src/main.rs |
| `lib_entry` | Public Function at crate root (visibility="pub", qualified_name has exactly two segments: `file::name`) | Rust module system |

**Detection is heuristic.** Confidence:
- `main`, `test`: 1.0 (definite entry points)
- `async_main`: 0.95 (very likely)
- `handler`: 0.8 (project-specific convention)
- `lib_entry`: 0.6 (may or may not be externally called)

### 3.2 Call chain traversal

From each entry point, traverse `Calls` edges using **BFS** (breadth-first search):

```
procedure trace_process(entry_point, max_depth):
  queue = [(entry_point, 0)]
  visited = {entry_point}
  participants = [entry_point]
  
  while queue is not empty:
    (node, depth) = queue.pop_front()
    if depth >= max_depth:
      continue
    for target in outgoing_calls(node):
      if target not in visited:
        visited.add(target)
        participants.append(target)
        queue.push_back((target, depth + 1))
  
  return Process {
    entry_point: entry_point,
    participants: participants,
    depth: max(depth reached),
    symbol_count: len(participants)
  }
```

**Why BFS, not DFS:** BFS gives correct depth values (minimum distance from entry point). Depth is a required property on Process nodes (stage-3.md §5.1) and is used by get_impact for blast-radius grouping.

**Cycle handling:** the `visited` set prevents re-traversal. Recursive call chains are detected but not walked infinitely. A symbol participates in the process at its *first* (shallowest) encounter depth.

**Max depth:** default 20. Configurable. Source: Fermi estimate — the longest realistic call chain in a 10K-LOC Rust codebase is ~15 hops (main -> handler -> service -> helper -> utility -> ...). 20 provides headroom.

### 3.3 Overlap

A symbol can participate in multiple processes (stage-3.md §5.2: ParticipatesIn is N:M). This is expected — utility functions are called from many entry points. The Process node records which *entry point* owns it (1:1 via EntryPointOf).

### 3.4 Process naming

Each process is named after its entry point: `process::{entry_point_qualified_name}`. Example: `process::src/main.rs::main`, `process::src/main.rs::do_index_codebase`.

---

## 4. New node and edge schemas

### 4.1 Node tables (DDL)

```sql
CREATE NODE TABLE IF NOT EXISTS Community(
  id STRING,
  name STRING,
  algorithm STRING,
  resolution_param DOUBLE,
  member_count INT64,
  modularity_contribution DOUBLE,
  PRIMARY KEY(id)
)

CREATE NODE TABLE IF NOT EXISTS Process(
  id STRING,
  name STRING,
  entry_point_id STRING,
  entry_kind STRING,
  entry_confidence DOUBLE,
  depth INT64,
  symbol_count INT64,
  PRIMARY KEY(id)
)
```

### 4.2 Edge tables (DDL)

```sql
-- Symbol -> Community (N:1). Every symbol in exactly one community.
CREATE REL TABLE IF NOT EXISTS MemberOf_Function_Community(
  FROM Function TO Community)
CREATE REL TABLE IF NOT EXISTS MemberOf_Method_Community(
  FROM Method TO Community)
CREATE REL TABLE IF NOT EXISTS MemberOf_Struct_Community(
  FROM Struct TO Community)
CREATE REL TABLE IF NOT EXISTS MemberOf_Enum_Community(
  FROM Enum TO Community)
CREATE REL TABLE IF NOT EXISTS MemberOf_Trait_Community(
  FROM Trait TO Community)
CREATE REL TABLE IF NOT EXISTS MemberOf_Constant_Community(
  FROM Constant TO Community)
CREATE REL TABLE IF NOT EXISTS MemberOf_TypeAlias_Community(
  FROM TypeAlias TO Community)
CREATE REL TABLE IF NOT EXISTS MemberOf_Module_Community(
  FROM Module TO Community)

-- Function/Method -> Process (1:1 per process). Entry point.
CREATE REL TABLE IF NOT EXISTS EntryPointOf_Function_Process(
  FROM Function TO Process,
  confidence DOUBLE)
CREATE REL TABLE IF NOT EXISTS EntryPointOf_Method_Process(
  FROM Method TO Process,
  confidence DOUBLE)

-- Symbol -> Process (N:M). Participation.
CREATE REL TABLE IF NOT EXISTS ParticipatesIn_Function_Process(
  FROM Function TO Process,
  depth INT64)
CREATE REL TABLE IF NOT EXISTS ParticipatesIn_Method_Process(
  FROM Method TO Process,
  depth INT64)
```

### 4.3 ID generation

- Community: `community::{algorithm}::{resolution_param}::{sequence_number}` — deterministic from sorted member set hash.
- Process: `process::{entry_point_qualified_name}` — deterministic from the entry point.

---

## 5. MCP tools

Three tools, matching stage-3.md §11.5-11.7:

| Tool | Phase | Input | Output |
|---|---|---|---|
| `cluster_graph` | Clustering | run_id, finding_id, output_dir, resolution_param | community_count, modularity, elapsed_ms |
| `get_processes` | Process tracing | run_id, finding_id, output_dir, entry_kind | process_count, processes[] |
| `get_impact` | Blast radius | run_id, finding_id, output_dir, qualified_name, max_depth, min_confidence | root, affected[], affected_count, communities_affected |

**`get_impact` algorithm:** BFS from the target symbol, traversing Calls edges in reverse (who calls me?), grouped by depth. Each affected symbol is annotated with its community (via MemberOf). Output groups affected symbols by depth and by community.

**Phase ordering:** cluster_graph must complete before get_processes (precondition: "cluster" in phases_completed). get_impact requires both "cluster" and "process".

---

## 6. Invariants

| # | Invariant | Verification |
|---|---|---|
| I1 | Every symbol node has exactly one MemberOf edge after clustering | `MATCH (s) WHERE s:Function OR s:Method ... RETURN count(s) = (MATCH ()-[r:MemberOf_*]->() RETURN count(r))` |
| I2 | Every community has member_count = actual count of MemberOf edges pointing to it | Post-clustering validation query |
| I3 | Every community is a connected subgraph (C2 axiom) | Post-clustering validation: for each community, run WCC on its induced subgraph; assert components=1 |
| I4 | Every Process has exactly one EntryPointOf edge | `MATCH (p:Process) RETURN p.id, count{ (f)-[:EntryPointOf_*]->(p) } = 1` |
| I5 | No Process has zero ParticipatesIn edges | The entry point itself always participates |
| I6 | Process.symbol_count = count of ParticipatesIn edges | Post-tracing validation |
| I7 | phases_completed ordering: "cluster" before "process" | Enforced by tool preconditions |
| I8 | Determinism: same graph + same resolution_param = same communities | Required by stage-3.md §13 invariant 1 |

---

## 7. Implementation steps

Each step has a smoke test and LOC estimate. Total estimated: ~350 LOC in a new `src/clustering.rs` module.

| Step | Description | Smoke test | LOC |
|---|---|---|---|
| 1 | **Schema extension.** Add Community and Process node tables + all MemberOf/EntryPointOf/ParticipatesIn edge tables to `graph_store.rs`. | `cargo build` compiles; schema creation succeeds on fresh DB. | ~40 |
| 2 | **lbug algo probe.** Write a test that creates a 6-node graph and calls `CALL algo.louvain(...)` and `CALL algo.leiden(...)` via Cypher. Record which succeeds. | Test passes or fails — determines algorithm path. | ~30 |
| 3 | **Adjacency extraction.** Function to extract the weighted undirected adjacency list from the resolved graph (§2.4 weight table). | Unit test: 5 nodes, 4 edges -> correct adjacency with weights. | ~50 |
| 4 | **Clustering core.** If lbug has Louvain/Leiden: Cypher call + result parsing. If not: add `fa-leiden-cd` to Cargo.toml, call Leiden on extracted adjacency. | 6-node graph -> 2 communities, modularity > 0. | ~60 |
| 5 | **C2 repair pass.** If using Louvain: split disconnected communities using WCC. | Test with a known-disconnected community input. | ~30 |
| 6 | **Community persistence.** Create Community nodes and MemberOf edges. Update index.json with cluster_stats. | Query: all symbols have exactly one MemberOf edge. | ~40 |
| 7 | **Entry point detection.** Scan Function/Method nodes for main/test/handler/lib_entry patterns (§3.1). | Detect `main` and `do_*` handlers in our own codebase. | ~40 |
| 8 | **BFS process tracing.** Implement §3.2 BFS. Create Process nodes, EntryPointOf and ParticipatesIn edges. | Trace from `main` -> verify call chain includes expected functions. | ~50 |
| 9 | **get_impact implementation.** Reverse-BFS from a target symbol, annotate with communities. | Impact of `GraphStore::insert_node` -> returns callers grouped by depth. | ~40 |
| 10 | **MCP tool wiring.** Wire cluster_graph, get_processes, get_impact into do_* handlers in main.rs. | `echo '{"method":"tools/call","params":{"name":"cluster_graph",...}}' | cargo run` returns valid JSON. | ~30 |

---

## 8. Open questions

1. **lbug 0.15.3 algo extension scope.** Does `CALL algo.louvain(...)` work? Does `CALL algo.leiden(...)` work? **Resolution: Step 2 smoke test.** This is the single most important unknown; it determines whether we need an external crate.

2. **Edge weight tuning.** The weights in §2.4 are initial estimates. Should they be tuned empirically? **Recommendation: ship with these defaults, add a weight parameter to cluster_graph in a future iteration if needed.**

3. **Community naming heuristic.** Communities are currently identified by ID only. A human-readable name (e.g., "graph_store module", "MCP handler cluster") would improve tool output. **Deferred: derive names from the most common file or module among members. Not in v1.**

4. **Process deduplication.** If two entry points reach the exact same set of symbols, should they share a Process node? **No — each entry point gets its own Process per stage-3.md §5.2 "1:1" invariant on EntryPointOf.**

---

## 9. Source citations

| Claim | Source |
|---|---|
| Modularity Q definition | Newman, M.E.J. (2004). "Finding and evaluating community structure in networks." Physical Review E, 69(2), 026113. |
| Louvain algorithm | Blondel, V.D., Guillaume, J.-L., Lambiotte, R., Lefebvre, E. (2008). "Fast unfolding of communities in large networks." Journal of Statistical Mechanics, P10008. |
| Leiden algorithm, C2 guarantee | Traag, V.A., Waltman, L., van Eck, N.J. (2019). "From Louvain to Leiden: guaranteeing well-connected communities." Scientific Reports, 9(1), 5233. |
| Louvain in lbug/Kuzu algo extension | dba evaluation (stages/stage-3-db-evaluation.md §1, "Algo extension specifics") |
| Leiden absent from Kuzu 0.11.3 | dba evaluation (stages/stage-3-db-evaluation.md §1, "FALSIFIED") |
| lbug 0.15.3 extension status | Unverified — inherited from Kuzu, needs smoke test |
| fa-leiden-cd crate | crates.io/crates/fa-leiden-cd, MIT license |
| BFS for shortest-path depth | Cormen, Leiserson, Rivest, Stein (2009). Introduction to Algorithms, 3rd ed., §22.2 |
