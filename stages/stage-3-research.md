# Stage 3 research — building a GitNexus-equivalent in Rust

**Status:** verification document. No spec yet. No code. Produced to answer: *what would be necessary to rebuild GitNexus-class code intelligence using our zetetic + genius agents, in Rust, as stage 3 of ai-architect-mcp?* All facts traced to sources.

---

## 1. What GitNexus actually is

GitNexus is an open-source code-intelligence engine, written in TypeScript/Node.js, that indexes a codebase into a knowledge graph and exposes the graph to AI agents via an MCP server. It runs entirely locally — CLI with native bindings or browser via WebAssembly. Created by Abhigyan Patwari.

**Core claim** (their framing, not mine): *"GitNexus precomputes relational intelligence at index time so that a single tool call returns complete context, rather than relying on the LLM to perform multiple rounds of graph queries."* This is the design axiom — the LLM gets complete answers from one call, not a probe-and-walk loop.

### 1.1 Indexing pipeline (6 phases, ordered)

1. **Structure** — walks the file tree, maps folder/file relationships.
2. **Parsing** — extracts functions, classes, methods, and interfaces using Tree-sitter ASTs.
3. **Resolution** — resolves imports, function calls, heritage, constructor inference, and `self`/`this` receiver types across files with language-aware logic.
4. **Clustering** — groups related symbols into functional communities via **Leiden community detection**.
5. **Processes** — traces execution flows from entry points through call chains.
6. **Search** — builds hybrid search indexes (BM25 + semantic embeddings + Reciprocal Rank Fusion).

Each phase produces artifacts the next consumes. This is architecturally identical to the stage-pipeline discipline we've been building.

### 1.2 MCP tool surface (16 tools at latest README)

Per-repo:
- `list_repos` — enumerate indexed repositories
- `query` — process-grouped hybrid search (BM25 + semantic + RRF)
- `context` — 360° symbol view (categorized refs, process participation)
- `impact` — blast-radius analysis with depth grouping and confidence
- `detect_changes` — git-diff impact, maps changed lines to affected processes
- `rename` — multi-file coordinated rename with graph + text search
- `cypher` — raw Cypher queries against the graph

Multi-repo (workgroup scope):
- `group_list`, `group_sync`, `group_contracts`, `group_query`

Prompts (not tools):
- `detect_impact` — pre-commit analysis
- `generate_map` — architecture documentation

### 1.3 Storage — LadybugDB

GitNexus stores the graph in **LadybugDB** — a columnar embedded graph database with Cypher query language, full-text search, vector indices, and ACID transactions. LadybugDB is the direct successor to Kùzu (Kùzu archived in October 2025; its team continued as LadybugDB). Embedded means in-process — no server, no network.

Per GitNexus's README: "LadybugDB native (fast, persistent)" for CLI and "LadybugDB WASM (in-memory, per session)" for browser.

### 1.4 Language support

Per GitNexus docs, the tool supports deep semantic analysis for **8 languages**: TypeScript, JavaScript, Python, Java, Go, Rust, PHP, and Ruby. Each language has its own resolution logic for imports / call graphs / receiver types, which is the hardest part of the pipeline.

### 1.5 **License — critical constraint**

GitNexus is licensed under **PolyForm Noncommercial 1.0.0**. This is NOT an open-source license in the OSI sense; it forbids commercial use. Consequences for us:

- **We cannot fork GitNexus or copy its code.** Not a single file, not a single function, not a single query.
- **We can read their README and published articles** for architectural inspiration — that is fair use of documentation.
- **We must write our implementation independently** using a clean-room approach: the design decisions can be informed by GitNexus, but every line of code must be ours and must trace to our own sources (tree-sitter docs, Kùzu docs, published papers).
- **Given "we're in the partner program of Anthropic"** (user, 2026-04-11), any commercial dependency on PolyForm Noncommercial code is a non-starter. This is why rebuilding, not bundling, is the right path.

---

## 2. Rust ecosystem inventory — what already exists

Per-component audit of crates and their licensing. **Zero invented tools.** Every row is a crate we can actually `cargo add`.

