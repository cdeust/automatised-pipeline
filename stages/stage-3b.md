# Stage 3b — Resolution (the graph connects)

**Status:** spec (pre-implementation). Depends on 3a (complete). Adds cross-file semantic edges to the graph built by 3a.

---

## 1. Purpose

Stage 3b converts unresolved string references into typed graph edges pointing to concrete target nodes. After 3b, the graph supports cross-file queries: "what does this module import?", "who calls this function?", "what implements this trait?".

**The quantity being defined:** resolution rate = (resolved refs) / (total refs). This is the operational measure of 3b's completeness. It is bounded above by the tractability ceiling (what can be resolved without a type checker). The gap between resolution rate and 1.0 is the residual that requires LSP integration (deferred to 3b-v2).

---

## 2. Resolution edge types

3b adds edges to the 3a graph. No new node labels. Five new edge kinds, each with a defined resolution algorithm.

| Edge type | From label(s) | To label(s) | Resolution algorithm | Tractable without type checker? |
|---|---|---|---|---|
| `Imports` | File, Module | File, Module, Function, Struct, Enum, Trait, Constant, TypeAlias | Match `Import.path` property against `qualified_name` of target nodes in the graph. For `crate::` prefix, strip and search. For `self::` / `super::`, resolve relative to the importing file's module path. For glob imports (`is_glob = true`), create one `Imports` edge per public symbol in the target module. | Yes — string match against graph index |
| `Calls` | Function, Method | Function, Method | Parse function bodies for `call_expression` nodes (tree-sitter). Extract callee path. If fully qualified (`foo::bar::baz()`), resolve directly against `qualified_name`. If unqualified (`baz()`), resolve via the file's import graph (follow `Imports` edges to find `baz` in scope). | Partially — fully-qualified and import-resolved calls: yes. Method calls on inferred types: no. |
| `Implements` | Struct, Enum | Trait | Parser already extracts impl blocks with `trait_name` property on Method nodes. Collect all methods where `trait_name` is non-empty. Group by (receiver type, trait name). Match `trait_name` against `Trait.name` or `Trait.qualified_name` in the graph. Create edge from the impl'd type to the trait. | Yes — the parser already has the data; resolution is name matching |
| `Extends` | Trait | Trait | Parse trait declarations for supertraits. In tree-sitter-rust, `trait Foo: Bar + Baz` has a `type_bound_list` child. Extract each bound name. Match against `Trait.qualified_name` in the graph. | Yes — AST extraction + name matching |
| `Uses` | Function, Method, Struct, Field | Struct, Enum, Trait, TypeAlias | Extract type annotations from function parameters, return types, field types, and type alias targets (already stored as `type_annotation` / `target_type` properties). Parse the type text for identifiers. Match each identifier against symbol names in the graph via the import scope. | Partially — simple type names: yes. Generic parameters, associated types, trait objects: no. |

### Edge properties

Every resolution edge carries:

```
confidence: f32      // 0.0-1.0 — source: stage-3.md §6.3
resolution_method: String  // one of: "qualified-name-match", "import-scope-lookup",
                           //         "trait-name-match", "supertrait-parse",
                           //         "type-annotation-parse"
```

**Confidence assignment rules:**
- `1.0` — fully qualified path match, unique target in graph
- `0.9` — import-scope-resolved, unique target after import chain
- `0.7` — name match with ambiguity (multiple candidates; picked the one in the same crate)
- `0.5` — heuristic match (name match across crates, no disambiguating information)

These thresholds are operational, not arbitrary: 1.0 means the resolution is provably correct given the graph; lower values encode the probability of shadowing or re-export confusion. Source: no published paper; these are project-specific operational thresholds. They can be calibrated by running resolution on our own codebase and checking the resolved edges manually.

---

## 3. New relationship tables (graph_store.rs additions)

3b adds these entries to `REL_TABLES`:

