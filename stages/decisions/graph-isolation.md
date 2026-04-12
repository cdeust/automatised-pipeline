# Decision: Graph Isolation Model — Per-Finding vs Per-Codebase

**Status:** decided
**Date:** 2026-04-11
**Method:** Lamport invariant analysis (spec-before-code, invariants-not-traces)
**Verdict:** Option A — graph-per-finding

---

## Scope

Correctness-critical component: stage-3 graph lifecycle (creation, querying, cleanup).

Rationale for formal rigor: the graph is mutable shared state (writes during indexing, reads during querying, deletes during cleanup). Two pipeline runs targeting the same codebase create a concurrency hazard. The combinatorics of interleaving exceed what can be tested; invariant reasoning is required.

---

## State

- **State variables per option:**
  - **Option A (per-finding):** `G[run_id, finding_id] -> GraphStore | absent`
  - **Option B (per-codebase):** `G[hash(codebase_path)] -> GraphStore | absent`, plus `refs[run_id, finding_id] -> hash(codebase_path)`

- **Initial state:** all graph slots absent.

---

## Option A — Graph-per-finding

### Safety invariant (what must never happen)

**A-S1.** A graph query for finding F must never return data from a graph indexed for finding F' (isolation).

**A-S2.** A graph write must never corrupt a graph being read by a concurrent query (no torn reads).

### Liveness invariant (what must eventually happen)

**A-L1.** After `index_codebase(run_id, finding_id, codebase_path)` returns `ok`, the graph at `runs/<run_id>/findings/<finding_id>/stage-3.graph/` is complete and queryable.

**A-L2.** After a finding is abandoned (aborted at stage 2), its graph directory can be deleted without affecting any other finding.

### Proof that invariants are satisfiable

1. **A-S1 holds trivially.** Each finding's graph lives at a unique filesystem path keyed by `(run_id, finding_id)`. No path aliasing is possible because `run_id` and `finding_id` are validated as filesystem-safe with no `/` or `..` (stage-1.md section 5.1.4). Path uniqueness implies data isolation. QED.

2. **A-S2 holds by construction.** Only one pipeline stage operates on one finding at a time (the pipeline is sequential per-finding: stage 1 -> stage 2 -> stage 3). No concurrent writer/reader on the same `(run_id, finding_id)` graph. No lock required.

3. **A-L1 holds.** `index_codebase` writes to a path determined solely by its own arguments. No dependency on external state. The only failure mode is disk I/O, which is handled by the existing atomic-write discipline (tempfile + rename).

4. **A-L2 holds.** `rm -rf runs/<run_id>/findings/<finding_id>/` deletes exactly one finding's artifacts. No dangling references exist because no other finding references this path.

**Mechanism cost:** zero locks, zero reference counters, zero staleness checks, zero shared-state coordination.

---

## Option B — Graph-per-codebase

### Safety invariant (what must never happen)

**B-S1.** A graph indexed for codebase C must reflect the state of C at the time of indexing, not a mixture of states from different indexing runs.

**B-S2.** A graph read during a concurrent re-index must return a consistent snapshot (no torn reads).

**B-S3.** Two concurrent `index_codebase` calls for the same codebase must not corrupt the graph.

### Liveness invariant (what must eventually happen)

**B-L1.** After `index_codebase` returns `ok`, the shared graph is queryable by any finding targeting this codebase.

**B-L2.** When no finding references a shared graph, it can be deleted (no leaks).

### Analysis: which invariants require mechanism?

1. **B-S1 — staleness.** The codebase may change between finding extraction (stage 1) and graph indexing (stage 3). Under Option B, a second finding may trigger a re-index of the same codebase that has since changed. Now the first finding's analysis is based on a graph that reflects a *later* state of the codebase than the finding was extracted from. To prevent this:
   - Need a **codebase version stamp** (git commit hash, or content hash of all files).
   - Need a **staleness check** at query time: "is this graph's version stamp compatible with the finding's extraction timestamp?"
   - Need a **policy** for what happens when stale: re-index (but now we have B-S3), refuse (but now liveness is blocked), or warn (but now correctness is degraded).

   **This is not a single mechanism; it is a protocol with at least three decision points.**

