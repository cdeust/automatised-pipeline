# Stage 3b-v2 — Polyglot code-intelligence analyzer (end-result ≥85% across 8 languages)

**Status:** spec (pre-implementation). Supersedes the Rust-centric draft of stage-3b-v2 dated 2026-04-17 in full. Depends on 3a (complete) and 3b-v1 (shipped at ~46% single-language resolution on our own Rust codebase).

**Two load-bearing user corrections (2026-04-11), internalized in this rewrite:**

> "The 85% apply to the end result analysis not the entry point."

> "The analyzer is not just rust codebase analysis but an agnostic codebase evaluation, meaning it can analyse swift, kotlin, java, rust, typescript, angular, go, react, whichever."

The first correction moves the measurement from "% of call-site refs that became `Calls` edges" to "% of consumer-tool answers that are correct". Resolution rate becomes one input, not the target. The second correction re-scopes the analyzer from "Rust with two stubs" to "language-neutral core + per-language adapters" covering Rust, Swift, Kotlin, Java, Python, TypeScript, JavaScript, and Go as first-class, plus TS-framework-aware handling of Angular and React.

Precedent for the 8-language shape: **GitNexus** (2024) is an open, polyglot code-intelligence graph that ships first-class support for Rust, Swift, Kotlin, Java, Python, TypeScript, JavaScript, and Go via tree-sitter adapters over a shared graph schema — the same envelope this spec commits to. Precedent for language-neutral name resolution: **Stack Graphs** (Creswick et al., GitHub Next, archived 2025; foundational paper Antwi-Boasiako et al., *Stack Graphs: Name Resolution at Scale*, 2022) formalizes per-language scope definitions that drive a single language-agnostic resolution engine. Industry interchange format: **SCIP** (Sourcegraph Code Intelligence Protocol, 2022, superset of Microsoft **LSIF** 2019) as the cross-language indexing envelope we align with for symbol identity.

---

## 1. Target — 85% on the end-result harness, across 8 languages

### 1.1 The metric that matters

The 85% target is defined as:

> When an agent calls a consumer-facing MCP tool, the answer is correct-and-complete with probability ≥0.85, averaged across the reference corpus.

Consumer-facing tools are the ones agents actually invoke to make decisions. Enumerated from `src/main.rs` dispatch:

| Tool | What correctness means |
|---|---|
| `search_codebase` | The returned symbol set matches the intended named entity (right `Class`/`Interface`/`Function`), no duplicates across aliases. |
| `get_context` | The returned neighborhood includes all first-hop callers and callees and nothing unrelated. |
| `get_symbol` | Fully-qualified name, kind, visibility, source range resolve to the declaration the agent meant. |
| `get_impact` | Blast radius (transitive reverse-callers, implementors) is complete within the indexed scope. |
| `query_graph` | Cypher returns the same rowset that a human executing the query by hand would return. |
| `cluster_graph` / `get_processes` | Community assignment is stable and interpretable (same symbol lands in the same cluster across runs). |
| `prepare_prd_input` | Extracted context references only symbols that actually exist and are relevant. |
| `validate_prd_against_graph` | Detected missing references are genuine misses; detected present references truly exist. |

Internal/engineering tools (`index_codebase`, `resolve_graph`, `lsp_resolve`, `extract_finding`, `start_verification`, etc.) are **not** in the 85% metric directly. They feed it.

### 1.2 Why resolution rate is an input, not the target

A 60% resolver can yield 90% end-result accuracy if the unresolved 40% is dead weight (internal `Vec::push` calls, JS `Array.prototype.forEach` invocations that no `get_impact` query ever asks about). Conversely, a 95% resolver that mis-resolves 5% of class-name and interface-name lookups destroys `get_impact` and `search_codebase` simultaneously because those are the queries agents actually run. **The harness measures the output of the agent-facing tools, not the count of edges.**

### 1.3 Scope — 8 languages as first class, 2 framework layers

