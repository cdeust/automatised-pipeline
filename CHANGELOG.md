# Changelog

All notable changes to this project will be documented here. Format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.0.8] — Multi-language parser expansion (Java, Kotlin, Swift, Objective-C, C, C++, Go)

### Added

- **Seven new tree-sitter parsers** under `src/parser/`:
  `java.rs`, `kotlin.rs`, `swift.rs`, `objc.rs`, `c.rs`, `cpp.rs`, `go.rs`.
  Adds JVM (Java + Kotlin), Apple (Swift + Objective-C), systems
  (C + C++) and Go to the previously-shipped Rust / Python / TypeScript
  trio. `parser/mod.rs` registers all 10 languages; `tool_schemas.rs`
  exposes them in `index_codebase` / `analyze_codebase` language hints.
- **Grammar dependencies** (Cargo.toml): `tree-sitter-java`,
  `tree-sitter-kotlin-ng`, `tree-sitter-swift`, `tree-sitter-objc`,
  `tree-sitter-c`, `tree-sitter-cpp`, `tree-sitter-go`. All MIT or
  Apache-2.0; all official tree-sitter grammars on crates.io.

### Changed

- `do_analyze_codebase`: replaced the explicit Rust/Python/TypeScript
  match with a generic `lang.as_str()` dispatch so LSP-enhanced
  resolution flows through to every supported language.
- Each new parser extracts typed symbols matching the existing
  `graph_store` schema (entities + edges) so the property graph
  remains polyglot-uniform.

### Migration notes

- First build is slower: each new tree-sitter grammar carries C
  source that must compile through `cmake` / `cc`. Subsequent
  incremental builds reuse the per-grammar caches.

## [0.0.7] — Rename binary `ai-architect-mcp` → `automatised-pipeline`

### Changed

- **Binary renamed** from `ai-architect-mcp` to `automatised-pipeline`
  to match the project / plugin / repository name. The Cortex
  `ap_bridge.py` allowlist already accepts `automatised-pipeline`;
  the legacy `ai-architect-mcp` identifier was a stale carryover from
  the project's earlier life as the umbrella `ai-architect` pipeline.
  Affected files: `Cargo.toml` `[[bin]] name`, `bin/ensure-binary.sh`,
  `.mcp.json`, `.github/workflows/release.yml`, `.claude/hooks/session-start.sh`.

### Migration notes

- Release artifacts are now named `automatised-pipeline-{os}-{arch}.tar.gz`
  (was `ai-architect-mcp-*`). Consumers (e.g. Cortex `pipeline_install_release.py`)
  must update their download URLs.
- Built binary path is now `target/release/automatised-pipeline`.
- The Rust crate name (`[package].name`) is unchanged at `ai-architect-mcp`
  to preserve crate identity for any downstream Cargo dependents.

## [0.0.6] — Self-locating plugin MCP launcher

### Fixed

- **`ai-architect` MCP server failed to connect from any non-plugin CWD.**
  The `.mcp.json` launcher relied on Claude Code injecting
  `CLAUDE_PLUGIN_ROOT`, which was not happening reliably. The fallback
  `${CLAUDE_PLUGIN_ROOT:-$(cd "$(dirname "$0")" && pwd)}` is broken
  under `bash -c` (where `$0` is `bash`, not the script path), so
  `$ROOT` resolved to the user's project directory — where
  `target/release/ai-architect-mcp` does not exist. Replaced the bash
  command with a Python one-liner that reads
  `~/.claude/plugins/installed_plugins.json` (always at a fixed
  absolute path) to discover the plugin install path, then `execvp`s
  the Rust binary, falling back to `bin/ensure-binary.sh` if the
  binary is missing. No CWD or env dependency. Users in any project
  now get the MCP server on plugin update — no per-project
  configuration required.

## [0.0.5] — Resilient install: pre-build the MCP binary

### Fixed

- **Inline `cargo run --release` fallback in `.mcp.json` blocked MCP
  startup.** When `target/release/ai-architect-mcp` was absent (fresh
  install or first session after a checkout), the launcher invoked
  `cargo run --release`, which can take 2–3 minutes for a cold rust
  toolchain. Claude Code's MCP startup timeout fires long before that,
  so the server appeared "disconnected" with no actionable message.
  Replaced with a fail-fast launcher: check binary → if missing, run
  `bin/ensure-binary.sh verbose` → re-check → if still missing, exit
  1 with a `FATAL` message printing the exact `cargo build` command
  to run. Never compiles inline during MCP startup.

