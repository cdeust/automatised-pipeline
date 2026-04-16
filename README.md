<p align="center">
  <strong>automatised-pipeline</strong><br>
  <em>Codebase intelligence as an MCP server — Rust, LadybugDB graph, tree-sitter, hybrid search.</em>
</p>

<p align="center">
  <a href="LICENSE"><img src="https://img.shields.io/badge/License-MIT-blue.svg" alt="MIT License"></a>
  <img src="https://img.shields.io/badge/Rust-1.94+-dea584.svg" alt="Rust 1.94+">
  <img src="https://img.shields.io/badge/Tools-23-orange" alt="23 MCP tools">
  <img src="https://img.shields.io/badge/Tests-220_passing-brightgreen" alt="220 tests">
  <img src="https://img.shields.io/badge/Languages-Rust_·_Python_·_TypeScript-blueviolet" alt="Languages">
  <img src="https://img.shields.io/badge/Stages-0_through_9-8A2BE2" alt="Stages">
</p>

<p align="center">
  <a href="#what-an-agent-can-ask-it">What An Agent Can Ask</a> · <a href="#getting-started">Getting Started</a> · <a href="#the-pipeline">Pipeline</a> · <a href="#23-mcp-tools">Tools</a> · <a href="#architecture">Architecture</a> · <a href="#the-zetetic-standard">Zetetic Standard</a>
</p>

<p align="center">
  <strong>Companion projects:</strong><br>
  <a href="https://github.com/cdeust/Cortex">Cortex</a> — persistent memory that consolidates and reconsolidates across sessions<br>
  <a href="https://github.com/cdeust/zetetic-team-subagents">zetetic-team-subagents</a> — 97 genius reasoning agents + 18 team specialists<br>
  <a href="https://github.com/cdeust/prd-spec-generator">prd-spec-generator</a> — TypeScript PRD generator that consumes our graph intelligence
</p>

---

Every AI coding assistant hits the same wall: you ask it to change `handle_tool_call`, and it either hallucinates a function that was renamed last week, edits something in the wrong community of the codebase, or silently breaks a call chain three modules away. Agents operate on strings; codebases have structure. The gap is where bugs live.

**automatised-pipeline** is a Rust MCP server that indexes any Rust / Python / TypeScript codebase into a LadybugDB property graph, resolves imports and call chains across files, detects functional communities via Leiden-class community detection, traces execution flows from entry points, builds a hybrid BM25 + sparse TF-IDF + RRF search index, and exposes all of it to AI agents through 23 MCP tools.

It is the **codebase intelligence layer** that sits between a finding ("this bug exists") and a PRD ("here is the fix, here is what it affects, here is what it must never break"). It is **read-only intelligence** — it never writes code, opens PRs, or runs CI. It tells the system what is true about the code so the next stage can reason without guessing.

**One pipeline stage = one MCP tool. 10 stages. 23 tools. 12,000+ lines of Rust. 220 tests. Zero warnings. Every constant sourced.**

---

## What an agent can ask it

```
analyze_codebase(path: "/path/to/project", output_dir: "/tmp/run")
  → index + resolve + cluster + build search index in one call
  → 430 nodes, 400 edges, 216 communities, 35 processes on our own codebase

search_codebase(graph_path, query: "process incoming tool requests")
  → hybrid ranked results: BM25 lexical + sparse TF-IDF semantic + RRF fusion
  → returns: handle_tool_call (score 0.021), dispatch_request (0.020), ...

get_context(graph_path, qualified_name: "src/main.rs::handle_tool_call")
  → 360° view: community membership, process participation,
    incoming calls, outgoing calls, types used, types that use it
  → did-you-mean suggestions when the symbol isn't found exactly

get_impact(graph_path, qualified_name)
  → blast radius: every process that transits this symbol, every community it touches
  → the answer to "what breaks if I change this?"

detect_changes(graph_path, diff_text OR base_ref+head_ref)
  → git diff → affected symbols → impacted communities → touched processes
  → risk score for the change

validate_prd_against_graph(prd_path, graph_path)
  → does the PRD reference real symbols? (symbol hallucination check)
  → does "scoped to X" match the actual community count?
  → does "doesn't affect main" hold against the call graph?

check_security_gates(graph_path, changed_symbols)
  → auth-critical community touch · unsafe symbol · public API change ·
    unresolved imports · test coverage gap

verify_semantic_diff(before_graph_path, after_graph_path)
  → what nodes/edges appeared, what disappeared, what dangles,
    new cycles via Tarjan SCC, regression score with verdict
```

---

## Getting started

### Prerequisites