2. **B-S2 + B-S3 — concurrent access.** Two findings targeting the same codebase, running in parallel:
   - Finding F1 calls `index_codebase` at time T1.
   - Finding F2 calls `index_codebase` at time T2, while F1's indexing is still writing.
   - Without a write lock, the graph is corrupted (two writers to the same database files).
   - With a write lock, F2 blocks until F1 finishes. But: should F2 re-index (wasted work if F1 just indexed the same version) or reuse F1's result (needs the version stamp from B-S1)?

   **This requires:** a filesystem lock (flock or equivalent), a version comparison protocol, and a "skip if already indexed at this version" fast path.

3. **B-L2 — cleanup.** The graph is shared. Deleting it requires knowing that *no* finding still references it. This requires a **reference counter** or a **scan of all active findings**. When a finding is abandoned:
   - Decrement the reference count.
   - If zero, delete.
   - But: what if the decrement races with a new finding acquiring a reference? Need an atomic compare-and-delete.

   **This requires:** a reference-counted lifecycle manager with atomic operations.

**Mechanism cost:** file locks, version stamps, staleness detection, reference counting, atomic lifecycle management. Minimum five new components, each with its own failure modes.

---

## Concurrency scenario (model check, N=2)

**Setup:** Two pipeline runs R1 and R2, both targeting codebase `/repos/foo`, findings F1 and F2.

### Option A

| Time | R1 | R2 | Shared state |
|------|----|----|--------------|
| t1 | `index_codebase(R1, F1, /repos/foo)` starts | idle | G[R1,F1] being written |
| t2 | writing... | `index_codebase(R2, F2, /repos/foo)` starts | G[R1,F1] writing, G[R2,F2] writing |
| t3 | done | done | G[R1,F1] complete, G[R2,F2] complete |

**Conflict?** None. Different paths, no shared state. Both writes succeed independently. Disk cost: 2x graph size. CPU cost: 2x index time.

### Option B

| Time | R1 | R2 | Shared state |
|------|----|----|--------------|
| t1 | `index_codebase(R1, F1, /repos/foo)` starts | idle | G[hash(/repos/foo)] being written |
| t2 | writing... | `index_codebase(R2, F2, /repos/foo)` starts | **CONFLICT: two writers** |
| t3 | ??? | ??? | Graph integrity depends on lock protocol |

**Without lock:** graph corruption (B-S3 violated).
**With lock:** R2 blocks at t2, resumes at t3. Must then check version stamp to decide: reuse or re-index. If codebase changed between t1 and t2, R2 must re-index, blocking R1's queries during the re-index (B-S2 at risk unless the storage backend supports MVCC).

---

## Staleness scenario

**Setup:** Codebase `/repos/foo` is at commit `abc123` when finding F1 is extracted at stage 1. By the time stage 3 runs, the codebase is at commit `def456`.

### Option A

The graph is indexed at `def456`. This is a finding-local concern: the finding's analysis is based on the current codebase state. If the delta between `abc123` and `def456` matters, stage 3's `detect_changes` tool can diff them. The staleness is **visible and local** — it affects only F1.

### Option B

Same indexing at `def456`. But now: if F2 was extracted when the codebase was at `def456`, F2 sees a correct graph while F1 sees a graph that is stale *relative to its extraction*. Both share the same graph. The staleness is **invisible** unless the version stamp protocol actively checks it. And the check must happen at every query, not just at index time, because another finding might trigger a re-index at any moment.

---

## Cleanup scenario

**Setup:** Finding F1 is abandoned (aborted at stage 2). Its graph must be cleaned up.

### Option A