### Added

- `bin/ensure-binary.sh` — idempotent build script. Exits 0 fast when
  `target/release/ai-architect-mcp` exists and is newer than every
  file under `src/` and `Cargo.{toml,lock}`. Otherwise runs
  `cargo build --release` with progress on stderr only (stdout is
  reserved for the MCP protocol). Distinct exit codes:
  127 (cargo not in PATH), 1 (build failure or post-build sanity
  failure). Runs in two modes: `quiet` (default; errors only) and
  `verbose` (progress + timing).
- `session-start.sh` hook now invokes `ensure-binary.sh verbose`
  BEFORE Claude Code attempts to connect MCP servers. First-time
  install builds the binary synchronously during the session-start
  banner; subsequent sessions exit instantly. Hook continues even on
  build failure — the `.mcp.json` launcher surfaces the error
  cleanly on `/mcp`.

## [0.0.4] — Idempotent BM25 index rebuild

### Fixed

- `search::bm25::build_index` now wipes ``index_dir`` before calling
  `Index::create_in_dir`. Tantivy refuses to reuse a directory that
  already contains an index (`Index already exists`), so consecutive
  runs of `analyze_codebase` (e.g., Cortex's `ingest_codebase` with
  `force_reindex=true`) failed with that error. The BM25 index is a
  derived artifact rebuilt from the live graph, so removing it is
  safe.

## [0.0.3] — Schema-guarded edge resolution

### Added

- `is_known_rel_table` helper in `graph_store.rs` — public predicate
  over `REL_TABLES` so producers that build relationship-table names
  from runtime symbol labels can validate before insertion instead of
  failing inside the graph driver.
- `Imports_File_Method` declared in `REL_TABLES`; previously a method
  imported directly from a file produced a dropped edge with no
  recoverable target table.

### Fixed

- `resolver::resolve_single_import`, `resolve_glob_import`,
  `resolve_calls`, and `resolve_field_type_uses` now consult
  `is_known_rel_table` before staging an edge. Unknown labels are
  logged (first 8 occurrences via an `AtomicU64` counter to bound
  log volume) and the edge is dropped — this replaces the previous
  hard failure path when a new caller/target label combination
  appeared at runtime.
- `lsp_resolver::try_add_lsp_edge` applies the same guard to
  LSP-derived edges (rust-analyzer / pyright / tsserver).

### Added — public-readiness baseline (carried over from Unreleased)

- Public-readiness baseline: LICENSE (MIT, sole independent author),
  CONTRIBUTING.md, CODE_OF_CONDUCT.md, SECURITY.md.
- GitHub issue templates (bug / feature / audit-finding) and PR template
  with audit-cycle checklist.

## [0.0.2] — Stage 1–9 wired + 23 MCP tools

### Added

- 23 MCP tools across pipeline stages 0 through 9:
  - Stage 0: `health_check`
  - Stage 1: `extract_finding`, `refine_finding`
  - Stage 2: `start_verification`, `append_clarification`,
    `finalize_verification`, `abort_verification`
  - Stage 3a: `index_codebase`, `query_graph`, `get_symbol`
  - Stage 3b: `resolve_graph`, `lsp_resolve`
  - Stage 3c: `cluster_graph`, `get_processes`, `get_impact`
  - Stage 3d: `search_codebase`, `get_context`, `analyze_codebase`,
    `detect_changes`
  - Stage 4: `prepare_prd_input`
  - Stage 6: `validate_prd_against_graph`
  - Stage 8: `check_security_gates`
  - Stage 9: `verify_semantic_diff`
- LadybugDB property graph with 16 node labels, 36+ relationship tables.
- tree-sitter AST extractors for Rust, Python, TypeScript.
- Cross-file resolution (imports, calls, impls) with confidence scoring;
  optional LSP deep resolution (rust-analyzer / pyright /
  typescript-language-server).
- Inline Louvain community detection with C2 repair.
- BFS execution-flow tracing from entry points.
- Hybrid BM25 + sparse TF-IDF + RRF search index (Tantivy-backed).
- Tarjan SCC for cycle detection in semantic-diff.
- 220 tests passing, zero clippy warnings, every numeric constant sourced.

### Architecture

- Hand-rolled stdio JSON-RPC 2.0 (no SDK — owns the wire).
- Clean Architecture with strict module boundaries:
  `transport → server → handlers → core modules → persistence`.

---

For pre-0.0.2 history (initial scaffolding, dependency selection),
see git log. The project entered semantic-versioned releases at v0.0.2.