| Pipeline phase | GitNexus uses | Rust equivalent | Crate | License | Notes |
|---|---|---|---|---|---|
| Structure | Node.js `fs` walker | `std::fs` / `walkdir` | stdlib / `walkdir` | MIT/Apache-2.0 | Trivial; likely stdlib only |
| Parsing | tree-sitter (TypeScript bindings) | tree-sitter (Rust crate) | `tree-sitter` + `tree-sitter-<lang>` per grammar | MIT | tree-sitter is C; Rust bindings are first-class. Per-language grammar crates exist (`tree-sitter-python`, `tree-sitter-rust`, etc.) |
| Graph storage | LadybugDB (native + WASM) | **Kùzu** (`kuzu` crate 0.11.3) or RyuGraph fork | `kuzu` | MIT | Kùzu archived Oct 2025 but 0.11.3 is stable and has pre-installed extensions: `algo`, `fts`, `json`, `vector`. LadybugDB is the successor but **I have not verified whether LadybugDB has a Rust crate** — needs confirmation before locking |
| Query language | Cypher | Cypher (via Kùzu/Ryu) | same | same | OpenCypher is supported natively |
| Resolution | per-language logic | per-language logic | **we write this ourselves** | our license | No off-the-shelf replacement. Hardest part. |
| Clustering | Leiden community detection | `fa-leiden-cd` or `single-clustering` | `fa-leiden-cd` or Kùzu's built-in `algo` extension | MIT | Kùzu 0.11.3's `algo` extension may include Leiden natively — needs verification. If yes, **zero new deps** for clustering. |
| Full-text search | BM25 | Tantivy (Lucene-grade BM25) or Kùzu's `fts` extension | `tantivy` or Kùzu built-in | MIT | Tantivy is the reference Rust full-text library (<10ms startup). Alternatively Kùzu 0.11.3 ships `fts`. |
| Semantic embeddings | some JS embedding model | `fastembed` or `candle` or `ort` (ONNX Runtime) | `fastembed` or `candle-core` | Apache-2.0 / MIT | `fastembed` runs embedding models locally with sensible defaults. `candle` (HuggingFace) is lower-level. Vector search itself is covered by Kùzu's built-in `vector` extension. |
| Rank fusion | Reciprocal Rank Fusion | arithmetic | **we write this** | ours | RRF is 5 lines. No crate needed. Source: Cormack, Clarke, Büttcher 2009 "Reciprocal Rank Fusion Outperforms Condorcet and Individual Rank Learning Methods". |
| Process tracing | graph traversal | `kuzu` Cypher queries | `kuzu` | MIT | No separate crate needed — it's just Cypher walks. |
| MCP transport | TypeScript MCP SDK | our own hand-rolled stdio JSON-RPC | existing `src/main.rs` | ours | We already have this. Stage 3 adds new tools to the existing server. |

### 2.1 Headline observation — Kùzu 0.11.3 covers four of six phases out of the box

Kùzu's `algo` + `fts` + `vector` extensions potentially replace three separate external deps. If those extensions are first-class in the Rust crate, the effective new-dep count for stage 3 is:

- `kuzu` (graph + Cypher + algo + fts + vector)
- `tree-sitter` + per-language grammar crates
- **Nothing else for the first MVP.**

No Tantivy, no fa-leiden-cd, no fastembed, no candle — all deferred, replaced by Kùzu extensions. This is a *massive* simplification vs. what I initially sketched. It hinges on verifying Kùzu's Rust crate actually exposes the extensions — this is the #1 open question for Shannon to resolve in the spec phase.

### 2.2 The one thing nobody builds for you — resolution

Resolution is per-language semantic analysis: "what does this import path refer to?", "what function does this call bind to?", "what type does `self` have here?". **No crate does this generically.** Each language has its own rules. GitNexus's supported 8 languages each got hand-written resolution logic, and GitNexus has *internal roadmap documents* (`type-resolution-roadmap.md`, `type-resolution-system.md`) tracking this as an ongoing problem.

For a Rust rewrite, our options:

- **(a) Leverage each language's own LSP server.** Python has pyright, TypeScript has tsc, Rust has rust-analyzer, Go has gopls. Query them for resolved symbols via LSP protocol. Cost: every language adds an external process dependency. Benefit: resolution quality matches the canonical compiler.
- **(b) Parse-only, no resolution.** Stage 3 MVP extracts the bare AST into the graph — functions, classes, imports by literal name — and skips cross-file resolution. Queries can't walk call edges accurately, but they can walk imports and structure. Value is limited but concrete.
- **(c) Write our own resolvers, per language.** Hand-write what GitNexus already hand-wrote. Years of work. Rejected for an MVP.

The honest answer is **(b) for MVP, (a) for v2**. Ship parse-only first, layer LSP integration later when the shape of the graph is validated by real queries.

