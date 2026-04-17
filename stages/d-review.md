# B1/B2/B3 Review — 0.666 → 0.845

## 1. Verdict
**APPROVED-WITH-CHANGES** — functionally correct, all regression tests pass, three-registry rel-table consistency restored, no security regression. Two non-critical must-fixes around determinism and one 40-LOC body overrun.

## 2. Per-fix
- **B1 (multi-brace `use`)** — **CORRECT**. `collect_use_leaves` correctly handles `use_list`, `scoped_use_list`, `use_as_clause`, `use_wildcard`, `self`-in-brace, nested braces, and aliases. Raw-brace regression assertion is in place. Termination is guaranteed (tree-sitter tree is finite/acyclic). Aliased name uses alias as display name; non-aliased keeps full path. Edge case `use {a, b};` is not valid Rust but would not panic (empty prefix → bare leaves).
- **B2 (cluster_graph mapping)** — **PARTIAL**. Correct behavior and wiring; integration test proves non-empty mapping, correct truncation flag, and valid cluster-id parsing. But `collect_cluster_memberships` does **not sort entries before truncating at 10k** — iteration over `MEMBEROF_LABELS` is deterministic but Kuzu row order per query is not guaranteed. On large graphs exceeding the cap, the truncated set is non-deterministic run-to-run, which breaks Q12 ARI reproducibility.
- **B3 (harness Q13 scorer)** — **CORRECT**. `extract_axis_set` deduplicates/sorts, `.get()` chain is panic-safe, `parse_tool_payload` uses `from_str + map_err` (no unwrap). PRD fixture shape matches `prd_validator::load_claims` contract (`affected_symbols[].qualified_name`, `scope_claims`). Test passes.

## 3. Security regression — **NO**
All four CRITICAL fixes intact: (1) new Cypher in `collect_cluster_memberships` interpolates only `MEMBEROF_LABELS` (compile-time const) — no user input, `cypher_str` not needed; (2) no new subprocess/file-open paths; (3) new rel-table names (`Defines_File_Import`, `Defines_Module_Import`) are compile-time constants in all three registries; (4) no new `remove_dir_all`/`create_dir_all` on caller input.

## 4. Three-registry consistency — **YES**
`graph_store::REL_TABLES`, `main::rel_table_triples()`, and `indexer::is_valid_rel_table` all gained the same two entries (`Defines_File_Import`, `Defines_Module_Import`). Verified by diff.

## 5. Function-size audit
**`collect_use_leaves` in src/parser/rust.rs is 47-LOC body** (new code, over 40). Recommend extracting the per-kind match arms (`use_wildcard` stripping + `self` handling) to helpers.

## 6. Must-fix count: **2**
**Biggest**: *B2 non-deterministic truncation*. In `collect_cluster_memberships`, sort `entries` by `(qualified_name, community_id)` before applying the 10k cap. Without this, any corpus exceeding the cap produces different Q12 ARI scores per run.

## 7. What the engineer missed
- **Dead B3 fallback**: `flagged_present` is emitted by no MCP tool in the codebase (grepped `src/*.rs`). The fallback branch in `extract_axis_set` is unreachable dead code. Either remove it or cite the contract that requires it.
- **Determinism guarantee on the cap** (§6) — not covered by the integration test because the fixture is <10k.
- **`cluster_id_from_community_id` silently maps parse-fails to `-1`**: if persister ever changes ID shape, scorer silently returns all-`-1` labels and ARI collapses to 0 with no error signal. Consider logging a warning when `-1` is produced.
- Test coverage for aliased glob (`use a::* as x;`) — not valid Rust, so fine, but a negative-path test would harden intent.
