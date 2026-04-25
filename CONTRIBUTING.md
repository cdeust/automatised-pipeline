# Contributing to automatised-pipeline

Thanks for considering a contribution. This is a Rust MCP server with
**23 tools, 220 tests, zero warnings, every constant sourced**. Every
change is held to that bar.

---

## What this project is

A Rust 1.94+ stdio JSON-RPC server (no SDK — we own the wire). Indexes
Rust / Python / TypeScript codebases via tree-sitter, persists to
LadybugDB (a property-graph engine the project builds from C++ source),
resolves cross-file imports + calls, detects functional communities via
Louvain + C2 repair, traces execution flows, and exposes hybrid
BM25 + sparse TF-IDF + RRF search. Read-only intelligence — never
writes code, never opens PRs. See [README](README.md) for the full
architecture.

---

## Dev setup

**Prerequisites:** Rust 1.94+ (`rustup install stable`), CMake (LadybugDB
builds its C++ core from source — ~5 minutes first build, cached after).

```bash
git clone https://github.com/cdeust/automatised-pipeline.git
cd automatised-pipeline
cargo build --release
# First build: ~5 minutes (compiles LadybugDB C++ core)
# Subsequent builds: <1 second incremental

cargo test --release        # full test suite (220+ tests)
cargo clippy --release -- -D warnings   # zero warnings policy
cargo fmt --check
```

The `.mcp.json` shipped at the repo root registers the binary with Claude
Code automatically when you open the directory.

---

## Branching + workflow

- `main` is the integration branch.
- Branch naming: `feature/<short-slug>`, `fix/<short-slug>`, `docs/<short-slug>`, `tool/<tool-name>` (for new MCP tools).
- One MCP tool per PR when adding new ones. The JSON schema, the handler,
  the receipt-style response shape, and the integration test all in the
  same commit.
- Conventional commit messages preferred. Reference issue numbers in the
  body when applicable.

---

## Coding standards (excerpt)

Standard Rust style + a few project-specific rules. The full bar comes
from [zetetic coding standards](https://github.com/cdeust/zetetic-team-subagents/blob/main/rules/coding-standards.md):

- **No warnings.** `cargo clippy --release -- -D warnings` must pass.
  Allow-attributes need an inline justification.
- **No `unwrap()` / `expect()`** in non-test code unless paired with a
  comment explaining why the option/result is provably non-`None` /
  non-`Err` at that point.
- **No `unsafe`** without an explicit safety comment per
  [Rust's `unsafe` guidelines](https://doc.rust-lang.org/nomicon/).
- **§8 Source discipline.** Every numeric constant ≥3 significant digits
  needs a `// source:` annotation (citation, benchmark path, measured
  data with date, or "provisional heuristic" with a calibration plan).
- **§4.1 File ≤500 lines, §4.2 function ≤50 lines.** Split along concern
  boundaries, not arbitrary line counts.
- **No `Box<dyn Trait>`** when an enum dispatch table works. Reflection
  for control flow is refused per §7.
- **`Result<T, E>` over panics** at every public API boundary.

---

## Adding an MCP tool

Each new tool must:

1. **Have a JSON schema** enforced at the wire (`schemars` derive, validated
   on the dispatch path).
2. **Return a receipt-style response** with timing + counts. No bare
   payloads.
3. **Use reason codes on error.** No cryptic protocol errors that don't
   map to a documented failure mode.
4. **Have an integration test** that exercises the full stdio path
   (request envelope → JSON-RPC frame → handler → response). Unit tests
   alone are insufficient.
5. **Be documented in the README's tool table** + the relevant pipeline
   stage.

Reference: look at `src/handlers/health_check.rs` for the simplest tool
shape, `src/handlers/index_codebase.rs` for the canonical heavy-lifting
pattern, and `src/handlers/get_impact.rs` for the cross-graph query
pattern.

---

## Modifying graph algorithms

Graph code (Louvain, C2 repair, Tarjan SCC, Leiden, BFS process tracing)
is tested against published reference implementations. Changes here:

1. **Cite the source paper.** Modifications relative to canonical
   pseudocode require a `// source:` block explaining the divergence.
2. **Preserve invariants.** Modularity must monotonically increase across
   Louvain rounds. Tarjan SCC must produce a valid topological order on
   the condensation.
3. **Benchmark before + after.** `benchmarks/graph-algorithms/` has the
   reference fixtures. A regression of >5% on any benchmark blocks
   merge unless explicitly justified.

---

## Testing

```bash
cargo test --release                    # full suite
cargo test --release -- --test-threads=1   # serial mode (debugging flakes)
cargo bench                             # micro-benchmarks
```

The `tests/integration/` suite spins up a real stdio JSON-RPC server
process and exercises the wire protocol. These tests are slow (~30s) but
load-bearing — a wire-level regression is a regression we ship.

---

## What NOT to do

- Don't introduce a new dependency without justification. The Cargo.toml
  is curated; new crates need an issue discussion or an ADR.
- Don't bypass `cargo clippy`. If clippy is wrong, file an upstream
  issue and add a single `#[allow(...)]` with a comment in the same PR.
- Don't add a tool with no integration test.
- Don't merge a graph-algorithm change without before/after benchmarks.

---

## Code of Conduct

This project follows [`CODE_OF_CONDUCT.md`](CODE_OF_CONDUCT.md).

---

## Reporting security issues

See [`SECURITY.md`](SECURITY.md). The MCP server reads filesystem paths
from untrusted input; any path-traversal or injection issue is
high-priority.

---

## License

MIT. Contributions are licensed under the same. See [`LICENSE`](LICENSE).
The graph-theoretic and IR algorithms used remain attributable to their
original authors; the MIT license covers this implementation.