---

## 3. What the genius agents would own

Task decomposition — who solves what, why they're the right genius, what they produce.

| Phase | Genius | Problem shape | Output |
|---|---|---|---|
| **Measurement definition** — "what does 'understand a codebase' mean operationally?" | **shannon** | Define the right quantity before theorizing. What is the minimum graph schema that carries load for stage 3's queries? What are the information channels (BM25 / vector / graph) and how do they combine? | The spec (`stages/stage-3.md`) — schemas, invariants, measures |
| **Graph schema design** — node labels, edge types, invariants | **liskov** | Abstract data type discipline. Every node label must satisfy a contract. Every edge must preserve invariants under all operations. LSP in the graph sense: a `Method` node is a `Function` node is a `Symbol` node. | A typed Rust `enum NodeLabel` + `enum EdgeKind` with contract comments |
| **Resolution correctness** — proving call edges are only added when the target is provably reachable | **lamport** | Proof before code. The resolution phase is distributed-cause reasoning: you must prove that when you add an edge `A -[:CALLS]-> B`, B is actually the target, not just a name that happens to match. TLA+-style invariants. | Resolver spec with pre/post conditions per language |
| **AST-to-graph encoding** — what signal to extract, what noise to drop | **curie** | Residual with a carrier — extract the signal (semantic structure) while discarding the noise (whitespace, comments, formatting). Curie's canonical move: isolate the tiny thing that matters from the large background. | Per-language extraction rules, field by field |
| **Graph traversal algorithms** — impact analysis, call-chain walks | **dijkstra** | EWDs: correctness of the walk. Impact analysis = transitive closure over `CALLS` edges. Call chain = path enumeration. Every walk needs a termination proof and a cycle-handling rule. | Algorithm specifications with loop invariants |
| **BM25 / search indexing** — if we end up writing our own | **knuth** | TAOCP rigor. BM25 is a well-specified algorithm (Robertson et al.); knuth-level attention is needed to the tokenizer, stemming, and scoring edge cases. Likely deferred since Kùzu `fts` handles it — but if we write our own, Knuth. | Ranked retrieval spec |
| **Order-of-magnitude sanity** — "how big is the graph for a 500k-LOC Rust codebase?" | **fermi** | Mass-balance the whole system. How many nodes? How many edges? How much memory? How long to index? If the answer is "10GB and 3 hours", the architecture is wrong. If it's "100MB and 30 seconds", we ship. | Back-of-envelope numbers with explicit assumptions |
| **MCP tool surface** — what are the smallest messages stage 3 exposes? | **kay** | Each MCP tool is a message. What's the minimum message set that lets an agent do real work? kay's principle: small messages that compose, not fat RPCs. | Tool list + schemas + rationale for each |
| **Rust implementation** | **engineer** | Translate the specs into Rust code with the stage-1/stage-2 discipline (≤40 LOC bodies, atomic writes, zero unwraps, source-cited constants) | `src/main.rs` additions + any extracted modules |
| **Architectural review** | **architect** / **code-reviewer** | Module decomposition. Does stage 3 need its own module at this size? Does the flat-file discipline still hold? | Review file with verdicts + must/should/nice findings |
| **Literature grounding** | **research-scientist** | Verify every algorithmic claim traces to a paper. BM25 → Robertson 1994; Leiden → Traag 2019; RRF → Cormack 2009; tree-sitter → Brunsfeld 2018. | Citation table added to the spec |
| **Graph DB choice** | **dba** | Kùzu vs Ryu vs LadybugDB vs something else. Verify the extensions claim (algo/fts/vector). Evaluate archival risk. | DB decision with sourced reasoning |

**Not in scope for genius agents** (but needed): `mlops` (if we end up using a local embedding model), `security-auditor` (license audit of every new crate we pull in), `test-engineer` (smoke-test design for stage 3 — much harder than stage 1/2 because "correctness" is fuzzier).

---

## 4. Minimum-viable stage 3 — the honest MVP scope

Stage 3 in full parity with GitNexus is months of work. Stage 3 as an *honest minimum* that gives real value and matches our "grow slowly" discipline is much smaller:

### 4.1 MVP in-scope