```
// Imports — source: stage-3.md §5.2, stage-3b.md §2
("Imports_File_File",       NODE_FILE,       NODE_FILE),
("Imports_File_Module",     NODE_FILE,       NODE_MODULE),
("Imports_File_Function",   NODE_FILE,       NODE_FUNCTION),
("Imports_File_Struct",     NODE_FILE,       NODE_STRUCT),
("Imports_File_Enum",       NODE_FILE,       NODE_ENUM),
("Imports_File_Trait",      NODE_FILE,       NODE_TRAIT),
("Imports_File_Constant",   NODE_FILE,       NODE_CONSTANT),
("Imports_File_TypeAlias",  NODE_FILE,       NODE_TYPE_ALIAS),
("Imports_Module_Function", NODE_MODULE,     NODE_FUNCTION),
("Imports_Module_Struct",   NODE_MODULE,     NODE_STRUCT),
("Imports_Module_Enum",     NODE_MODULE,     NODE_ENUM),
("Imports_Module_Trait",    NODE_MODULE,     NODE_TRAIT),
("Imports_Module_Constant", NODE_MODULE,     NODE_CONSTANT),
("Imports_Module_TypeAlias",NODE_MODULE,     NODE_TYPE_ALIAS),

// Calls — source: stage-3.md §5.2, stage-3b.md §2
("Calls_Function_Function", NODE_FUNCTION,   NODE_FUNCTION),
("Calls_Function_Method",   NODE_FUNCTION,   NODE_METHOD),
("Calls_Method_Function",   NODE_METHOD,     NODE_FUNCTION),
("Calls_Method_Method",     NODE_METHOD,     NODE_METHOD),

// Implements — source: stage-3.md §5.2, stage-3b.md §2
("Implements_Struct_Trait",  NODE_STRUCT,     NODE_TRAIT),
("Implements_Enum_Trait",    NODE_ENUM,       NODE_TRAIT),

// Extends — source: stage-3b.md §2
("Extends_Trait_Trait",      NODE_TRAIT,      NODE_TRAIT),

// Uses — source: stage-3.md §5.2, stage-3b.md §2
("Uses_Function_Struct",    NODE_FUNCTION,   NODE_STRUCT),
("Uses_Function_Enum",      NODE_FUNCTION,   NODE_ENUM),
("Uses_Function_Trait",     NODE_FUNCTION,   NODE_TRAIT),
("Uses_Function_TypeAlias", NODE_FUNCTION,   NODE_TYPE_ALIAS),
("Uses_Method_Struct",      NODE_METHOD,     NODE_STRUCT),
("Uses_Method_Enum",        NODE_METHOD,     NODE_ENUM),
("Uses_Method_Trait",       NODE_METHOD,     NODE_TRAIT),
("Uses_Method_TypeAlias",   NODE_METHOD,     NODE_TYPE_ALIAS),
("Uses_Struct_Struct",      NODE_STRUCT,     NODE_STRUCT),
("Uses_Struct_Enum",        NODE_STRUCT,     NODE_ENUM),
("Uses_Struct_Trait",       NODE_STRUCT,     NODE_TRAIT),
("Uses_Field_Struct",       NODE_FIELD,      NODE_STRUCT),
("Uses_Field_Enum",         NODE_FIELD,      NODE_ENUM),
("Uses_Field_Trait",        NODE_FIELD,      NODE_TRAIT),
```

All tables use `confidence DOUBLE, resolution_method STRING` as edge properties.

---

## 4. Resolution pipeline shape

### 4.1 When it runs

Resolution is a **post-parse pass**. It runs AFTER `index_codebase` (3a) completes. It modifies the existing graph **in-place** by adding edges. It does not produce a separate artifact.

### 4.2 Idempotency

`resolve_graph` is idempotent. Running it twice on the same graph produces the same edges. Implementation: before inserting a resolution edge, check if it already exists (MATCH query). If yes, skip. This is O(E) in the number of resolved edges per run, acceptable for v1.