| Language | Tree-sitter grammar | LSP server (primary) | Notes |
|---|---|---|---|
| Rust | `tree-sitter-rust` | `rust-analyzer` | reference implementation (already shipped) |
| Swift | `tree-sitter-swift` | `sourcekit-lsp` | protocols, extensions, actors |
| Kotlin | `tree-sitter-kotlin` | `kotlin-language-server` | extension fns, data classes, JVM interop |
| Java | `tree-sitter-java` | `jdtls` (Eclipse JDT LS) | anchor for JVM semantics |
| Python | `tree-sitter-python` | `pyright` / `pylsp` | dynamic; MRO resolution |
| TypeScript | `tree-sitter-typescript` (TS + TSX) | `typescript-language-server` | covers Angular + React |
| JavaScript | `tree-sitter-javascript` | `typescript-language-server` | ES modules, prototype chain |
| Go | `tree-sitter-go` | `gopls` | structural interfaces, method sets |

Grammar crate availability confirmed against crates.io index as of 2026-04-11; all eight have published Rust bindings. Angular and React are TypeScript-dialect overlays — decorators, JSX/TSX, component-prop inference — handled by the TS adapter's framework sub-modules, not as separate languages.

---

## 2. The evaluation harness

This is built **before** any resolution code changes, because it defines the target.

### 2.1 Canonical queries (14 queries × 8 languages = 112 test points)

Each query exercises one consumer-facing tool on a known correct answer. Queries are the same across languages; the reference answers differ.

| # | Query template | Tool exercised | What it measures |
|---|---|---|---|
| Q1 | "Find class/struct/type named X" | `search_codebase` | kind-tagged exact match across adapters |
| Q2 | "Find interface/protocol/trait named X" | `search_codebase` | interface concept translates across languages |
| Q3 | "Find function/method named X" | `search_codebase` | free function vs method distinction |
| Q4 | "What are all callers of function X?" | `get_impact` (reverse edges) | cross-file call-graph completeness |
| Q5 | "What does X call?" | `get_context` (forward edges) | 1-hop call resolution |
| Q6 | "What classes implement interface X?" | `get_impact` on `Implements` | implements/extends parity across langs |
| Q7 | "What's the blast radius of changing X?" | `get_impact` (transitive, depth=3) | recursion stops correctly |
| Q8 | "What symbols live in file F?" | `query_graph` | `Defines`/`Contains` completeness |
| Q9 | "What imports does F have, and where do they resolve?" | `query_graph` on `Imports` | import path resolution to concrete symbols |
| Q10 | "What module is symbol X in?" | `get_symbol` | module/package/namespace concept |
| Q11 | "What field X exists on type Y?" | `query_graph` on `HasField` | class/struct fields across langs |
| Q12 | "What symbols cluster with X?" | `cluster_graph` | community stability under re-run |
| Q13 | "Given PRD text T, which referenced names exist in the graph?" | `validate_prd_against_graph` | end-to-end name validation |
| Q14 | "Given file F, what unresolved external refs does it have?" | `query_graph` on unresolved | accurate accounting of what we don't know |