- **Phase 1 — Structure**: walk the target directory, map file tree.
- **Phase 2 — Parsing**: tree-sitter AST extraction for **one language** — I propose **Rust** (we're dogfooding on our own MCP server). Extract: `File`, `Module`, `Struct`, `Enum`, `Function`, `Impl`, `Trait`, `Use` statements.
- **Phase 3 — Resolution**: limited to *structural* resolution — `Use` statements → resolved module path; `Impl` blocks → the trait/type they implement. **No call-graph resolution** (i.e., we don't try to resolve `foo()` inside a function body to the actual target function). This is option (b) from §2.2.
- **Phase 6 — Search**: **deferred**. No BM25, no vector search, no RRF. Stage 3 MVP exposes only Cypher.

### 4.2 MVP MCP tools (proposed — Shannon will refine)

| Tool | Purpose | Input | Output |
|---|---|---|---|
| `index_codebase` | Phase 1 + 2 + 3 end-to-end. Walk, parse, insert into graph, persist. | `path`, `language`, `output_dir` | `{graph_path, node_count, edge_count, elapsed_ms}` |
| `query_codebase` | Raw Cypher against the indexed graph. The escape hatch / dogfood tool. | `graph_path`, `query` | query result as JSON |
| `verify_codebase` | Run a named set of verification queries (layer violations, orphaned symbols, etc.) and return structured findings. | `graph_path`, `ruleset_name` | list of violations per rule |

Three tools, one language, structural-only resolution. That's it.

### 4.3 MVP explicitly out-of-scope (deferred to 3b, 3c, ...)

- Multi-language support
- Call-graph resolution (requires LSP integration or hand-written per-language logic)
- Leiden clustering
- BM25 / semantic / RRF hybrid search
- Impact analysis (`impact` tool) — needs call graph
- Rename refactoring — needs call graph + multi-file coordination
- Process tracing from entry points — needs call graph
- Multi-repo group operations

Each deferred capability becomes its own stage or sub-stage. The growth rule (`NOTES.md`) still holds: one capability at a time.

---

## 5. Risks and open questions — to resolve before Shannon writes the stage-3 spec

1. **Does the `kuzu` Rust crate expose the `algo`/`fts`/`vector` extensions?** If yes, we get clustering + BM25 + vector search for free. If no, we need Tantivy + fa-leiden-cd + fastembed as three additional deps. **Must verify by reading `docs.rs/kuzu/latest` before locking.**
2. **Kùzu archival risk.** Kùzu archived October 2025. The 0.11.3 release is stable and functional but unmaintained. RyuGraph (predictable-labs fork) and LadybugDB (the original team's successor) are both candidates. We need to pick one and accept the long-term maintenance story. **Decision deferred to `dba` agent.**
3. **Resolution scope for MVP.** Confirm option (b) — structural-only — is acceptable. If the user wants call-graph resolution at MVP, stage 3 grows significantly and needs LSP integration (option a). Dramatically changes scope.
4. **Target language for MVP.** I propose Rust (we dogfood on our own MCP). Alternatives: Python (most common target), TypeScript (GitNexus's own target, most sample data available). User may want something different.
5. **On-disk layout extension.** Stage 3 produces a graph, which is meaningfully different from stage 1/2's flat JSON artifacts. Does `runs/<run_id>/findings/<finding_id>/stage-3.graph/` contain the Kùzu database files directly? Or is the graph global (one graph per `output_dir`, not per finding)? This is an architecture question — Shannon's job.
6. **Verification ruleset source.** For `verify_codebase`, what's in the default ruleset? My list in §3 ("layer violations, cyclic imports, god class, dead symbols") has precedent (Martin, Lakos), but the exact queries need to be spec'd in Cypher. Shannon + dba.
7. **License audit obligation.** Every new crate pulled into Cargo.toml must have its license recorded in a project-level `THIRD_PARTY.md`. Per our "partner program" context this is not optional. `security-auditor` agent.

---

## 6. Source citations

Everything above, traced to where it came from.

| Claim | Source |
|---|---|
| GitNexus overview + architecture | [GitHub README — abhigyanpatwari/GitNexus](https://github.com/abhigyanpatwari/GitNexus) |
| GitNexus pipeline phases + tool list | [hoangyell.com — GitNexus: The Knowledge Graph That Makes AI Agents Actually Understand Your Codebase](https://hoangyell.com/gitnexus-explained/) |
| GitNexus client-side / zero-server framing | [yuv.ai blog — GitNexus: Zero-Server Code Intelligence](https://yuv.ai/blog/gitnexus) |
| GitNexus "precomputes relational intelligence at index time" claim | [pebblous.ai blog — GitNexus Hits #1 on GitHub](https://blog.pebblous.ai/blog/gitnexus-code-knowledge-graph-2026/en/) |
| GitNexus 8-language support + tree-sitter usage | [sitepoint.com — Client-Side RAG: Building Knowledge Graphs in the Browser with GitNexus](https://www.sitepoint.com/client-side-rag-building-knowledge-graphs-in-the-browser-with-gitnexus/) |
| LadybugDB is successor to Kùzu | [ladybugdb.com](https://ladybugdb.com/) + [dbdb.io — LadybugDB](https://dbdb.io/db/ladybugdb) |
| Kùzu archival October 2025 | [docs.rs — kuzu 0.11.3](https://docs.rs/crate/kuzu/latest) + [GitHub — kuzudb/kuzu](https://github.com/kuzudb/kuzu) |
| Kùzu Rust crate + MIT license | [lib.rs — kuzu](https://lib.rs/crates/kuzu) + [crates.io — kuzu 0.10.1](https://crates.io/crates/kuzu/0.10.1) |
| RyuGraph fork | [GitHub — predictable-labs/ryugraph](https://github.com/predictable-labs/ryugraph) |
| Tantivy + BM25 scoring | [GitHub — quickwit-oss/tantivy](https://github.com/quickwit-oss/tantivy) + [tantivy/src/query/bm25.rs](https://github.com/quickwit-oss/tantivy/blob/main/src/query/bm25.rs) |
| Leiden Rust crates | [crates.io — fa-leiden-cd](https://crates.io/crates/fa-leiden-cd) + [GitHub — fixed-ai/fa-leiden-cd](https://github.com/fixed-ai/fa-leiden-cd) + [crates.io — single-clustering](https://crates.io/crates/single-clustering) |
| Leiden algorithm origin | [Wikipedia — Leiden algorithm](https://en.wikipedia.org/wiki/Leiden_algorithm) (Traag, Waltman, van Eck 2019) |
| GitNexus license | [GitHub — abhigyanpatwari/GitNexus README — PolyForm Noncommercial 1.0.0](https://github.com/abhigyanpatwari/GitNexus/blob/main/README.md) |
| Semantic Spacetime / hypergraphs in LadybugDB | [ai.plainenglish.io — Semantic Spacetime in LadybugDB](https://ai.plainenglish.io/semantic-spacetime-in-ladybugdb-hypergraphs-metagraphs-and-memory-without-leaving-cypher-b753bf7e8464) |

**Papers referenced but not fetched (the specs will need full citations when Shannon writes them):**
- Brunsfeld 2018 — *Tree-sitter: An Incremental Parsing System for Programming Tools*
- Robertson, Walker, Jones, Hancock-Beaulieu, Gatford 1994 — *Okapi at TREC-3* (origin of BM25)
- Cormack, Clarke, Büttcher 2009 — *Reciprocal Rank Fusion Outperforms Condorcet and Individual Rank Learning Methods*
- Traag, Waltman, van Eck 2019 — *From Louvain to Leiden: guaranteeing well-connected communities*
- Lakos 1996 — *Large-Scale C++ Software Design* (cyclic-import criterion)
- Martin 2012 — *Clean Architecture* (layer-violation criterion)

---

## 7. Verdict

Building a GitNexus-equivalent in Rust is technically feasible without writing a single line of GitNexus-owned code. Kùzu's built-in extensions collapse three would-be dependencies into one, provided they're exposed through the Rust crate (§5 risk #1). The hardest problem — per-language resolution — can be deferred to post-MVP by shipping a structural-only first cut.

The 6-phase GitNexus pipeline maps cleanly to a multi-sub-stage growth: each of structure / parse / resolve / cluster / process / search can become its own MCP tool or sub-stage without violating our growth rule.

**Before this becomes a spec, the user needs to answer:**
1. Is the MVP scope (§4) acceptable? Three tools, one language (Rust), structural-only resolution, no clustering/search?
2. Kùzu or Ryu or LadybugDB for the graph backend? (dba agent can evaluate in parallel with Shannon's spec work.)
3. Target language for MVP: Rust (dogfood), Python, or something else?
4. License discipline: add `THIRD_PARTY.md` and require `security-auditor` sign-off on any new crate? (Partner-program context makes this non-optional.)

Once those are answered, dispatching `shannon` + `dba` + `architect` in parallel produces the stage-3 spec faster than sequential work — each has an independent sub-problem.

---

*This document is the verification. The stage-3 spec (`stages/stage-3.md`) is deferred until §5 risks and §7 questions are resolved.*