### 4.3 MCP tool

One new tool: `resolve_graph`. Schema:

```json
{
  "type": "object",
  "required": ["graph_path"],
  "additionalProperties": false,
  "properties": {
    "graph_path": { "type": "string", "description": "Path to the stage-3.graph/ directory." }
  }
}
```

Output: `{ status: "ok", imports_resolved, calls_resolved, implements_resolved, extends_resolved, uses_resolved, total_refs, resolution_rate, elapsed_ms }`.

### 4.4 Integration with index_codebase

`index_codebase` does NOT automatically run resolution. The agent must call `resolve_graph` separately. Rationale: the agent may want to query the unresolved graph first (3a tools work without resolution), and resolution is expensive enough to warrant explicit invocation.

### 4.5 Metadata update

After `resolve_graph` completes:
- `stage-3.index.json` field `phases_completed` gains `"resolve"`.
- `stage-3.index.json` field `resolution_stats` is populated: `{ total_refs, resolved_refs, resolution_rate }`.

---

## 5. Resolution algorithms — detail

### 5.1 Import resolution

**Input:** All `Import` nodes in the graph (extracted by 3a parser from `use` declarations).

**Algorithm:**

1. For each `Import` node, read its `path` property (e.g., `crate::graph_store::GraphStore`).
2. Normalize the path:
   - Strip `crate::` prefix. The remainder is a module-relative path.
   - For `self::X` — resolve relative to the current file's module.
   - For `super::X` — resolve relative to the parent module.
   - For external crates (`std::`, `serde::`, etc.) — no target in the graph. Mark as `is_resolved = false`, skip edge creation. These are out-of-codebase references.
3. Split the normalized path on `::`. Walk the graph from module roots: match the first segment against `Module.name`, then the next segment against symbols defined by that module, etc.
4. If a unique target is found: create an `Imports` edge from the file to the target. Set `confidence = 1.0`. Set `is_resolved = true` on the `Import` node.
5. If multiple candidates: pick the one in the same crate (same file tree). Set `confidence = 0.7`.
6. If no target found: leave `is_resolved = false`. Do not create an edge.

**Glob imports (`use foo::*`):**
- Find the target module.
- Query all symbols with a `Defines` edge from that module where `visibility` starts with `pub`.
- Create one `Imports` edge per public symbol.

### 5.2 Call resolution

**Input:** `CallSite` nodes (if extracted by 3a) OR call expressions extracted during a second tree-sitter pass over function bodies.

**Current state of 3a parser:** The parser does NOT extract `CallSite` nodes yet (the `NODE_CALL_SITE` constant exists in graph_store.rs but no extraction code exists in rust_parser.rs). **3b must add call-site extraction to the parser.**

**Algorithm:**

1. For each `.rs` file, re-parse with tree-sitter. Walk all `call_expression` nodes inside function/method bodies.
2. Extract the callee text (the expression before `()`).
3. Classification:
   - **Fully qualified:** `foo::bar::baz()` — resolve `foo::bar::baz` against `qualified_name` in the graph.
   - **Imported name:** `baz()` — look up `baz` in the file's resolved import set (the `Imports` edges from 5.1). If found, the target is the import's target.
   - **Method call:** `x.baz()` — requires knowing the type of `x`. **Out of scope for 3b-v1.** Skip. Set `is_resolved = false`.
   - **Self call:** `self.baz()` or `Self::baz()` — resolve against methods on the current impl's type.
4. Create `Calls` edge from the containing function/method to the resolved target. Confidence per the rules in section 2.

**Dependency:** Call resolution depends on import resolution (step 5.1 must run first). The resolution pass is ordered: imports first, then calls.

### 5.3 Impl-trait binding

**Input:** Method nodes with `trait_name` property (already extracted by 3a parser in `extract_impl_method`).

**Algorithm:**

