# ai-architect-mcp

Stage-by-stage MCP rewrite of the ai-architect pipeline. One pipeline stage = one MCP tool. Grown by the zetetic + genius agents in `.claude/agents/`.

## Language

Rust. Hand-rolled stdio JSON-RPC 2.0 (no MCP SDK). Two deps: `serde`, `serde_json`.

## Rules

1. One pipeline stage = one MCP tool. No exceptions.
2. No pre-scaffolding. Layers, modules, helpers, and abstractions appear only when a stage actually needs them. When a stage grows past a single arm in `handle_tool_call`, *then* extract it.
3. Every algorithm, constant, and query must trace to a source (paper, spec, measured data). Zetetic standard applies.
4. Reference implementation for what the pipeline should do: `/Users/cdeust/Developments/ai-architect/` (macOS app). Read it, don't copy it.

## Stages

| # | Tool | Status | Purpose |
|---|---|---|---|
| 0 | `health_check` | **live** | Handshake + server state. Confirms the MCP is alive before any stage call. |
| 1 | `extract_finding`, `refine_finding` | **live** | Deterministic extraction + orchestrator-aware persistence. Finding → extracted artifact → refined artifact. |
| 2 | `start_verification`, `append_clarification`, `finalize_verification`, `abort_verification` | **live** | Clarification session state machine. Open → turns → finalized/aborted. |
| 3a | `index_codebase`, `query_graph`, `get_symbol` | **live** | Structure + Parse: tree-sitter AST → LadybugDB graph. Rust-only. |
| 3b | `resolve_graph`, `lsp_resolve` | **live** | Resolution: cross-file import/call/trait resolution. |
| 3c | `cluster_graph`, `get_processes`, `get_impact` | **live** | Clustering (Leiden) + Process tracing + blast radius. |
| 3d | `search_codebase`, `get_context`, `analyze_codebase`, `detect_changes` | **live** | Hybrid search (BM25+vector+RRF), 360° context, diff→impact. |
| 4  | `prepare_prd_input` | **live** | Bundle finding + graph intel (matched symbols, impacted communities, impacted processes, graph stats) into `stage-4.prd_input.json`. Read-only against the graph. |
| 5  | *(generate-prd skill)* | **owned by prd-spec-generator** | PRD generation with its 9-step workflow, multi-judge verification, 9-file export. Out of scope for this MCP. |
| 6 | `validate_prd_against_graph` | **live** | Symbol hallucination + community consistency + process impact validation against PRD claims |
| 7  | *(implementation)* | **owned by coding agent** | Claude Code / Cursor / CLI agent consumes PRD + impact bundle and edits the target repo. Optional `prepare_implementation_handoff` tool on our side. |
| 8 | `check_security_gates` | **live** | Auth-critical + unsafe + public-API + unresolved-import + test-coverage-gap gates |
| 9  | `verify_semantic_diff` | **live** | Diff post-implementation graph against pre-impl snapshot: dangling refs, new SCCs, unresolved delta. Heuristic `regression_score` with `clean`/`concerning`/`regression` verdict. |
| 10 | *(benchmark)* | **owned by target project CI** | We do not own perf/regression harnesses. |
| 11 | *(deployment)* | **owned by CI/CD** | Out of scope. |
| 12 | *(PR creation)* | **owned by gh CLI / orchestrator** | We supply diff + PRD + impact report; an agent opens the PR. |

## Build & run

```bash
cargo build --release
cargo run --quiet --release
```

`.mcp.json` registers the server with `cargo run --quiet --release` so clients can launch it without a pre-build step (first launch compiles).

## Growing the MCP

Assign an agent from `.claude/agents/` (e.g. `engineer`, `architect`, `dba`) or `.claude/agents/genius/` (e.g. `shannon` for "define the right measure first", `lamport` for "proof before code") to implement the next stage. Each stage lands as:

1. One new object in `tools_list()` in `src/main.rs`
2. One new match arm in `handle_tool_call()`
3. A new source in the stages table above

Extract to modules only when the stage's logic can't reasonably live in one arm.