Not every query applies to every language (e.g. Q11 "field" has no clean analog in Go's embedded interfaces); per-language applicability is declared in the harness config and the denominator adjusts.

### 2.2 Reference codebases (5 corpora, mid-size, open-source, diverse)

Each chosen to exercise a different language's hard cases. Sizes chosen to fit in CI (≤100k LOC each, indexable cold in ≤5 min).

| Corpus | Language(s) | LOC | Why |
|---|---|---|---|
| `ripgrep` (BurntSushi/ripgrep) | Rust | ~50k | trait-heavy; cross-crate deps; regex/memchr |
| `fastapi` (tiangolo/fastapi) | Python | ~55k | decorators; Pydantic meta-programming; dynamic dispatch |
| `vscode-extension-samples` | TS + some JS | ~40k | Angular-adjacent DI patterns; real-world TS with decorators |
| `kotlinx.coroutines` (Kotlin) | Kotlin + Java interop | ~60k | extension fns; suspend fns; JVM interop |
| `kubernetes/kubectl` | Go | ~80k | structural interfaces; large package graph |

Swift and JavaScript are evaluated via smaller sentinel repos (chosen from the GitNexus fixture set) to keep the initial harness bootstrappable. The 5-corpus set is the CI gate; full 8-language coverage is an extended nightly run.

### 2.3 Scoring

Per query, per corpus:

- **Q1–Q3, Q10 (single-symbol queries):** exact-match on fully-qualified name. 1.0 if match, 0.0 if miss.
- **Q4–Q9, Q11, Q14 (set queries):** F1 against a hand-labeled reference set of size ~50 per corpus. Precision = |returned ∩ truth| / |returned|; Recall = |returned ∩ truth| / |truth|; F1 = 2·P·R/(P+R).
- **Q12 (clustering):** adjusted Rand index against a human-labeled community partition, averaged over 3 runs to catch instability.
- **Q13 (PRD validation):** precision on "names flagged as present" + recall on "names truly present"; unweighted mean.
- **Q7 (blast radius):** recall at depth=3 against a golden transitive closure.

**Weighted formula for the 85%:**

```
end_result_score = Σ_q w_q · score_q
```

where `w_q` reflects how often the query appears in real agent traces. Initial weights, to be recalibrated from telemetry after one month:

| Query group | Weight | Rationale |
|---|---|---|
| Q1–Q3 (symbol lookup) | 0.25 | most common agent action; every session |
| Q4–Q5 (direct call graph) | 0.20 | `get_context`/`get_impact` invocations |
| Q7 (transitive impact) | 0.15 | refactoring decisions |
| Q6 (interface implementors) | 0.10 | architectural queries |
| Q9 (imports) | 0.10 | dep-tracing |
| Q13 (PRD validation) | 0.10 | pipeline-critical |
| Q8, Q10, Q11, Q14 | 0.05 (sum) | lower frequency |
| Q12 (clustering) | 0.05 | periodic, not per-query |

The 85% target is on the per-language `end_result_score`, averaged across corpora, with **no single language below 0.75** (so that "average" cannot hide a broken Swift adapter behind strong Rust numbers).

### 2.4 Ground-truth acquisition

- **LSP-oracle tier.** For each corpus, run the language's LSP server and capture `textDocument/definition` / `textDocument/references` answers for every query point. This is the cheap ground truth. Does not apply to Q12 (clustering) or Q7 (transitive blast radius), which need hand labels.
- **Hand-labeled golden tier.** ~50 per corpus × 5 corpora × 14 queries = up to 3500 labels. In practice 200–300 per corpus is enough. One-time labeling effort amortized over every CI run.
- **LSP is not used to score LSP.** When Layer 1 (LSP-first resolution) is on, the oracle falls back to hand labels to avoid circularity.

---

## 3. Language-neutral schema

The current schema (Function, Method, Struct, Enum, Variant, Trait, Field, Constant, TypeAlias, Import, CallSite, Module, File, Directory, Community, Process) is Rust-flavored. Rename and generalize.

### 3.1 Label decisions

| Prior label | Decision | New label | `kind` property values |
|---|---|---|---|
| `Struct` | **rename** to `Class` with `kind` | `Class` | `struct` (Rust/Swift/Go), `class` (Java/Kotlin/TS/Python/Swift), `record` (Java/Kotlin), `dataclass` (Python), `object` (Kotlin singleton), `actor` (Swift) |
| `Trait` | **rename** to `Interface` with `kind` | `Interface` | `trait` (Rust), `protocol` (Swift), `interface` (Java/Kotlin/TS/Go), `abc` (Python ABC) |
| `Enum` | keep | `Enum` | `enum` (all), `sealed_class` (Kotlin/Java) — sealed hierarchies are enum-shaped |
| `Variant` | keep | `Variant` | — (for Rust/Swift enum cases, Kotlin sealed subtypes) |
| `Function` | keep | `Function` | `free`, `closure`, `lambda`, `arrow` |
| `Method` | keep | `Method` | `instance`, `static`, `class_method`, `extension`, `operator` |
| `Field` | keep | `Field` | `stored`, `computed`, `constant`, `property` |
| `Constant` | keep | `Constant` | — |
| `TypeAlias` | keep | `TypeAlias` | `alias`, `typedef`, `newtype` |
| `Import` | keep | `Import` | — |
| `CallSite` | keep | `CallSite` | — |
| `Module` | keep | `Module` | `module`, `package`, `namespace` (for TS/C# flavor) |
| `File`, `Directory`, `Community`, `Process` | keep | — | structural, language-neutral already |

Additions:

- **`Package`**: distinct from `Module` for languages where the two differ (Java package vs. module, Go module vs. package). When indistinguishable in a language, the adapter sets both to the same node or uses `Module` only.
- **`Annotation`**: decorator/attribute/annotation — `@Deprecated` (Java/Kotlin), `@Component` (Angular), `@dataclass` (Python), `#[derive(...)]` (Rust). Separate node because these drive resolution (Angular DI, Spring wiring, Rust derives).
- **`GenericParameter`**: type parameter `T`, `<T: Bound>`, useful for Q7 (blast radius must follow type parameters).

Rationale for rename over parallel-label: parallel labels (`Trait` + `Interface` both exist) doubles the queryable label set and every agent has to know which one applies to which language. Renaming to the semantic-concept label is one-time cost for permanent simplicity. Migration path preserves old labels as aliases (queries by `:Trait` still resolve) through one release cycle, then drops them.

### 3.2 Edge label decisions

Existing: `Contains`, `Defines`, `HasMethod`, `HasField`, `HasVariant`, `Calls`, `Imports`, `Implements`, `Extends`, `Uses`.

Extensions:

- **`HasProperty`**: for TS/Swift computed properties distinct from fields (optional — adapter may fold into `HasField` with `kind=computed`).
- **`AnnotatedWith`**: from any node to an `Annotation`.
- **`Overrides`**: method-override relationship (Java/Kotlin/TS); currently collapsed into `Implements` which is imprecise.
- **`Receives`**: for Kotlin extension functions and Swift protocol extensions — the extension method `Receives` its receiver type.

`Calls` edges gain a property `dispatch`: one of `static`, `virtual`, `interface`, `dynamic`, `unknown`. Needed for Q7 blast-radius precision (interface-dispatch callers are different from static ones).

### 3.3 Node identity

Symbol identity across languages uses an SCIP-style symbol string:

```
scheme   package-manager+pkg-name  version   namespace/type#member().
  e.g.   cargo+serde                1.0.197   serde/Deserialize#deserialize().
  e.g.   npm+react                  18.2.0    react/Component#render().
  e.g.   maven+org.junit            5.10.0    org/junit/jupiter/api/Test#.
```

This is load-bearing: cross-language edges (e.g. a Kotlin class implementing a Java interface, a TS file importing a JS module) need symbol identity that survives adapter boundaries. SCIP specifies this exact format (Sourcegraph, 2022). LSIF used URI strings which do not carry package identity — SCIP is the correct choice for a polyglot indexer.

---

## 4. Language adapter trait

### 4.1 The trait

```rust
pub trait LanguageAdapter: Send + Sync {
    fn language_id(&self) -> Language;
    fn detect(&self, path: &Path) -> bool;

    // Parsing + symbol extraction
    fn parse(&self, source: &str, path: &Path) -> Result<ParseResult, String>;

    // Label mapping — how this language's concepts map to the neutral schema
    fn label_map(&self) -> &LabelMap;

    // Import resolution — module path → source file(s) on disk
    fn resolve_import(&self, graph: &GraphStore, import: &Import, ctx: &ResolveContext)
        -> Result<Option<String>, String>;

    // Method resolution — receiver type + method name → concrete Method node
    fn resolve_method_call(&self, graph: &GraphStore, call: &CallSite, ctx: &ResolveContext)
        -> Result<Option<Resolution>, String>;

    // Language-specific expansions (macros, decorators, compiler intrinsics)
    fn expansion_table(&self) -> &[ExpansionRule];

    // Curated stdlib/standard-library index
    fn stdlib_index(&self) -> &StdlibIndex;

    // Dependency manifest discovery + parsing
    fn dependency_manifest(&self, root: &Path) -> Option<DependencyManifest>;

    // Resolve a DependencyManifest entry to its source on disk
    fn dependency_source_path(&self, dep: &DependencyEntry) -> Option<PathBuf>;

    // LSP integration point — which server binary to probe
    fn lsp_server(&self) -> Option<LspServerSpec>;
}
```

The trait is concrete and small. Functions that are universal across languages (building the symbol index, edge deduplication, clustering, community detection) live in a `core` module that consumes `&dyn LanguageAdapter` — no per-language code in the core.

### 4.2 Per-language specialization

For each of the 8 languages, the one thing that is genuinely unique (everything else fits the trait):

| Language | The unique thing | Handled via |
|---|---|---|
| Rust | Macro system (`macro_rules!`, proc-macros) | `expansion_table()` + LSP delegation for proc-macros |
| Swift | Protocol extensions + conditional conformance | `Receives` edges + `resolve_method_call` narrowing |
| Kotlin | Extension functions (receiver is not where method is defined) | `Receives` edges; scope table carries receiver context |
| Java | Package-private visibility + class-file based resolution | visibility-aware symbol index; `.class` introspection in future |
| Python | Runtime method dispatch; MRO; metaclasses | best-effort via LSP (pyright narrows); static path degrades gracefully |
| TypeScript | Structural typing; decorators; path-mappings in tsconfig.json | tsconfig path resolver; decorator table; structural-match fallback |
| JavaScript | Prototype chain; dynamic dispatch | limited static resolution; LSP-first strongly recommended |
| Go | Structural interfaces (implicit implementation); method sets | implicit `Implements` synthesis from method-set subset check |

Angular/React overlays: the TS adapter's `expansion_table()` includes decorator expansions (`@Component`, `@Injectable`, `@NgModule` → DI provider edges; `useState`/`useEffect` hooks → React-specific `Calls` edges with `dispatch=dynamic`).

### 4.3 Files that exist today vs files that change

Today: `src/parser/{rust.rs, python.rs, typescript.rs}` and `src/resolver.rs` with Rust-centric logic. The v2 layout:

```
src/
  core/              # language-neutral
    schema.rs        # neutral labels + kind vocab
    symbol_index.rs
    graph_ops.rs     # shared edge-buffer, dedup, cluster
    resolve_pipeline.rs  # orchestrates layers 1–6 over adapters
  adapter/
    mod.rs           # trait definition
    rust.rs
    swift.rs
    kotlin.rs
    java.rs
    python.rs
    typescript.rs
    javascript.rs
    go.rs
    framework/
      angular.rs     # overlay on typescript
      react.rs       # overlay on typescript
  harness/           # the evaluation harness from §2
```

Migration is incremental: rust.rs moves first, keeps working, then python/typescript move, then new adapters land.

---

## 5. Universal resolution layers vs per-language layers

The prior six-layer plan revisited with universal-vs-specific tagging. Each layer is a **strategy** — universal in shape — whose **data** is per-language.

| # | Layer | Shape | Data source |
|---|---|---|---|
| 1 | LSP-first | Universal strategy | Per-language LSP server (`lsp_server()` per adapter) |
| 2 | Dependency indexing | Universal strategy | `dependency_manifest()` + `dependency_source_path()` per adapter |
| 3 | Scope-table method resolution | Universal strategy | Adapter's parse pass emits bindings; core walks them |
| 4 | Macro/decorator/intrinsic expansion | Universal strategy | `expansion_table()` per adapter (empty for languages without) |
| 5 | Stdlib index | Universal strategy | `stdlib_index()` per adapter (curated per ecosystem) |
| 6 | Fuzzy name-match fallback | Universal strategy | Uses the neutral symbol index; no per-language data |

Key reframing vs prior plan:

- **Layer 4 is not Rust-only.** Every language has an equivalent: TS decorators (`@Component` → wires DI), Java annotations (`@Inject`), Python decorators (`@app.route` in Flask, `@dataclass`), Angular's `@Component` metadata, React hooks (pattern-matched as implicit calls), Swift property wrappers. The universal framing is "name-triggered expansion table" and each adapter contributes entries.
- **Layer 5 is genuinely per-language data, universal in shape.** Rust std, Python stdlib, Java JDK, Kotlin stdlib, Go standard library, Swift stdlib, JS builtins, TS lib.*.d.ts. The table format is identical; the entries are per-language.
- **Layer 3 is more valuable for some languages than others.** Go and Java have heavy explicit typing → scope tables resolve a lot. Python and JS have weak explicit typing → scope tables catch only the annotated subset. Swift and Kotlin sit in between.
- **Empirical calibration (2026-04-17, rust-self corpus)**: a static-only Layer 3 implementation (typed function params + typed let bindings + `self` resolution, no type inference from RHS) was built and measured. Result: `end_result_score` moved from **0.845 → 0.840** — a net regression of −0.005. Root cause: idiomatic Rust uses `let x = foo()` (no type annotation) constantly; method CallSites now emitted for these can't resolve their receiver type, and the unresolved noise slightly pollutes `get_context`'s `calls`/`called_by` sets. Conclusion: **on Rust with modern idioms, static Layer 3 without upstream type inference is not worth shipping**. The architect's +8 pp prediction held only under the assumption of heavy explicit typing (which Rust programmers optimize away via inference). For Rust, Layer 1 (LSP) is not optional — rust-analyzer's type inference is what Layer 3 would need to lean on. For Go/Java/typed TS, static Layer 3 should still pay off; reassess when those adapters ship.
- **Layer 1 is the great equalizer.** LSP shipped for every language in scope.

Contribution estimates are per-language (prior plan's single-codebase percentages no longer apply):

| Language | LSP-only (L1) | Static stack (L2–L5) | Combined projection |
|---|---|---|---|
| Rust | ~80% | ~80% (already reached in v1 trajectory) | ~88% |
| Java | ~85% | ~70% | ~88% |
| Kotlin | ~80% | ~65% | ~85% |
| Go | ~85% | ~75% | ~88% |
| Swift | ~75% | ~60% | ~82% |
| TypeScript | ~82% | ~70% | ~87% |
| JavaScript | ~70% | ~45% | ~75% — **the weak link** |
| Python | ~78% | ~55% | ~80% |

Projections are based on: LSP quality (rust-analyzer, jdtls, gopls are mature; kotlin-language-server is mid-maturity; pyright is mature for typed Python), and static-analysis tractability (strong static typing → high static path). The 85% per-language floor requires LSP for the dynamic languages; static-only on JS will not reach 85%.

---

## 6. Dependency indexing across languages

### 6.1 Uniform manifest

```rust
pub struct DependencyManifest {
    pub ecosystem: Ecosystem,   // Cargo, Npm, Maven, Gradle, Go, Pip, Spm, ...
    pub direct: Vec<DependencyEntry>,
    pub lockfile_hash: [u8; 32], // for cache invalidation
}

pub struct DependencyEntry {
    pub name: String,
    pub version: String,
    pub source_kind: SourceKind, // Registry, Git, Local, System
    pub scip_symbol_prefix: String, // e.g. "cargo+serde 1.0.197"
}
```

### 6.2 Per-ecosystem manifest discovery

| Ecosystem | Manifest files (priority order) | Source location of installed deps |
|---|---|---|
| Cargo (Rust) | `Cargo.lock`, resolved via `cargo metadata --format-version 1` | `~/.cargo/registry/src/index.crates.io-*/<name>-<ver>/src/` |
| npm/pnpm/yarn (TS/JS) | `package-lock.json` or `pnpm-lock.yaml` or `yarn.lock`; + `tsconfig.json` paths | `node_modules/<name>/` — prefer `*.d.ts` over `*.js` for resolution |
| Maven (Java) | `pom.xml` | `~/.m2/repository/...` |
| Gradle (Java/Kotlin) | `build.gradle[.kts]` + generated `dependencies.lock` | `~/.gradle/caches/modules-2/files-2.1/...` |
| Go | `go.mod` + `go.sum` | `$GOPATH/pkg/mod/<mod>@<ver>/` |
| pip/uv/poetry (Python) | `pyproject.toml`, `poetry.lock`, `requirements.txt`, `uv.lock` | `site-packages/<pkg>/` in the active venv |
| SwiftPM | `Package.swift` + `Package.resolved` | `.build/checkouts/` (workspace) or `~/Library/Developer/Xcode/DerivedData/.../SourcePackages/checkouts/` |

The core resolver is ignorant of which source a node came from; it only uses the `source = "dep"` flag (already present in 3b-v1) and the `scip_symbol_prefix` for identity. Angular/React have no separate manifest — they are npm deps handled by the TS adapter.

### 6.3 Depth policy

Depth=2 (direct + one hop) applies uniformly. Justification unchanged from prior plan: most real call chains stop at a dep's public surface. Per-ecosystem exceptions:

- **Go:** depth=1 is often enough because Go discourages transitive API leakage. Allow override.
- **npm:** depth=2 can explode (a single `webpack` dep pulls hundreds). Prefer `.d.ts`-only indexing to keep byte budget bounded.
- **Gradle multi-project:** in-repo sub-projects are always depth=∞; external deps get depth=2.

### 6.4 Cache

Unified cache at `~/.cache/ai-architect-mcp/dep-graphs/<ecosystem>/<hash>/`. Key is SHA-256 of the relevant lockfile entries. LRU cap 5 GB, configurable. One cache; all adapters write into it.

---

## 7. LSP-first with static fallback — the decision

**Decision: LSP-first, with static fallback as a first-class path, not a bolted-on afterthought.** Not "static-first with LSP enhancement".

### 7.1 Why LSP-first

Every language in scope has a mature LSP. The prior plan's caveat ("rust-analyzer may not be installed") generalizes in both directions: some machines have every LSP, some have none. A CI runner configured for a given project will have the right LSP; a researcher's laptop typically has 2-3. LSP-first with graceful degradation dominates static-first because LSP captures inferred types, generics, trait solving, structural subtyping (Go, TS), MRO (Python), and method sets (Go) — all of which the static path can only approximate.

### 7.2 Graceful degradation ladder

When resolving a reference:

1. **Try LSP.** Adapter reports whether `lsp_server()` is on `PATH`. If yes, issue `textDocument/definition` / `textDocument/typeDefinition`. Success → high-confidence edge with `resolution_method = "lsp-<server>"`.
2. **LSP unavailable or returned no result.** Fall to static stack in order: stdlib index (L5) → dep-graph lookup (L2) → scope-table (L3) → expansion table (L4) → fuzzy match (L6, optional).
3. **All layers fail.** Record `UnresolvedRef` with reason classifying the gap (`"no-lsp-and-no-static-match"`, `"lsp-says-no-definition"`, `"dep-not-indexed"`).

### 7.3 Why not static-first

Static-first treats LSP as an optional enhancement. That framing tolerates a persistently lower ceiling on dynamic languages (JS/Python hit ~45–55% static vs ~70–78% LSP). A "static-first" system that hits 55% on Python would fail the per-language floor, and the whole point of the rewrite is to not fail on any language.

### 7.4 The failure mode to watch

LSP answers can be **wrong** under certain conditions (rust-analyzer in workspaces with build scripts that haven't run; typescript-language-server when tsconfig paths are misconfigured; pyright when `typings` aren't installed). The end-result harness catches this because it scores against ground truth, not against LSP. If an LSP-corrupted answer lowers the score, the harness tells us; we do not pretend LSP is always right.

---

## 8. Migration plan from current Rust-heavy architecture

Ten steps. Each leaves the system green. No big-bang rewrite.

| Step | Scope | Risk | Blast radius |
|---|---|---|---|
| 1 | Build the harness from §2 — queries + corpora + scoring. Ship empty (everything scores 0/F). This is the measurement scaffold. | low | isolated new directory |
| 2 | Rename `Struct` → `Class`, `Trait` → `Interface` in the schema. Add `kind` column. Keep old labels as aliases. Run harness — score unchanged. | medium | every Cypher query in the codebase |
| 3 | Introduce `LanguageAdapter` trait. Refactor existing Rust/Python/TS parsers into adapters. No new languages yet. Harness score unchanged. | medium | all parser code |
| 4 | Move universal resolution layers (edge buffer, symbol index, expansion driver) into `core`. Adapters supply data. Score unchanged. | medium | resolver.rs |
| 5 | Add LSP-first path behind auto-detect. Runs for whatever languages have LSP on PATH. Harness picks up gains on Rust/Python/TS. | medium-high | lsp client, resolver dispatch |
| 6 | Add Java adapter. Ship harness scores on Java corpus. | medium | adapter/java.rs |
| 7 | Add Kotlin adapter. JVM-interop edges between Java and Kotlin nodes unlock cross-language `get_impact` answers. | medium | adapter/kotlin.rs |
| 8 | Add Go adapter + implicit-interface synthesis. | medium | adapter/go.rs |
| 9 | Add Swift adapter. | medium | adapter/swift.rs |
| 10 | Add JavaScript adapter + Angular/React framework overlays to TS. | medium-high | adapter/javascript.rs, adapter/framework/ |

Each step runs the harness and commits the scoreboard. Any step that regresses the score on any corpus rolls back before merge.

Rollback points: every step is reversible via `git revert`. The adapter trait (step 3) is the riskiest because it touches every language module at once — its own PR, own review.

---

## 9. Invariants the upgrade preserves (expanded from 3b-v1)

1. Unresolved references are never deleted. The absence of an edge is data.
2. Every edge has a confidence and a `resolution_method`; no silent high-confidence edges.
3. Layers are additive; running or not running a layer does not change the output of other layers.
4. Project/dep/stdlib partition is preserved — `source` property on every synthetic node.
5. Idempotency — re-running `resolve_graph` produces the same edge set.
6. The neutral schema is canonical; language-specific labels are aliases or `kind` values, never the primary identity.
7. Performance budget is a CI test. Cold `analyze_codebase` ≤ 5 min per corpus; warm ≤ 60 s.
8. The harness score is a CI gate. No language drops below 0.75; the average stays ≥ 0.85.

---

## 10. Open questions

1. **Clojure / C# / Ruby / C++ — in or out?** GitNexus supports a wider set. We scope this spec to 8. The adapter trait is designed so these can be added later without core changes. Confirm 8 is the right initial scope for this project or if C++ (heavy deps, templates) belongs in v2.
2. **Angular/React/Vue — are these adapters, overlays, or something else?** This spec calls them overlays on the TS adapter. Vue intentionally deferred. Svelte/Solid — out of initial scope. Confirm.
3. **The JS floor (~75%) is below the 85% target.** Options: (a) accept that JS without types is below floor and document it; (b) require type-definitions-present in the JS corpus; (c) invest in ts-flow inference-from-JSDoc. Recommend (b) as the operational compromise.
4. **Java bytecode / .class file indexing** — worth it? Fetches richer type information than source alone. Cost: new dep on asm or jvm-bytecode crate. Defer to v3.
5. **SCIP emit.** The SCIP symbol format is already in the schema. Should `analyze_codebase` also emit a SCIP index file (Sourcegraph-compatible)? One-way valve to join the broader tooling ecosystem. Cheap given the symbol format is already SCIP-shaped.
6. **Harness labeling effort.** 200-300 labels per corpus × 5 corpora is ~1500 labels. Not small. Recommend spawning a one-week focused labeling sub-project before step 5 (LSP), so the harness has ground truth before LSP-vs-static comparison is meaningful.
7. **Precision floor.** 3b-v1 noted recall ≥85% and precision ≥0.95. The polyglot harness uses F1 on set queries, which balances both. Confirm F1 is the right balance point or if we should gate separately.
8. **Dependency indexing of system SDKs** (JDK, Swift stdlib shipped with Xcode, Python stdlib at `/usr/lib/python*`): treat these as Layer 5 (curated stdlib index) data rather than Layer 2 (dep graph)? The spec assumes yes — system SDK goes through `stdlib_index()`, not `dependency_manifest()`.

---

## 11. Summary

| Concern | v1 (this document supersedes) | v2 (this document) |
|---|---|---|
| Target | 85% Rust resolution rate | 85% end-result harness score across 8 languages |
| Metric | % of refs → edges | weighted sum of 14 consumer-tool queries, ground-truth scored |
| Scope | Rust + two partial parsers | 8 first-class languages + 2 TS framework overlays |
| Schema | Rust-flavored labels | neutral labels with `kind` property; SCIP symbol identity |
| Architecture | per-language parsers feeding Rust-biased resolver | `LanguageAdapter` trait over language-neutral core |
| Layers | 6 Rust-specific | 6 universal strategies, per-language data |
| LSP | layer 1 of 6, auto-detect | the primary path with graceful fallback |
| Measurement | resolution-rate benchmark on one repo | harness with 5 corpora, per-query scoring, per-language floor |
| Migration | add layers incrementally | adapter refactor + add languages incrementally, both behind the same harness gate |

The two load-bearing user corrections are what drove this rewrite: we measure the end result (the agent's answer), not the resolver's internal accounting; and the analyzer is polyglot by design, not Rust with attachments. Everything else (the six layers, the performance budget, the edge-buffer mechanism, the dep-graph cache, the invariants) is preserved from v1 because those decisions were sound — they just needed to be re-framed around an 8-language world and re-measured at the right point.