1. Query all `Method` nodes where `trait_name` is non-empty.
2. Group by `(receiver_type, trait_name)`.
3. For each group: resolve `trait_name` against `Trait.name` and `Trait.qualified_name` in the graph.
4. Resolve `receiver_type` (the qualified name of the impl'd type, stored in the method's `receiver_type` property) against `Struct.qualified_name` or `Enum.qualified_name`.
5. If both resolve: create `Implements` edge from the struct/enum to the trait. Confidence = 1.0 (the parser is definitive here).

### 5.4 Supertrait resolution (Extends)

**Input:** Trait nodes. Requires a second tree-sitter pass to extract supertrait bounds.

**Algorithm:**

1. For each `Trait` node, re-parse its source span to find `type_bound_list` children.
2. Each bound name is a supertrait candidate.
3. Resolve each name against `Trait.name` / `Trait.qualified_name` in the graph (using import scope).
4. Create `Extends` edge from the sub-trait to each resolved supertrait.

**Parser change:** Add supertrait extraction to `extract_trait` in rust_parser.rs. Store as a new property `supertraits: Vec<String>` (comma-separated names) on the `Trait` node, or as `ExtractedRef` entries with `kind = "Extends"`.

### 5.5 Type-usage resolution (Uses)

**Input:** Type annotation strings already stored on nodes: `Field.type_annotation`, `Function` parameters (not yet extracted — deferred), `TypeAlias.target_type`.

**Algorithm:**

1. For each node with a type annotation property, tokenize the type string.
2. Extract identifiers (skip primitives: `i32`, `u8`, `bool`, `String`, `str`, `Vec`, `Option`, `Result`, `Box`, `Arc`, `Rc`, `HashMap`, `HashSet`, `BTreeMap`).
3. For each non-primitive identifier, resolve via import scope against symbols in the graph.
4. Create `Uses` edge from the containing symbol to the resolved type.

**Note:** This is the lowest-confidence resolution. Type annotation text includes generics (`Vec<Foo>`), references (`&Foo`), lifetimes (`'a`), and nested types. The tokenizer must strip these syntactic wrappers to find the nominal type names. Complex cases (associated types, impl Trait, dyn Trait) are skipped.

---

## 6. What 3b must never lose

1. **Unresolved references.** An `Import` or `CallSite` node that fails resolution retains `is_resolved = false`. It is never deleted. The absence of an edge is data.
2. **Resolution confidence.** Every edge carries its confidence score. Downstream tools filter by confidence; the resolver never discards low-confidence matches silently.
3. **Resolution method provenance.** Every edge records how it was resolved. This is auditable.
4. **3a's existing graph.** Resolution only ADDS edges. It never modifies or removes nodes or edges created by 3a.
5. **The `phases_completed` update.** After resolution, `"resolve"` must appear in the array. Stage 4 checks this.

---

## 7. What is explicitly out of scope for 3b-v1

Deferred to 3b-v2 (LSP integration):

- **Method calls on inferred types:** `let x = get_thing(); x.do_it();` — requires type inference.
- **Generic resolution:** `fn foo<T: Trait>(t: T) { t.bar() }` — `bar` is on `Trait`, but which impl?
- **Macro expansion:** `println!`, `vec!`, `derive` macros — tree-sitter sees the invocation, not the expansion.
- **Closure/lambda type inference.**
- **Trait object dispatch:** `dyn Trait` — concrete type is runtime-determined.
- **Associated types:** `<T as Trait>::Output` — requires full trait solving.
- **Re-exports:** `pub use foo::Bar` — the re-export chain requires transitive resolution.
- **External crate resolution:** `use serde::Serialize` — the target is outside the indexed graph.

### 7.1 Resolution-rate calibration (not a bug, a ceiling)