- Rust 1.94+ (`rustup install stable`)
- CMake (LadybugDB builds its C++ core from source — ~5 minutes first build, cached after)

### Clone + build

```bash
git clone https://github.com/cdeust/automatised-pipeline.git
cd automatised-pipeline
cargo build --release
# First build: ~5 minutes (compiles LadybugDB C++ core)
# Subsequent builds: <1 second incremental
```

### Register the MCP server

The repo ships a `.mcp.json` that Claude Code picks up automatically when you open the directory:

```json
{
  "mcpServers": {
    "ai-architect": {
      "command": "cargo",
      "args": ["run", "--quiet", "--release", "--manifest-path", "Cargo.toml"]
    }
  }
}
```

Or register globally:

```bash
claude mcp add ai-architect -- /absolute/path/to/target/release/ai-architect-mcp
```

### First run

```bash
# Run the binary directly to verify the handshake
./target/release/ai-architect-mcp

# Or exercise it via stdio JSON-RPC:
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' \
  '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"health_check","arguments":{}}}' \
  | ./target/release/ai-architect-mcp
```

---

## The pipeline

Every stage is a tool. Stages build on each other but are independently callable. The pipeline is serial in logical order but MCP calls are stateless — you can re-run stages 3a-3d on a fresh codebase without re-running stages 1-2.

| # | Tool(s) | What it does |
|---|---|---|
| **0** | `health_check` | Handshake + protocol + tool count |
| **1** | `extract_finding`, `refine_finding` | Deterministic finding extraction + orchestrator-aware prompt refinement |
| **2** | `start_verification`, `append_clarification`, `finalize_verification`, `abort_verification` | Human-gated clarification loop with SHA-256 transcript digest, atomic single-file session state |
| **3a** | `index_codebase`, `query_graph`, `get_symbol` | tree-sitter AST → LadybugDB graph (16 node labels, 36+ relationship tables) |
| **3b** | `resolve_graph`, `lsp_resolve` | Import/call/impl resolution with confidence scoring + optional LSP deep resolution (rust-analyzer / pyright / typescript-language-server) |
| **3c** | `cluster_graph`, `get_processes`, `get_impact` | Leiden-class community detection (Louvain + C2 repair) + BFS execution-flow tracing from entry points |
| **3d** | `search_codebase`, `get_context`, `analyze_codebase`, `detect_changes` | Hybrid BM25 + sparse TF-IDF + RRF search · 360° symbol view · all-in-one analysis · git-diff impact |
| **4** | `prepare_prd_input` | Bundle verified finding + graph intel → artifact for prd-spec-generator |
| **6** | `validate_prd_against_graph` | Symbol hallucination · community consistency · process-impact contradiction |
| **8** | `check_security_gates` | Auth-critical community · unsafe symbol · public-API change · unresolved-import intro · test-coverage gap |
| **9** | `verify_semantic_diff` | Before/after graph diff with Tarjan SCC cycle detection and regression scoring |

