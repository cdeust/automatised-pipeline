# Stage 3a Implementation Plan — Structure + Parse

**Method:** Simon (bounded rationality, satisficing). **Date:** 2026-04-11.

---

## Problem definition

- **Decision:** what is the right decomposition of sub-stage 3a into concrete implementation steps, ordered for one engineer agent at a time?
- **Space:** all possible orderings of the work items needed to make `index_codebase`, `query_graph`, and `get_symbol` functional on a real Rust codebase.
- **Constraints:** one engineer agent at a time; one language (Rust) first; `lbug` 0.15.x is the graph DB (verified, MIT); `tree-sitter` + `tree-sitter-rust` for parsing; `src/main.rs` is 2002 LOC (growth rule: extract to modules when a stage can't live in one arm); no function body > 40 LOC; every constant sourced; `cargo check` zero warnings.

## Satisficing criteria

| Criterion | Threshold | Rationale |
|---|---|---|
| **Functional milestone** | After the final step, `index_codebase` on our own MCP server produces a graph and `query_graph` with `MATCH (f:Function) RETURN f.name LIMIT 10` returns real function names | This is the stated acceptance test |
| **Step independence** | Each step has a smoke test that passes before the next begins | Growth rule: one stage at a time fully implemented |
| **LOC per step** | No step adds more than ~400 LOC | Keeps diffs reviewable; one engineer dispatch per step |
| **New crates** | Total new crates for all of 3a <= 5 | Fewer deps = less risk, faster builds |
| **Compilation** | `cargo check` zero warnings after every step | Hard constraint from project rules |
| **Function size** | No function body > 40 LOC | Discipline from stage 1 tech-debt cleanup |

## Means-ends analysis

| Current state | Goal state | Biggest difference | Operator |
|---|---|---|---|
| No graph DB in the project | Graph DB initialized, schema created | No `lbug` dependency | Step 1: add `lbug`, create schema |
| No parser | AST extracted from `.rs` files | No `tree-sitter` dependency | Step 2: add tree-sitter, parse one file |
| Parsed AST in memory, no graph | Nodes and edges persisted in lbug | No write path from AST to graph | Step 3: walk + parse + insert pipeline |
| Graph exists on disk but no MCP tool | Agent can call `index_codebase` | No tool registration | Step 4: wire MCP tools |
| Tools exist but untested on real code | `MATCH (f:Function) RETURN f.name LIMIT 10` returns real names from our own codebase | No end-to-end validation | Step 5: smoke test on self |

---

## Implementation steps

### Step 1 — Storage foundation

**What it adds:**
- `Cargo.toml`: add `lbug` crate (version locked to 0.15.x)
- `src/graph_store.rs`: new module implementing the `GraphStore` port from stage-3.md section 7, backed by lbug. Functions: `open_or_create`, `create_schema`, `insert_node`, `insert_edge`, `bulk_insert_nodes`, `bulk_insert_edges`, `execute_query`, `get_node_by_qualified_name`, `flush`, `close`.
- Schema creation via Cypher: `CREATE NODE TABLE` for each NodeLabel used in 3a (Directory, File, Module, Function, Method, Struct, Enum, Variant, Trait, Field, Constant, TypeAlias, Import, CallSite). `CREATE REL TABLE` for each EdgeKind used in 3a (Contains, Defines, HasMethod, HasField, HasVariant).
- `src/main.rs`: add `mod graph_store;` declaration.

**Smoke test:** Unit test in `graph_store.rs` — open a temp DB, create schema, insert one Function node, query it back with `MATCH (f:Function) WHERE f.name = 'test_fn' RETURN f.name`, assert the name matches.

**Satisficing question:** Can we open a lbug database, create the schema for all 3a node labels and edge kinds, and round-trip a node through Cypher? If yes, step 2 is unblocked.

**Estimated LOC:** ~250 (schema DDL strings + wrapper functions + test)

**Risk:** `lbug` may not compile on the current toolchain. Fallback: `kuzu 0.11.3` (frozen but functional; API is nearly identical since lbug is the direct successor).

---

### Step 2 — Rust parser adapter

**What it adds:**
- `Cargo.toml`: add `tree-sitter` (MIT) + `tree-sitter-rust` (MIT)
- `src/rust_parser.rs`: new module implementing the `CodeParser` port from stage-3.md section 6 for Rust only. Functions: `parse_rust_file` (entry point), plus private helpers for each node type extraction: `extract_functions`, `extract_structs`, `extract_enums`, `extract_traits`, `extract_impls`, `extract_use_statements`, `extract_constants`, `extract_type_aliases`.
- Output type: `ParseResult` struct with `Vec<ExtractedNode>` and `Vec<ExtractedRef>`.
- Tree-sitter S-expression queries for each Rust construct. Source: tree-sitter-rust grammar (https://github.com/tree-sitter/tree-sitter-rust/blob/master/src/node-types.json).

**Smoke test:** Parse our own `src/main.rs` (2002 lines). Assert: (a) zero parse errors, (b) extracted nodes include known functions like `main`, `handle_tool_call`, `write_message`, (c) every extracted node has valid span (start_line < end_line or start_col < end_col for single-line items).

**Satisficing question:** Can we parse a `.rs` file and extract a list of typed symbols with names, qualified names, spans, and properties? If yes, step 3 is unblocked.

**Estimated LOC:** ~350 (tree-sitter query construction + node extraction + ParseResult types + test)

---

### Step 3 — Walk + parse + persist pipeline

**What it adds:**
- `src/indexer.rs`: new module. Functions: `index_codebase` (entry point), `walk_directory` (recursive file discovery, respects `.gitignore` via `ignore` crate or manual exclusion of `target/`, `.git/`, etc.), `index_file` (parse + insert nodes + insert edges for one file).
- Wires step 1 (graph_store) and step 2 (rust_parser) together: for each `.rs` file found, parse it, then bulk-insert nodes and edges into the lbug graph.
- Directory and File nodes created during the walk.
- Containment edges: Directory -[Contains]-> File, Directory -[Contains]-> Directory.
- Definition edges: File -[Defines]-> top-level symbols; Struct -[HasField]-> Field; Enum -[HasVariant]-> Variant; Struct/Trait -[HasMethod]-> Method.
- `Cargo.toml`: possibly add `ignore` crate (MIT) for .gitignore-aware walking, or use `walkdir` (MIT) with manual exclusion. Decision: use `walkdir` + manual exclusion list to minimize deps.

**Smoke test:** Index our own `src/` directory. Open the resulting lbug database. Run `MATCH (f:File) RETURN count(f)` — expect >= 1 (at minimum main.rs + the new modules). Run `MATCH (fn:Function) RETURN count(fn)` — expect > 30 (our main.rs has dozens of functions).

**Satisficing question:** Does the pipeline walk a real directory, parse all `.rs` files, and persist a graph where nodes and edges are queryable via Cypher? If yes, step 4 is unblocked.

**Estimated LOC:** ~200 (walk logic + file-to-graph insertion + edge creation + test)

**Note:** `walkdir` may already be needed; if not, `std::fs::read_dir` with a recursive helper suffices for MVP and avoids a new dep. Satisfice: use `std::fs` if the walk logic fits in < 40 LOC per function, else add `walkdir`.

---

### Step 4 — MCP tool wiring

**What it adds:**
- `src/main.rs`: three new entries in `tools_list()` — `index_codebase`, `query_graph`, `get_symbol`.
- `src/main.rs`: three new match arms in `handle_tool_call()`, each delegating to `indexer.rs` and `graph_store.rs`.
- Tool schemas (JSON Schema for each tool's input, per stage-3.md section 3a):
  - `index_codebase`: `{ path: string, language: "rust", output_dir: string }` -> `{ graph_path, node_count, edge_count, elapsed_ms }`
  - `query_graph`: `{ graph_path: string, query: string }` -> `{ columns, rows, elapsed_ms }`
  - `get_symbol`: `{ graph_path: string, qualified_name: string }` -> `{ node, edges_out, edges_in }`
- On-disk layout decision: the lbug database lives at `<output_dir>/graph/` (a directory containing lbug's internal files). This parallels stages 1-2 putting artifacts under `<output_dir>/runs/<run_id>/`.

**Smoke test:** Build and run the MCP server. Send a JSON-RPC `tools/call` for `index_codebase` with `path` pointing at our own `src/`. Then send `query_graph` with `MATCH (f:Function) RETURN f.name LIMIT 10`. Assert the response contains real function names.

**Satisficing question:** Can an MCP client call `index_codebase` and then `query_graph` and get a useful answer? If yes, the "first useful query" milestone is reached.

**Estimated LOC:** ~150 (tool definitions + match arms + parameter validation + response formatting)

---

### Step 5 — Module extraction + cleanup

**What it adds:**
- `src/main.rs` is currently 2002 LOC. Steps 1-4 add ~150 LOC directly to main.rs (step 4 match arms) while the bulk goes to new modules. But the `tools_list()` function and `handle_tool_call()` function will grow. If either exceeds 40 LOC, extract stage-3 tool definitions into a `src/stage3_tools.rs` module.
- Review all new code for: zero warnings, no function > 40 LOC, all constants sourced, all error paths return proper JSON-RPC errors (not panics).
- Add integration test: `tests/stage3a_smoke.rs` — end-to-end test that indexes a small fixture Rust project (3-5 files with known structure) and runs the three verification queries from Shannon's spec.

**Smoke test:** `cargo test` passes. `cargo clippy` zero warnings. The integration test proves the three tools work end-to-end on a fixture project.

**Satisficing question:** Is the code clean enough that the next engineer (working on 3b — resolution) can add `resolve_graph` without first cleaning up 3a? If yes, 3a is complete.

**Estimated LOC:** ~100 (test fixture + integration test + any extraction refactoring)

---

## Summary

| Step | Name | New files | New crates | LOC est. | Cumulative LOC |
|---|---|---|---|---|---|
| 1 | Storage foundation | `src/graph_store.rs` | `lbug` | ~250 | ~250 |
| 2 | Rust parser adapter | `src/rust_parser.rs` | `tree-sitter`, `tree-sitter-rust` | ~350 | ~600 |
| 3 | Walk + parse + persist | `src/indexer.rs` | `walkdir` (conditional) | ~200 | ~800 |
| 4 | MCP tool wiring | modifications to `src/main.rs` | none | ~150 | ~950 |
| 5 | Module extraction + cleanup | `tests/stage3a_smoke.rs`, possibly `src/stage3_tools.rs` | none | ~100 | ~1050 |

**Total estimated LOC added:** ~1050

**First useful query milestone:** Step 4. After step 4 completes, an agent can call `index_codebase` on our own MCP server and then `query_graph` with `MATCH (f:Function) RETURN f.name LIMIT 10` and get real function names back.

**Total new crates for 3a:**
1. `lbug` — graph database (MIT). Fallback: `kuzu 0.11.3`.
2. `tree-sitter` — parser runtime (MIT).
3. `tree-sitter-rust` — Rust grammar (MIT).
4. `walkdir` — directory walking (MIT/Apache-2.0). Conditional: only if `std::fs` recursive walk exceeds 40 LOC per function.

Maximum 4 crates. Minimum 3 (if walkdir is unnecessary).

---

## Near-decomposability assessment

| Proposed boundary | Intra coupling | Inter coupling | Verdict |
|---|---|---|---|
| `graph_store.rs` (storage) | High: all lbug interactions, schema DDL, Cypher execution | Low: exposes `GraphStore` trait; consumers pass nodes/edges in, get query results out | Decomposable — clean port boundary |
| `rust_parser.rs` (parsing) | High: all tree-sitter interactions, AST traversal, node extraction | Low: exposes `parse_rust_file` returning `ParseResult`; no knowledge of storage | Decomposable — clean port boundary |
| `indexer.rs` (orchestration) | Medium: wires parser + storage + walk | Medium: depends on both parser output types and storage input types | Acceptable — this is the integration module by design; its coupling to both sides is expected and necessary |

The three-module decomposition is near-decomposable. The intra-module interactions (lbug API calls within graph_store, tree-sitter API calls within rust_parser) dominate the inter-module interactions (indexer calling parse + insert). This satisfies Simon's criterion.

---

## Candidates considered

| Order variant | Description | Satisfices? | Why / why not |
|---|---|---|---|
| **A (selected)** | Storage -> Parser -> Pipeline -> Tools -> Cleanup | Yes | Each step has a self-contained smoke test; natural dependency order; no circular dependencies between steps |
| B | Parser -> Storage -> Pipeline -> Tools | Yes but worse | Parser without storage means extracted nodes go nowhere; smoke test is weaker (can only test in-memory, not round-trip through Cypher) |
| C | Tools first (stubs) -> Storage -> Parser -> Pipeline | No | Stub tools that return fake data violate "no pre-scaffolding" rule from NOTES.md |
| D | All-at-once (one giant step) | No | Violates step independence criterion; no intermediate smoke tests; too large for one engineer dispatch |

Selected **A**: first to satisfice, natural dependency order, each step independently testable.

---

## Aspiration level check

The aspiration level ("after 3a, an agent can index our own MCP server and query it with Cypher") is achievable in 5 steps / ~1050 LOC / 3-4 new crates. This is not optimistic — it is the minimum path through the dependency graph. No aspiration adjustment needed.

---

## Hand-offs

- **Implementation (steps 1-5):** engineer agent, one dispatch per step, sequential.
- **lbug compilation verification:** dba agent (if lbug fails to compile, dba switches to kuzu 0.11.3 fallback and documents the delta).
- **Tree-sitter query correctness:** test-engineer agent (the smoke tests in steps 2-3 need known-answer fixtures).
- **Architecture review after step 5:** architect agent (verify the three-module decomposition holds, review main.rs growth).
- **Next sub-stage (3b — resolution):** blocked until all 5 steps of 3a pass their smoke tests.