Measured on the pipeline's own Rust codebase (~12K LOC, 3189 graph nodes, 1277 edges via `analyze_codebase`) on 2026-04-17: **~46% resolution rate**. The remaining ~54% is the classes enumerated above — std-lib calls (we intentionally stop at the project root), macro-expanded code, `dyn Trait` dispatch, and method calls on inferred types.

Treat this as the baseline to detect regressions: a drop below ~40% on a comparable Rust codebase indicates a real resolver bug. Higher rates require LSP integration (`lsp_resolve`, already shipped as a separate tool per 3b-v2) — rust-analyzer / pyright / typescript-language-server fill in exactly the categories listed above.

This is the honest ceiling of type-checker-free static analysis. Source: issue #1 resolution + verified on real codebase.

---

## 8. Implementation steps (Simon-style)

### Step 1 — Schema extension

**What it adds:**
- `graph_store.rs`: add all 3b relationship table entries from section 3 to `REL_TABLES`.
- `graph_store.rs`: add edge property columns (`confidence DOUBLE, resolution_method STRING`) to the DDL generator for 3b tables.
- `graph_store.rs`: update `is_valid_rel_table` (in indexer.rs) to include new table names.

**Smoke test:** `create_schema()` succeeds with the new tables. Insert a dummy `Imports_File_Struct` edge with confidence and resolution_method properties. Query it back.

**Estimated LOC:** ~80

---

### Step 2 — Import resolution

**What it adds:**
- `src/resolver.rs`: new module. Function `resolve_imports(store: &GraphStore) -> Result<ResolveStats, String>`.
- Algorithm from section 5.1. Queries all `Import` nodes, resolves paths, inserts `Imports` edges, updates `is_resolved`.
- Handles `crate::`, `self::`, `super::` prefixes. Skips external crates.

**Smoke test:** Index our own `src/` directory with 3a. Run `resolve_imports`. Query `MATCH (f:File)-[r:Imports_File_Struct]->(s:Struct) RETURN f.name, s.name`. Expect at least `main.rs -> GraphStore` (from `use crate::graph_store::GraphStore`).

**Estimated LOC:** ~200

---

### Step 3 — Call-site extraction + call resolution

**What it adds:**
- `rust_parser.rs`: add `extract_call_sites` function. Walks `call_expression` nodes inside function/method bodies. Returns `Vec<ExtractedRef>` with `kind = "Calls"`.
- `resolver.rs`: add `resolve_calls(store: &GraphStore) -> Result<ResolveStats, String>`. Depends on import resolution having run first.
- `indexer.rs`: wire call-site extraction into `index_single_file` (persist `CallSite` nodes).

**Smoke test:** After indexing + resolving our codebase, query `MATCH (a:Function)-[r:Calls_Function_Function]->(b:Function) RETURN a.name, b.name LIMIT 10`. Expect real call edges (e.g., `index_codebase -> collect_rs_files`).

**Estimated LOC:** ~250

---

### Step 4 — Impl-trait binding + supertrait resolution

**What it adds:**
- `resolver.rs`: add `resolve_implements(store: &GraphStore) -> Result<ResolveStats, String>`. Groups methods by (receiver, trait_name), creates `Implements` edges.
- `rust_parser.rs`: add supertrait extraction to `extract_trait`. Store supertraits as `ExtractedRef` entries with `kind = "Extends"`.
- `resolver.rs`: add `resolve_extends(store: &GraphStore) -> Result<ResolveStats, String>`.

**Smoke test:** If our codebase has any `impl Trait for Struct` blocks, verify `Implements` edges exist. For `Extends`, add a test fixture with `trait Foo: Bar {}` and verify the edge.

**Estimated LOC:** ~150

---

### Step 5 — Type-usage resolution (Uses)

**What it adds:**
- `resolver.rs`: add `resolve_uses(store: &GraphStore) -> Result<ResolveStats, String>`. Reads type annotation properties, tokenizes, resolves identifiers.
- Primitive filter list (hardcoded, sourced: Rust Reference, section "Primitive Types").