> Stages 5 (PRD generation), 7 (implementation), 10 (benchmark), 11 (deployment), 12 (PR) belong to other systems in the pipeline: [prd-spec-generator](https://github.com/cdeust/prd-spec-generator), the coding agent, CI, and `gh`. This project is the **read-only intelligence** half.

---

## 23 MCP Tools

Every tool takes structured JSON arguments via the MCP protocol and returns a structured JSON response. No LLM is called from inside any tool — intelligence is the agent's job; the tool's job is safe, fast data movement with invariants.

```
Stage 0:  health_check
Stage 1:  extract_finding · refine_finding
Stage 2:  start_verification · append_clarification · finalize_verification · abort_verification
Stage 3a: index_codebase · query_graph · get_symbol
Stage 3b: resolve_graph · lsp_resolve
Stage 3c: cluster_graph · get_processes · get_impact
Stage 3d: search_codebase · get_context · analyze_codebase · detect_changes
Stage 4:  prepare_prd_input
Stage 6:  validate_prd_against_graph
Stage 8:  check_security_gates
Stage 9:  verify_semantic_diff
```

Each tool has a JSON Schema enforced at the wire, reason codes on error (no cryptic protocol errors), and a receipt-style response with timing and counts.

---

## Architecture

Rust MCP server, hand-rolled stdio JSON-RPC 2.0 (no SDK — we own the wire). Clean Architecture with module boundaries.

```
transport (stdio, JSON-RPC framing)
      ↓
server/main.rs  (request dispatch, tool registry)
      ↓
handlers (do_* functions, one per tool)
      ↓
core modules:
    graph_store        — LadybugDB port (Cypher + UNWIND + prepared statements)
    parser/{rust,python,typescript,mod}  — tree-sitter AST extractors
    indexer            — walk + parse + persist pipeline
    resolver           — cross-file import/call/impl resolution
    lsp_{client,resolver}  — optional LSP deep resolution
    clustering         — inline Louvain + C2 repair + process tracing
    search/{bm25,vector,rrf,mod}  — hybrid search (Tantivy + sparse TF-IDF + RRF)
    prd_input          — stage 4: bundle for prd-spec-generator
    prd_validator      — stage 6: validate PRD claims against graph
    security_gates     — stage 8: auth/unsafe/API/imports/coverage checks
    semantic_diff      — stage 9: before/after graph regression scoring
    git_diff           — diff parser + symbol mapping
```

### Crates

Eight crates. Nothing speculative; everything justified.

| Crate | Purpose | License | Why |
|---|---|---|---|
| `serde` + `serde_json` | Wire serialization | MIT | JSON-RPC, artifact persistence |
| `sha2` | Stage-2 transcript digest | MIT | Tamper detection |
| `lbug` (LadybugDB) | Embedded property graph + Cypher | MIT | Native Cypher, FTS-ready, the Kùzu successor |
| `tree-sitter` | Incremental parser runtime | MIT | First-class Rust bindings |
| `tree-sitter-rust` · `-python` · `-typescript` | Language grammars | MIT | Semantic structure without a compiler |
| `tantivy` | Lucene-grade BM25 | MIT | Real ranked text search, <10ms startup |

Deliberately **not** included: async runtime (we're stdio-blocking), HTTP client, LLM SDK, embedding model runtime (sparse TF-IDF replaces it at zero dep cost).

### Storage

Graphs are per-finding by design (Lamport's isolation invariant): each finding gets its own LadybugDB instance at `<output_dir>/runs/<run_id>/findings/<finding_id>/graph/`. Zero-coordination concurrency, trivial cleanup, no cross-finding state leakage. Redundant indexing for shared codebases is acknowledged and mitigated in a later optional cache layer — not shoehorned into the core.

---

## The zetetic standard

Inherited from [zetetic-team-subagents](https://github.com/cdeust/zetetic-team-subagents). Not a prompt suggestion — an enforcement rule that holds in code.

| Pillar | Question |
|---|---|
| **Logical** | *Is it consistent?* |
| **Critical** | *Is it true?* |
| **Rational** | *Is it useful?* |
| **Essential** | *Is it necessary?* |

**In this codebase it concretely means:**

1. Every algorithm traces to a source. Louvain → *Blondel et al. 2008*. Leiden C2 repair → *Traag et al. 2019*. RRF → *Cormack, Clarke, Büttcher 2009*. SCC → *Tarjan 1972*. BM25 via Tantivy → *Robertson et al. 1994*.
2. Every named constant has a `// source:` comment. `RRF_K = 60` cites Cormack 2009. `BULK_BATCH_SIZE = 500` cites Kùzu/LadybugDB tuning. `PARSE_TIMEOUT_MICROS = 5_000_000` is justified in the block above it.
3. No invented numbers. Where a value was chosen by judgment, the comment says so ("heuristic, not paper-backed") and cites its operational justification.
4. Tool responses cite the spec that governs each error reason. `unsafe finding_id (spec §5.1.4, §9.3 Q4): must match [A-Za-z0-9._-]+` — callers see which rule they violated.
5. When a capability can't be proved at spec time, the tool degrades gracefully and says so in plain language. Example: `lsp_resolve` on a stub binary returns `lsp_probe_failed: found on PATH but didn't respond as an LSP server (stdout closed immediately; likely a stub, proxy, or non-LSP binary)` — not a cryptic protocol error.

---

## Security

Four CRITICAL, four HIGH, three MEDIUM findings were surfaced by a `security-auditor` agent pass and fixed in commit [`512d683`](https://github.com/cdeust/automatised-pipeline/commit/512d683):

- Cypher injection via `insert_edge` → centralized `cypher_str()` escaping (`\` first, then `'`)
- Git argument injection → `validate_git_ref` rejects `--`, newlines, NUL; `--` separator before refs
- Arbitrary binary execution via `lsp_command` → strict allowlist (`rust-analyzer`, `pyright`, `pyright-langserver`, `typescript-language-server`)
- Symlink traversal → `fs::symlink_metadata` + `MAX_DEPTH`
- Resource exhaustion → `MAX_FILES=100_000`, `MAX_FILE_BYTES=10 MB`, `MAX_TOTAL_BYTES=2 GB`, `MAX_DEPTH=64`
- Tree-sitter pathological input → `set_timeout_micros(5_000_000)` + `MAX_PARSE_BYTES=1 MB`
- `query_graph` read-only → forbidden-keyword whole-word filter (CREATE/DELETE/MERGE/SET/REMOVE/DROP/ALTER/CALL/LOAD)
- `graph_path` filesystem safety → `validate_graph_path_safe()` before any `remove_dir_all`
- LSP `rootUri` → RFC 3986 percent-encoding
- Diff line overflow → `DIFF_LINE_MAX = u64::MAX / 2` guard

Each fix has a test that asserts the exploit is now rejected. Run `cargo test` to see 220 tests pass including the exploit-regression suite.

---

## Scale

Verified by the `dba` agent through compile-and-run probes against lbug 0.15.3:

| Strategy | ms/edge |
|---|---|
| Raw string per edge (naive) | 5.36 |
| Prepared statement, no transaction | 5.48 |
| `BEGIN TRANSACTION` + prepared + `COMMIT` | 0.70 |
| **UNWIND + typed `LogicalType::Struct`** | **0.143** |

The bulk-insert path uses UNWIND with a typed struct schema (the engineer who wrote the first version used `LogicalType::Any` which fails the binder — the typed struct form works). Prepared statements are cached in a `RefCell<HashMap<query, PreparedStatement>>` on the `GraphStore`. Sparse TF-IDF replaces the dense `N × V × 4B` matrix — **30.5× smaller** on our own codebase (108 KB vs 3.2 MB) and scales linearly with non-zero terms rather than vocab size. Clustering eliminated `probe_node_label_for_process` (per-node Cypher round-trip) in favor of a single in-memory `HashMap<id, label>` population pass.

500-file synthetic Rust fixture indexes in **~38 seconds** end-to-end (parse + resolve + cluster + search index), down from the pre-audit implied "5 min – 1 hour" bracket.

---

## Integration with the rest of the stack

```
                 ┌─────────────────────────────────────────┐
                 │           Claude Code agent             │
                 └────────────┬────────────────────────────┘
                              │ MCP (stdio JSON-RPC)
                              ↓
      ┌──────────────────────────────────────────────────┐
      │             automatised-pipeline                 │  ← this repo
      │  stage 0 · 1 · 2 · 3a-d · 4 · 6 · 8 · 9          │
      │  Rust · LadybugDB · tree-sitter · Tantivy        │
      └──────┬──────────────────┬────────────────────────┘
             │                  │
             │                  └────→  stage 5 (PRD gen)
             │                          [prd-spec-generator]
             ↓                          TypeScript / Node
     ┌─────────────────┐                    │
     │     Cortex      │                    │
     │  memory engine  │ ←──────────────────┘
     │  PostgreSQL +   │
     │    pgvector     │
     └─────────────────┘
             ↑
             │  cross-session memory for findings,
             │  decisions, lessons learned
             │
     ┌─────────────────────────────┐
     │  zetetic-team-subagents     │
     │  97 genius + 18 specialists │
     │  problem-shape routing      │
     └─────────────────────────────┘
```

- **Cortex** — every architectural decision made during a pipeline run gets remembered. When the next finding touches a similar area, Cortex surfaces the prior reasoning before you re-derive it.
- **zetetic-team-subagents** — the genius agents (Shannon, Lamport, Simon, Popper, Feynman, Fermi, dba, architect, security-auditor, engineer) designed this project stage by stage. Every major decision in `stages/*.md` traces to an agent dispatch.
- **prd-spec-generator** — consumes our `stage-4.prd_input.json` artifact via disk or MCP-to-MCP query of `search_codebase` / `get_context` / `get_impact`. Each in its ideal language: our performance-critical graph work in Rust, their document generation in TypeScript.

---

## Testing

```bash
cargo test                                          # 220 tests, full suite
cargo test --release --test scalability_bench       # 500-file synthetic fixture
cargo test --release --test lbug_bulk_investigation # dba's 9 UNWIND probes
cargo test --release --test stage3a_integration     # end-to-end per sub-stage
cargo test --release --test stage9_integration      # before/after diff
cargo check                                         # zero warnings required
cargo build --release                               # release binary
```

Every stage has an integration test with fixture data. The `lbug_bulk_investigation` test is intentionally preserved — it's the compile-and-run proof that dba's UNWIND pattern works, kept for regression protection and documentation.

---

## Repository layout

```
automatised-pipeline/
├── src/
│   ├── main.rs                    ← MCP server, 23 tool handlers
│   ├── tool_schemas.rs            ← JSON Schemas for every tool
│   ├── lib.rs                     ← re-exports for integration tests
│   ├── graph_store.rs             ← LadybugDB port (UNWIND + prepared + cached)
│   ├── parser/
│   │   ├── mod.rs                 ← language dispatch
│   │   ├── rust.rs · python.rs · typescript.rs
│   ├── indexer.rs                 ← walk + parse + persist
│   ├── resolver.rs                ← cross-file resolution
│   ├── lsp_client.rs              ← minimal LSP probe + client
│   ├── lsp_resolver.rs            ← LSP-backed deep resolution
│   ├── clustering.rs              ← Louvain + C2 repair + BFS process tracing
│   ├── search/
│   │   ├── mod.rs                 ← orchestration, get_context, 3-layer qn lookup
│   │   ├── bm25.rs · vector.rs · rrf.rs
│   ├── prd_input.rs               ← stage 4
│   ├── prd_validator.rs           ← stage 6
│   ├── security_gates.rs          ← stage 8
│   ├── semantic_diff.rs           ← stage 9
│   └── git_diff.rs                ← diff parsing + symbol mapping
├── stages/                        ← locked spec per stage (Shannon, then engineer implements)
│   ├── stage-1.md · stage-2.md · stage-3.md · stage-3b.md · stage-3c.md
│   ├── stage-6.md · stage-8.md
│   ├── stage-1.review.md · stage-3-db-evaluation.md · stage-3-research.md
│   └── decisions/                 ← Popper / Lamport / Simon verdicts per decision
├── tests/
│   ├── stage{3a,3b,3c,3d,4,6,8,9}_integration.rs
│   ├── multilang_integration.rs
│   ├── stage3d_hybrid_search.rs
│   ├── scalability_bench.rs
│   ├── lbug_bulk_investigation.rs
│   ├── tfidf_size_report.rs
│   └── fixtures/multilang/        ← sample.rs · sample.py · sample.ts
├── .claude/
│   ├── agents/                    ← 18 specialists + 97 genius agents
│   ├── skills/ · commands/ · tools/ · hooks/
│   └── scripts/
├── .mcp.json
├── NOTES.md                       ← stages table + growth rule
├── Cargo.toml
└── README.md
```

---

## The zetetic decisions behind the build

Every major architectural decision was made by a genius agent with a specific problem shape. Stored in `stages/decisions/*.md` and in Cortex.

| Decision | Agent | Verdict |
|---|---|---|
| Rust vs C/C++ for the glue layer | **Popper** | Conjecture "Rust is the right language" is unfalsified. `lbug` + `tree-sitter` already run native C/C++; Rust is the glue where the borrow checker pays the most. |
| Graph-per-finding vs graph-per-codebase | **Lamport** | Per-finding. Isolation holds by construction with zero coordination; the redundant-indexing cost is mitigable in an optional cache layer later. |
| Stage 3a decomposition | **Simon** | Five steps, satisficed against the growth rule; first useful query at step 4. |
| DB backend choice | **dba** | LadybugDB (`lbug 0.15.3`) — only option simultaneously maintained, native Cypher, embedded, with FTS + vector + algo extensions. |
| Stage 2 clarification loop shape | **Shannon** | Four-tool state machine with atomic single-file session (no crash window between separate files), unconditional one-round-minimum before finalize. |
| lbug UNWIND pattern | **dba** | `LogicalType::Struct { fields }` works; `LogicalType::Any` fails the binder — 38× speedup verified by compile-and-run probes. |

Agents are spawned via [zetetic-team-subagents](https://github.com/cdeust/zetetic-team-subagents); each genius is a reasoning pattern (not a persona) with canonical moves and primary-source citations.

---

## Status

Private repo by design. Not ready for public release until the full hardening pass is done — security audit fixes are in, correctness fixes are in, scale fixes are in, stages 4/6/8/9 are live, but every capability marked "live" above has been verified end-to-end on this machine, not yet in a production context.

**What works today**: indexing Rust / Python / TypeScript codebases end-to-end, resolving cross-file relationships, clustering into communities, tracing processes from entry points, hybrid search, PRD input preparation, PRD claim validation, security gate checking, before/after regression detection.

**What's deferred**:
- Cross-file indexer batching to unlock the full 38× UNWIND win (currently 1.17× aggregate; per-edge rate is already 0.143 ms)
- `is_unsafe` extraction in the Rust parser (stage 8 S2 runs in `info`-skip mode pending this)
- LSP-based deep method resolution on inferred types
- Multi-repo / workgroup operations (GitNexus `group_*`)
- Rename / refactor tools (we are read-only by design)

---

## License

MIT — see [LICENSE](LICENSE).

---

<p align="center"><sub>Built by <a href="https://github.com/cdeust">cdeust</a>. Every stage designed by a genius agent. Every constant sourced.</sub></p>
