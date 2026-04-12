# Stage 3 — Database Evaluation for Codebase Intelligence Engine

**Author:** dba agent | **Date:** 2026-04-11 | **Standard:** zetetic (every claim sourced or marked unknown)

---

## 1. Kuzu 0.11.3 Rust Crate Audit

### Version and availability
- **Crate name:** `kuzu` on crates.io ([crates.io/crates/kuzu](https://crates.io/crates/kuzu))
- **Latest version:** 0.11.3 ([docs.rs/crate/kuzu/latest](https://docs.rs/crate/kuzu/latest))
- **License:** MIT ([github.com/kuzudb/kuzu/blob/master/LICENSE](https://github.com/kuzudb/kuzu/blob/master/LICENSE))
- **Archived:** Yes, October 10, 2025 by Kuzu Inc. ([The Register](https://www.theregister.com/2025/10/14/kuzudb_abandoned/))
- **Last release:** v0.11.3 on 2025-10-10 (same day as archival)

### Extensions (algo, fts, json, vector)
- **VERIFIED:** Kuzu 0.11.3 ships with four pre-installed extensions: `algo`, `fts`, `json`, `vector`. No manual INSTALL required. ([docs.kuzudb.com/extensions](https://kuzudb.github.io/docs/extensions/), [release notes](https://github.com/kuzudb/kuzu/releases/tag/v0.11.3))
- **VERIFIED:** The official extension server was discontinued at 0.11.3 — only pre-installed extensions are available without a local extension server.
- **VERIFIED:** Extensions are accessible via Cypher queries (e.g., `CALL CREATE_FTS_INDEX(...)`) through the standard query API. The Rust crate exposes `Connection::query()` which executes arbitrary Cypher strings. Extensions are not exposed as dedicated Rust API methods — they are invoked via Cypher statements, same as Python/Node bindings. ([docs.kuzudb.com/client-apis/rust](https://docs.kuzudb.com/client-apis/rust/))

### FTS extension specifics
- **VERIFIED:** `CALL CREATE_FTS_INDEX(table, index_name, [properties])` creates BM25 full-text indexes on node table STRING properties. Uses Okapi BM25 scoring internally. ([docs.kuzudb.com/extensions/full-text-search](https://docs.kuzudb.com/extensions/full-text-search/))
- **Limitation:** FTS indexes can only be created on node tables, not edge tables.

### Algo extension specifics
- **VERIFIED:** Available algorithms: PageRank, Louvain, Weakly Connected Components, Strongly Connected Components, K-Core Decomposition. ([docs.kuzudb.com/extensions/algo](https://docs.kuzudb.com/extensions/algo/))
- **FALSIFIED:** The research doc implied Leiden might be available. **Leiden is NOT in Kuzu's algo extension.** Only Louvain is available. Louvain is the predecessor; Leiden (Traag 2019) is strictly better but not implemented. This means a separate Leiden crate (e.g., `fa-leiden-cd`) would be needed if Leiden specifically is required. Louvain may be acceptable for MVP.

### Vector extension specifics
- **VERIFIED:** Vector index extension exists. Supports HNSW indexing. Invoked via Cypher. ([github.com/kuzudb/kuzu README](https://github.com/kuzudb/kuzu))
- **CANNOT VERIFY from docs alone:** Exact distance metrics supported (cosine vs L2), HNSW tuning parameters (ef_construction, m). Needs `cargo build` test.

### Build mechanism
- **VERIFIED:** The Rust crate builds Kuzu's C++ library from source via CMake by default. Statically links against parquet, arrow, utf8proc, antlr4_cypher, antlr4_runtime, re2. ([docs.rs/crate/kuzu/0.0.5-pre/source/build.rs](https://docs.rs/crate/kuzu/0.0.5-pre/source/build.rs), [docs.kuzudb.com/developer-guide](https://docs.kuzudb.com/developer-guide/))
- **Build requirements:** CMake >= 3.15, C++20 compiler (GCC 12+, Clang 18+), Rust 1.81.0+.
- **Alternative:** Set `KUZU_LIBRARY_DIR` env var to use pre-built libraries; `KUZU_SHARED` for dynamic linking.
- **Risk:** Large C++ build. First `cargo build` will be slow (minutes, not seconds). CMake + C++20 compiler are hard requirements.

### Archival risk assessment
- Kuzu is archived, read-only, no future patches. Any bug found in 0.11.3 cannot be fixed upstream.
- Two successor projects exist (LadybugDB, RyuGraph) — see below.
- For a short-lived MVP, 0.11.3 is functional. For long-term, migration to a maintained fork is inevitable.

---

## 2. LadybugDB Rust Binding Audit

### Crate existence
- **VERIFIED:** LadybugDB has a Rust crate named `lbug` on crates.io. ([crates.io/crates/lbug](https://crates.io/crates/lbug), [docs.rs/lbug/latest/lbug](https://docs.rs/lbug/latest/lbug/))
- **Also found:** A separate `ladybug` crate exists on crates.io ([crates.io/crates/ladybug](https://crates.io/crates/ladybug)) — unclear if this is the same or a different package. The official one appears to be `lbug`.

### Version and activity
- **Latest GitHub release:** v0.15.2, released 2026-03-18 ([github.com/LadybugDB/ladybug/releases](https://github.com/LadybugDB/ladybug/releases))
- **Actively maintained:** Yes. Recent features include multiple labels, WAL refactoring, MVCC bug fixes.
- **CANNOT VERIFY:** Whether the `lbug` crate on crates.io tracks v0.15.2 or lags behind. Needs `cargo add lbug` test.

### License
- **VERIFIED:** MIT license. ([lib.rs/crates/lbug](https://lib.rs/crates/lbug), GitHub repo)

### Relationship to Kuzu
- **VERIFIED:** LadybugDB is the continuation by the original Kuzu team after Kuzu Inc. archived the project. It is a direct successor codebase, not a clean-room reimplementation. ([ladybugdb.com](https://ladybugdb.com/), [dbdb.io/db/ladybugdb](https://dbdb.io/db/ladybugdb))

### Build mechanism
- **VERIFIED:** Same as Kuzu — builds and statically links C++ library from source via Cargo.

### Extension status
- **CANNOT VERIFY from web search alone:** Whether `lbug` 0.15.x still ships the same pre-installed extensions (algo, fts, json, vector). The codebase evolved from Kuzu 0.11.3, so they likely persist, but this needs a `cargo build` + Cypher test to confirm.

### Assessment
LadybugDB is the strongest long-term option if the API is stable and the crate tracks releases. The risk is that it's a young project (first release ~Q4 2025) and the Rust crate may have rough edges.

---

## 3. RyuGraph Audit

### Crate existence
- **VERIFIED:** `ryugraph` crate exists on crates.io, version 25.9.0 visible. ([crates.io/crates/ryugraph/25.9.0](https://crates.io/crates/ryugraph/25.9.0))
- **Docs:** [docs.rs/ryugraph/latest/ryugraph](https://docs.rs/ryugraph/latest/ryugraph/)

### Version and activity
- **Latest GitHub release:** v25.9.2, released 2025-12-06. ([github.com/predictable-labs/ryugraph/releases](https://github.com/predictable-labs/ryugraph/releases))
- **Maintained by:** Predictable Labs, Inc.
- **CANNOT VERIFY:** Whether there have been commits in 2026 (Q1-Q2). The last known release is December 2025. This is a 4-month gap as of April 2026 — not alarming but worth monitoring.

### License
- **VERIFIED:** MIT license (inherited from Kuzu). ([github.com/predictable-labs/ryugraph](https://github.com/predictable-labs/ryugraph))

### Relationship to Kuzu
- **VERIFIED:** RyuGraph is a community fork of Kuzu, not from the original team. The original team went to LadybugDB.

### Build mechanism
- **VERIFIED:** Same pattern — builds and statically links C++ from source.

### What it adds over Kuzu 0.11.3
- **CANNOT VERIFY from search alone.** The versioning scheme (25.9.x) diverges from Kuzu's (0.11.x), suggesting active development, but specific new features vs. Kuzu 0.11.3 are not documented in search results.

### Assessment
RyuGraph is a viable fork but carries higher risk than LadybugDB: it's a community effort (not the original team), the last release is 4 months old, and it's unclear what it adds beyond Kuzu. It's a fallback, not a first choice.

---

## 4. SQLite + FTS5 + sqlite-vec Audit

### rusqlite + FTS5
- **VERIFIED:** `rusqlite` with `bundled-full` feature enables FTS5 at compile time. The bundled SQLite build includes compilation flags for FTS3, FTS3_PARENTHESIS, FTS5, JSON1, LOAD_EXTENSION, RTREE, STAT4. ([github.com/rusqlite/rusqlite](https://github.com/rusqlite/rusqlite), [docs.rs/crate/rusqlite/latest/features](https://docs.rs/crate/rusqlite/latest/features))
- **FTS5 uses BM25:** Yes, FTS5 provides `bm25()` ranking function natively.

### sqlite-vec
- **VERIFIED:** `sqlite-vec` has a Rust crate that exposes `sqlite3_vec_init`. It registers via `sqlite3_auto_extension()` with rusqlite. Requires `unsafe` transmute. ([alexgarcia.xyz/sqlite-vec/rust.html](https://alexgarcia.xyz/sqlite-vec/rust.html), [github.com/asg017/sqlite-vec/blob/main/examples/simple-rust/demo.rs](https://github.com/asg017/sqlite-vec/blob/main/examples/simple-rust/demo.rs))
- **Requires:** rusqlite `bundled` feature enabled.

### Cypher support
- **FALSIFIED:** SQLite has NO Cypher support. SQLite uses SQL. There is no known Cypher-to-SQL transpiler for SQLite. The user explicitly requires Cypher ("cypher graph relational check"). This means **SQLite cannot satisfy the Cypher requirement** without building a custom query translation layer, which is a large engineering effort and defeats the purpose.

### Property graph model
- **NOT NATIVE:** SQLite is relational. Property graphs must be modeled as tables (nodes table + edges table + properties as columns or JSON). This works but is manual and loses the native graph traversal semantics (variable-length path queries, pattern matching).

### Graph algorithms
- **NOT AVAILABLE:** No built-in graph algorithms. Would need external crates (fa-leiden-cd, petgraph, etc.).

### Capability matrix

| Requirement | Status |
|---|---|
| Property graph storage | YELLOW — manual via relational tables |
| Cypher query language | RED — not available, SQL only |
| Full-text search (BM25) | GREEN — FTS5 native |
| Vector indices | GREEN — via sqlite-vec extension |
| Community detection | RED — not available |
| ACID transactions | GREEN — native |
| Embedded (in-process) | GREEN — native |
| Scale (848K+ nodes) | GREEN — proven by user's Swift reference |
| Persistence | GREEN — native |

### Assessment
SQLite is proven at this scale and the FTS5 + sqlite-vec combo is solid. However, the absence of Cypher and native graph semantics makes it a poor fit for a codebase intelligence engine that needs pattern matching and path traversal. **Eliminated by the Cypher requirement.**

---

## 5. DuckDB Audit

### Rust crate
- **VERIFIED:** `duckdb` crate exists on crates.io. ([crates.io/crates/duckdb](https://crates.io/crates/duckdb), [docs.rs/duckdb/latest/duckdb](https://docs.rs/duckdb/latest/duckdb/))
- **License:** MIT ([lib.rs/crates/duckdb](https://lib.rs/crates/duckdb))

### Property Graph Queries (DuckPGQ)
- **VERIFIED:** DuckPGQ is a community extension implementing SQL/PGQ (SQL:2023 standard). ([duckpgq.org](https://duckpgq.org/), [duckdb.org/community_extensions/extensions/duckpgq](https://duckdb.org/community_extensions/extensions/duckpgq))
- **CRITICAL DISTINCTION:** DuckPGQ implements **SQL/PGQ, NOT Cypher**. SQL/PGQ uses Cypher-*inspired* visual pattern matching syntax (`()--[]-->()`) inside SQL MATCH clauses, but it is a different language from OpenCypher. You cannot run raw Cypher queries against DuckDB. ([duckpgq.org/documentation/sql_pgq](https://duckpgq.org/documentation/sql_pgq/))
- **Property graphs were transient** (connection-scoped) until DuckPGQ v0.1.0 / DuckDB v1.1.3, after which they became persistent.

### Full-text search
- **VERIFIED:** DuckDB has a core FTS extension using BM25 (Okapi BM25). Created via PRAGMA. ([duckdb.org/docs/current/core_extensions/full_text_search](https://duckdb.org/docs/current/core_extensions/full_text_search))

### Graph algorithms
- **VERIFIED:** DuckPGQ includes some graph functions. ([duckpgq.org/documentation/graph_functions](https://duckpgq.org/documentation/graph_functions/))
- **CANNOT VERIFY:** Whether Louvain or Leiden is among them. Documentation mentions graph functions but specific algorithm list not confirmed from search.

### Capability matrix

| Requirement | Status |
|---|---|
| Property graph storage | YELLOW — via DuckPGQ extension (graph as overlay on relational tables) |
| Cypher query language | RED — SQL/PGQ only, not OpenCypher |
| Full-text search (BM25) | GREEN — core FTS extension |
| Vector indices | YELLOW — not verified as native; may need external |
| Community detection | YELLOW — some graph functions in DuckPGQ, unconfirmed scope |
| ACID transactions | GREEN — native |
| Embedded (in-process) | GREEN — native |
| Scale (848K+ nodes) | GREEN — analytical engine, handles large datasets |
| Persistence | GREEN — native |

### Assessment
DuckDB is a powerful analytical engine but graph support is via a community extension using SQL/PGQ, not Cypher. If the user insists on Cypher specifically, DuckDB is eliminated. If SQL/PGQ's Cypher-inspired syntax is acceptable, DuckDB becomes viable but adds complexity (two extensions: DuckPGQ + FTS, both community-maintained).

---

## 6. Comparison Matrix

| Requirement | Kuzu 0.11.3 | LadybugDB (lbug) | RyuGraph | SQLite + ext | DuckDB + ext |
|---|---|---|---|---|---|
| Property graph | **GREEN** native | **GREEN** native | **GREEN** native | YELLOW manual | YELLOW overlay |
| Cypher | **GREEN** OpenCypher | **GREEN** OpenCypher | **GREEN** OpenCypher | **RED** none | **RED** SQL/PGQ only |
| FTS (BM25) | **GREEN** built-in | GREEN (likely) | GREEN (likely) | **GREEN** FTS5 | **GREEN** core ext |
| Vector indices | **GREEN** HNSW built-in | GREEN (likely) | GREEN (likely) | GREEN sqlite-vec | YELLOW unverified |
| Graph algorithms | YELLOW Louvain not Leiden | YELLOW (likely same) | YELLOW (likely same) | **RED** none | YELLOW partial |
| ACID | **GREEN** | **GREEN** | **GREEN** | **GREEN** | **GREEN** |
| Embedded | **GREEN** | **GREEN** | **GREEN** | **GREEN** | **GREEN** |
| Scale 848K+ | GREEN (designed for it) | GREEN | GREEN | **GREEN** proven | **GREEN** |
| Persistence | **GREEN** | **GREEN** | **GREEN** | **GREEN** | **GREEN** |
| Rust crate exists | **GREEN** | **GREEN** | **GREEN** | **GREEN** | **GREEN** |
| License (MIT) | **GREEN** | **GREEN** | **GREEN** | **GREEN** (public domain) | **GREEN** |
| Actively maintained | **RED** archived | **GREEN** v0.15.2 Mar 2026 | YELLOW last release Dec 2025 | **GREEN** | **GREEN** |

---

## 7. Recommendation

**Primary: LadybugDB (`lbug` crate)** — pending a `cargo build` smoke test.

**Rationale:**
1. It is the direct successor to Kuzu by the original team, meaning the API is familiar, the extensions likely carry forward, and the codebase has the deepest institutional knowledge.
2. It is the only option that is (a) actively maintained (v0.15.2, March 2026), (b) native property graph with OpenCypher, (c) has built-in FTS + vector + graph algorithms, and (d) has a Rust crate.
3. MIT license.

**Fallback: Kuzu 0.11.3 (`kuzu` crate)** — if `lbug` fails to compile or its extensions diverge from Kuzu's.

Kuzu 0.11.3 is frozen but functional. It can serve as the MVP database while LadybugDB matures. The migration path from `kuzu` to `lbug` should be low-friction since LadybugDB is a direct fork.

**Eliminated:**
- **SQLite** — no Cypher, no native graph model. Proven at scale but wrong paradigm for this use case.
- **DuckDB** — SQL/PGQ is not Cypher. Community extension adds fragility. Wrong tool for a graph-first workload.
- **RyuGraph** — viable but community fork without the original team. Choose LadybugDB over RyuGraph for the same reason you'd choose the upstream maintained by the authors.

**On Leiden vs Louvain:** The research doc assumed Leiden might be in the algo extension. It is not — only Louvain is available. For MVP, Louvain is acceptable (it's the same community detection family). For post-MVP, either (a) wait for LadybugDB to add Leiden, or (b) use `fa-leiden-cd` crate as an external algorithm on the extracted graph.

---

## 8. Open Questions (cannot verify without `cargo build`)

1. **Does `lbug` crate compile on macOS with the current toolchain?** The C++ build requires CMake + C++20 compiler. Must test.
2. **Does `lbug` 0.15.x retain the pre-installed extensions (algo, fts, json, vector)?** The research doc verified this for Kuzu 0.11.3 but LadybugDB may have reorganized extensions.
3. **What is the actual `lbug` crate version on crates.io?** Does it track v0.15.2 or lag behind?
4. **Does the Kuzu/LadybugDB vector extension support cosine distance?** HNSW is confirmed but distance metric options are undocumented in search results.
5. **What is the cold-start build time for the C++ static link?** This affects CI and developer experience.
6. **Is the `lbug` API a drop-in replacement for `kuzu`?** If so, starting with `kuzu` (known-good) and migrating to `lbug` later is low-risk.
7. **Does the blocking stdio MCP server conflict with CMake build dependencies at compile time?** No runtime concern (embedded = in-process), but build-time toolchain requirements must be validated.

---

*All claims above are sourced from web searches conducted 2026-04-11. "VERIFIED" means confirmed by at least one authoritative source (crates.io, docs.rs, GitHub, official docs). "CANNOT VERIFY" means the information was not available in search results and requires hands-on testing.*