**Smoke test:** Query `MATCH (f:Field)-[r:Uses_Field_Struct]->(s:Struct) RETURN f.name, s.name`. Expect field type annotations that reference project structs to produce edges.

**Estimated LOC:** ~150

---

### Step 6 — MCP tool wiring + metadata update

**What it adds:**
- `src/main.rs`: add `resolve_graph` tool to `tools_list()` and `handle_tool_call()`.
- `resolver.rs`: add `resolve_graph(store: &GraphStore) -> Result<FullResolveStats, String>` that calls steps 2-5 in order.
- Update `stage-3.index.json` with `phases_completed` and `resolution_stats`.

**Smoke test:** Call `resolve_graph` via MCP JSON-RPC. Verify response contains `resolution_rate > 0`. Verify `stage-3.index.json` has `"resolve"` in `phases_completed`.

**Estimated LOC:** ~100

---

### Step 7 — Cleanup + integration test

**What it adds:**
- Review all new code: zero warnings, no function > 40 LOC, all constants sourced.
- Integration test: index our own codebase, resolve, verify edge counts and resolution rate.
- Verify idempotency: run resolve twice, assert same edge count.

**Smoke test:** `cargo test` passes. `cargo clippy` zero warnings. Integration test proves resolution works end-to-end.

**Estimated LOC:** ~100

---

## Summary

| Step | Name | LOC est. | Cumulative |
|---|---|---|---|
| 1 | Schema extension | ~80 | ~80 |
| 2 | Import resolution | ~200 | ~280 |
| 3 | Call-site extraction + call resolution | ~250 | ~530 |
| 4 | Impl-trait binding + supertrait resolution | ~150 | ~680 |
| 5 | Type-usage resolution | ~150 | ~830 |
| 6 | MCP tool wiring + metadata | ~100 | ~930 |
| 7 | Cleanup + integration test | ~100 | ~1030 |

**Total estimated LOC:** ~1030. **New crates:** 0. **New modules:** 1 (`src/resolver.rs`).

---

## 9. Open questions

1. **Import node `path` quality.** The 3a parser stores the import path from `parse_use_argument`. Need to verify the stored path format matches what the resolver expects (e.g., is `crate::graph_store::GraphStore` stored as-is, or is `crate::` stripped?). Answer: read the Import node data from an actual indexed graph before implementing step 2.

2. **CallSite node existence.** The `NODE_CALL_SITE` label and table exist in the schema, but the 3a parser never creates CallSite nodes. Step 3 adds this extraction. Should CallSite nodes be persisted (adding to the graph's node count) or should call resolution work directly from the tree-sitter re-parse without persisting intermediate nodes? Recommendation: persist them — they are useful for queries like "show me all call sites in this function" even without resolution.

3. **Re-parse vs single-pass.** Steps 3 and 4 require re-parsing files (for call expressions and supertraits). Should the resolver re-read and re-parse every file, or should the 3a parser be extended to extract this data in the first pass? Recommendation: extend the 3a parser to extract call sites and supertraits during the initial parse, storing them as `ExtractedRef` entries. This avoids double-parsing. But this means step 3 modifies `rust_parser.rs` (a 3a file), which is acceptable since 3b builds on 3a.

4. **`is_resolved` property update.** The schema for `Import` and `CallSite` nodes includes `is_resolved` (stage-3.md §5.1), but the 3a implementation does not include this column in the DDL. Step 1 must add it, or it must be retrofitted into 3a first. Need to check: does LadybugDB support `ALTER TABLE` to add a column to an existing node table?

5. **Performance on large codebases.** The resolution algorithms are O(N*M) in the worst case (N refs, M candidate symbols). For our own codebase (~200 symbols) this is trivial. For a 500k-LOC codebase with ~50k symbols, the naive approach may be slow. Mitigation: build an in-memory `HashMap<String, Vec<NodeId>>` index of qualified names before resolution. Defer to step 2 implementation.