`rm -rf runs/<run_id>/findings/<F1>/stage-3.graph/`

Done. No coordination. No reference counting. No race conditions. The path is unique to F1; nothing else reads it.

### Option B

1. Decrement `refs[R1, F1]` from the shared graph's reference count.
2. Check if count == 0.
3. If yes, delete graph. But: between steps 2 and 3, a new finding might acquire a reference. Need atomic compare-and-delete.
4. If no, leave graph. But: what if all remaining references are from abandoned findings that haven't cleaned up yet? Need a garbage collector or periodic scan.

---

## Verdict

**Option A (graph-per-finding)** is the correct choice.

The deciding invariant is **A-S1 (isolation)**, which holds trivially under Option A and requires significant mechanism under Option B. Specifically:

- Option A's safety invariants are satisfied by **path uniqueness alone** — a property already guaranteed by the existing `(run_id, finding_id)` layout from stages 1-2.
- Option B's safety invariants (B-S1 staleness, B-S2 concurrent reads, B-S3 concurrent writes) each require a new protocol component, and those components interact (the lock protocol depends on the version stamp, the cleanup protocol depends on the reference counter, the staleness check depends on the lock protocol).

**The mechanism cost ratio is 0:5** — Option A requires zero new coordination mechanisms; Option B requires at minimum five (file locks, version stamps, staleness detection, reference counting, lifecycle management).

### The one scenario where Option B wins

Large codebase, many findings. If 20 findings target the same 10GB codebase, Option A indexes it 20 times (200GB disk, 20x CPU). Option B indexes it once (10GB disk, 1x CPU).

**Mitigation for Option A:** this is a performance optimization, not a correctness concern. If it becomes a real bottleneck:

1. **Short term:** add an optional `graph_source` parameter to `index_codebase` that points to an existing graph directory to copy/hardlink from, with a version stamp check. This is an explicit, user-controlled cache — not implicit shared state. The finding still owns its copy; A-S1 still holds.

2. **Long term:** introduce a read-only graph cache keyed by `(codebase_path, git_commit_hash)` that `index_codebase` checks before doing a full walk+parse. The cache is advisory (can be deleted at any time without breaking any finding), and the finding copies from it into its own `stage-3.graph/` directory. Isolation is preserved; indexing cost is amortized.

Neither mitigation requires any of Option B's coordination mechanisms.

### Consistency with existing layout

The current stage-3 spec (section 9) already places `stage-3.graph/` under `runs/<run_id>/findings/<finding_id>/`. This decision confirms and ratifies that layout. No spec change required.

---

## Hierarchical proof summary

1. Option A's invariants (A-S1, A-S2, A-L1, A-L2) are satisfiable.
   1.1. A-S1 holds by path uniqueness (stage-1.md section 5.1.4 guarantees filesystem-safe IDs).
   1.2. A-S2 holds by single-writer-per-finding (pipeline is sequential per finding).
   1.3. A-L1 holds by atomic-write discipline (inherited from stages 1-2).
   1.4. A-L2 holds by path containment (finding directory contains all finding artifacts).
2. Option B's invariants (B-S1, B-S2, B-S3, B-L1, B-L2) require mechanism.
   2.1. B-S3 requires file locks (concurrent writers to shared graph).
   2.2. B-S1 requires version stamps + staleness protocol.
   2.3. B-S2 requires MVCC or read-lock coordination.
   2.4. B-L2 requires reference counting with atomic lifecycle management.
   2.5. These mechanisms interact: 2.1 depends on 2.2 (lock holder must check version), 2.4 depends on 2.1 (reference count must be updated under lock).
3. Option A's invariants are satisfiable with less mechanism.
   3.1. By 1, Option A needs 0 new mechanisms.
   3.2. By 2, Option B needs >= 5 new mechanisms.
   3.3. Fewer mechanisms = fewer failure modes = higher probability of correctness. QED.
