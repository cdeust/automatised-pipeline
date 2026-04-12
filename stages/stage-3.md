# Stage 3 — Codebase Intelligence

**Status:** spec (pre-implementation). No Rust yet. Consumes stage-2's verified finding as input. Inherits all on-disk layout discipline, atomic-write patterns, growth rules, and artifact shapes from stages 1-2.

---

## 1. Purpose

Stage 3 takes a verified finding (`stage-2.verified.json`) and a target codebase path, builds a typed property graph of the codebase's structure, and exposes that graph to agents via MCP tools. The graph is the substrate for all downstream analysis: impact assessment, dependency tracing, symbol search, and architectural verification.

**Design axiom (from GitNexus, restated in our terms):** precompute relational intelligence at index time so that a single tool call returns complete context, rather than requiring the agent to perform multi-round graph walks.

---

## 2. Shannon framing

**Source.** A target codebase directory. The source distribution is the set of all files, their ASTs, and the import/call/type relationships between symbols. This is a finite, observable, deterministic source — no randomness, no unknown distribution.

**Channel.** The MCP tool boundary plus the on-disk graph database. Stage 4 reads the graph; it never re-parses source files. The graph IS the channel.

**Code.** The encoding from AST to graph schema. The parser extracts; the schema defines what survives. Everything in the AST that the schema does not model is lost to downstream stages.

**Information upper bound.** Stage 3 can only hand to stage 4 what it captures in the graph. Any structural relationship present in the source code but absent from the graph schema is lost forever for this run. Section 10 lists what stage 3 must never drop.

---

## 3. Sub-stage decomposition

Stage 3 is large. It decomposes into four sub-stages (3a-3d), each fully implemented before the next begins. Each sub-stage adds tools that work end-to-end on their own.

### 3a — Structure + Parse (the graph exists)

**Implements:** GitNexus phases 1 (Structure) + 2 (Parsing).

**What it does.** Walks the target directory, parses every supported file with tree-sitter, extracts typed nodes and intra-file edges, and persists the graph to an on-disk database.

**MCP tools added:**
- `index_codebase` — end-to-end walk + parse + persist
- `query_graph` — execute a query against the indexed graph (Cypher or the storage port's native query language)
- `get_symbol` — retrieve one node by qualified name, with its immediate edges

**What it can do alone (before 3b).** An agent can index a codebase, browse its structure, query for any symbol, see file-contains-symbol and symbol-contains-symbol relationships, and read intra-file edges (function-contains-call-site, struct-has-field, impl-implements-trait). No cross-file resolution yet — import edges point to unresolved string paths.

**New deps:** `tree-sitter` (MIT), per-language grammar crates (MIT). Storage backend crate — deferred to `dba` agent.

### 3b — Resolution (the graph connects)

**Implements:** GitNexus phase 3 (Resolution).

**What it does.** Resolves import paths, call sites, trait implementations, and type references across files. Converts unresolved string references into typed graph edges pointing to concrete target nodes.

**MCP tools added:**
- `resolve_graph` — run the resolution pass on an already-indexed graph (idempotent; safe to re-run after incremental re-index)

**What it can do alone (with 3a).** Cross-file dependency queries work: "what does this module import?", "who calls this function?", "what implements this trait?". Impact analysis becomes possible because call chains are now walkable.

**New deps:** none. Resolution is our own per-language logic. For v1: Rust-only resolution via tree-sitter queries. Future languages: either hand-written resolvers or LSP integration (deferred; see section 8).

### 3c — Clustering + Processes (the graph understands)

**Implements:** GitNexus phases 4 (Clustering) + 5 (Processes).

**What it does.** Runs Leiden community detection on the resolved graph to group related symbols into functional communities. Traces execution flows from entry points (main, handlers, test functions) through call chains to produce named processes.

**MCP tools added:**
- `cluster_graph` — run Leiden on the graph; write community labels to nodes
- `get_processes` — enumerate detected processes (entry point + call chain + community membership)
- `get_impact` — blast-radius analysis: given a symbol, return all transitively affected symbols, grouped by depth and community

**What it can do alone (with 3a+3b).** An agent can ask "what are the major functional areas of this codebase?", "what is the blast radius of changing this function?", "which process does this symbol participate in?". This is the level at which codebase analysis becomes architecturally useful for PRD generation.

**New deps:** Leiden algorithm — either via the storage backend's built-in graph algorithms (if available) or `fa-leiden-cd` crate (MIT). Decision deferred to `dba` agent's evaluation.

### 3d — Search (the graph is queryable at scale)

**Implements:** GitNexus phase 6 (Search).

**What it does.** Builds hybrid search indexes over the graph: BM25 full-text on symbol names, docstrings, and file paths; semantic vector embeddings on code snippets; Reciprocal Rank Fusion to combine them.

**MCP tools added:**
- `search_codebase` — hybrid search (BM25 + semantic + RRF), returns ranked symbols with context
- `detect_changes` — given a git diff, map changed lines to affected symbols, processes, and communities

**What it can do alone (with 3a+3b+3c).** An agent can find symbols by natural-language query ("the function that handles authentication refresh"), not just by exact name. Change detection maps git diffs to architectural impact. This is full GitNexus-level intelligence.

**New deps:** BM25 — either via storage backend's FTS extension or `tantivy` crate (MIT). Embeddings — via the embedding port (section 9). RRF — arithmetic, no dep. Source: Cormack, Clarke, Buttcher 2009, "Reciprocal Rank Fusion Outperforms Condorcet and Individual Rank Learning Methods."

---

## 4. Full tool surface at stage 3 completion

When all sub-stages are complete, stage 3 adds **9 MCP tools** to the existing 7 (health_check + 2 stage-1 + 4 stage-2), bringing the total to **16**.

| Tool | Sub-stage | GitNexus equivalent | Purpose |
|---|---|---|---|
| `index_codebase` | 3a | phases 1+2 of indexing | Walk + parse + persist graph |
| `query_graph` | 3a | `cypher` | Raw query against the graph |
| `get_symbol` | 3a | `context` (partial) | 360-degree view of one symbol |
| `resolve_graph` | 3b | phase 3 of indexing | Cross-file resolution pass |
| `cluster_graph` | 3c | phase 4 of indexing | Leiden community detection |
| `get_processes` | 3c | process enumeration | Entry-point-rooted call chains |
| `get_impact` | 3c | `impact` | Blast-radius analysis |
| `search_codebase` | 3d | `query` | Hybrid BM25 + semantic + RRF search |
| `detect_changes` | 3d | `detect_changes` | Git diff to architectural impact |

### GitNexus tools we reproduce

| GitNexus tool | Our equivalent | Notes |
|---|---|---|
| `query` | `search_codebase` | Renamed for clarity — "query" is overloaded |
| `context` | `get_symbol` | Same 360-degree symbol view |
| `impact` | `get_impact` | Same blast-radius analysis |
| `detect_changes` | `detect_changes` | Same git-diff mapping |
| `cypher` | `query_graph` | Raw graph query escape hatch |

### GitNexus tools we skip (with justification)

| GitNexus tool | Reason for skipping |
|---|---|
| `list_repos` | Our pipeline operates on one codebase per run; repo enumeration is the orchestrator's job, not the graph's |
| `rename` | Multi-file coordinated rename is a code-modification tool; stage 3 is read-only analysis. Rename belongs in a future stage that writes code |
| `group_list`, `group_sync`, `group_contracts`, `group_query` | Multi-repo workgroup operations. Deferred to a future stage. Our pipeline processes one codebase at a time; cross-repo analysis is a separate concern |
| Prompts (`detect_impact`, `generate_map`) | These are prompt templates, not tools. The agent layer owns prompts; stage 3 provides data |

---

## 5. Graph schema

### 5.1 Node labels

Every node carries: `id` (unique within the graph, deterministic from qualified path), `name` (short name), `qualified_name` (fully qualified), `file_path` (source file), `start_line`, `end_line`, `start_col`, `end_col`.

```rust
enum NodeLabel {
    // Structure (phase 1)
    Directory,      // filesystem directory
    File,           // source file

    // Symbols (phase 2 — extracted by tree-sitter)
    Module,         // Rust: mod; Python: module; TS: file-as-module
    Function,       // free function or static method
    Method,         // instance method (has receiver)
    Struct,         // Rust: struct; Python: class; TS/Java: class
    Enum,           // Rust: enum; TS: enum; Python: class with variants
    Variant,        // enum variant
    Trait,          // Rust: trait; Java: interface; TS: interface; Python: Protocol
    Field,          // struct/class field
    Constant,       // const, static, or module-level constant
    TypeAlias,      // type alias / typedef

    // References (phase 2 — extracted but unresolved until phase 3)
    Import,         // use/import statement (unresolved path as property)
    CallSite,       // function/method call expression (unresolved target as property)

    // Derived (phase 4+)
    Community,      // Leiden cluster — a virtual node grouping symbols
    Process,        // entry-point-rooted execution flow
}
```

**Invariants per node label:**

| Label | Required properties beyond base | Invariant |
|---|---|---|
| `Directory` | `path` (absolute) | Exactly one per unique directory in the indexed tree |
| `File` | `path` (absolute), `language`, `byte_size`, `line_count` | Exactly one per parsed file |
| `Module` | `visibility` | One per Rust `mod`, Python file, TS file |
| `Function` | `visibility`, `is_async`, `parameter_count`, `return_type_text` | `return_type_text` is the source text of the return type annotation, not a resolved type. "I don't know" if no annotation |
| `Method` | same as Function + `receiver_type_text` | Distinguished from Function by having a receiver (`self`, `this`, `cls`) |
| `Struct` | `visibility`, `field_count`, `is_generic` | |
| `Enum` | `visibility`, `variant_count` | |
| `Variant` | `has_fields` | Parent is always an Enum |
| `Trait` | `visibility`, `method_count`, `is_generic` | |
| `Field` | `visibility`, `type_text` | `type_text` is source text, not resolved |
| `Constant` | `visibility`, `type_text` | |
| `TypeAlias` | `visibility`, `target_type_text` | |
| `Import` | `raw_path` (the literal import string), `is_resolved` (bool) | `is_resolved` flips from false to true after resolution pass |
| `CallSite` | `raw_callee` (the literal callee text), `is_resolved` (bool) | Same resolution semantics as Import |
| `Community` | `algorithm` ("leiden"), `resolution_param`, `member_count` | Virtual node; no source location |
| `Process` | `entry_kind` (main/handler/test), `depth`, `symbol_count` | Virtual node; no source location |

### 5.2 Edge types

```rust
enum EdgeKind {
    // Structural (phase 1)
    Contains,           // Directory -> File, Directory -> Directory
    
    // Containment (phase 2)
    Defines,            // File -> Symbol, Module -> Symbol, Struct -> Field, Enum -> Variant
    
    // Intra-file references (phase 2 — from tree-sitter AST)
    HasMethod,          // Struct/Enum/Trait -> Method
    HasField,           // Struct -> Field
    HasVariant,         // Enum -> Variant
    Returns,            // Function/Method -> TypeAlias/Struct/Enum (if type annotation exists)
    ParameterOf,        // (type node) -> Function/Method

    // Cross-file references (phase 3 — after resolution)
    Imports,            // File/Module -> File/Module/Symbol (resolved import)
    Calls,              // Function/Method -> Function/Method (resolved call)
    Implements,         // Struct -> Trait
    Extends,            // Struct -> Struct (inheritance / newtype)
    Uses,               // Symbol -> Symbol (generic type usage, field access on external type)
    
    // Derived (phase 4+)
    MemberOf,           // Symbol -> Community
    ParticipatesIn,     // Symbol -> Process
    EntryPointOf,       // Function/Method -> Process
}
```

**Edge invariants:**

| Edge | Source label(s) | Target label(s) | Cardinality | Invariant |
|---|---|---|---|---|
| `Contains` | Directory | File, Directory | 1:N | Tree structure — no cycles |
| `Defines` | File, Module, Struct, Enum | any Symbol | 1:N | Every symbol has exactly one `Defines` parent |
| `HasMethod` | Struct, Enum, Trait | Method | 1:N | |
| `HasField` | Struct | Field | 1:N | |
| `HasVariant` | Enum | Variant | 1:N | |
| `Imports` | File, Module | File, Module, Symbol | N:M | Only exists after resolution; `is_resolved` on the Import node must be true |
| `Calls` | Function, Method | Function, Method | N:M | Only exists after resolution; `is_resolved` on the CallSite node must be true |
| `Implements` | Struct | Trait | N:M | Only after resolution |
| `MemberOf` | any Symbol | Community | N:1 per resolution level | A symbol belongs to exactly one community at each Leiden resolution level |
| `ParticipatesIn` | any Symbol | Process | N:M | |
| `EntryPointOf` | Function, Method | Process | 1:1 | Each process has exactly one entry point |

---

## 6. Parser port

The interface any language-specific parser must satisfy. Language-agnostic at the port level; language-specific at the adapter level.

### 6.1 Port trait (Rust pseudocode)

```rust
/// Input to the parser port.
struct ParseRequest {
    file_path: PathBuf,        // absolute path to the source file
    file_content: String,      // the file's UTF-8 content (caller reads it)
    language: Language,        // enum: Rust, Python, TypeScript, Java, Go, etc.
}

/// A single extracted symbol.
struct ExtractedNode {
    label: NodeLabel,
    name: String,              // short name (e.g. "foo")
    qualified_name: String,    // fully qualified (e.g. "crate::module::foo")
    span: Span,                // { start_line, end_line, start_col, end_col }
    properties: BTreeMap<String, Value>,  // label-specific properties per §5.1
    children: Vec<ExtractedNode>,        // nested symbols (methods in struct, etc.)
}

/// A single extracted reference (unresolved).
struct ExtractedRef {
    kind: RefKind,             // enum: Import, Call, TypeRef, Impl
    raw_text: String,          // the literal text of the reference
    span: Span,
    source_symbol: String,     // qualified name of the containing symbol
}

/// Output of the parser port for one file.
struct ParseResult {
    file_path: PathBuf,
    language: Language,
    nodes: Vec<ExtractedNode>,
    refs: Vec<ExtractedRef>,
    line_count: u32,
    byte_size: u64,
    parse_errors: Vec<ParseError>,  // tree-sitter error nodes — reported, not fatal
}

/// The port trait.
trait CodeParser {
    /// Parse one file. Must be deterministic: same input -> same output.
    fn parse(&self, request: ParseRequest) -> Result<ParseResult, ParseError>;
    
    /// Which languages does this parser support?
    fn supported_languages(&self) -> Vec<Language>;
}
```

### 6.2 Language enum

```rust
enum Language {
    Rust,        // tree-sitter-rust (MIT)
    Python,      // tree-sitter-python (MIT)
    TypeScript,  // tree-sitter-typescript (MIT)
    JavaScript,  // tree-sitter-javascript (MIT)
    Java,        // tree-sitter-java (MIT)
    Go,          // tree-sitter-go (MIT)
    // Future: PHP, Ruby, Swift, Kotlin — each requires a grammar crate + adapter
}
```

**v1 ships Rust only.** Additional languages are added one at a time, each as its own PR, each with its own adapter implementing `CodeParser`. The port contract does not change when a language is added.

### 6.3 Resolution port

```rust
/// Input: the full graph after parsing (all nodes + unresolved refs).
/// Output: resolved edges to add to the graph.
struct ResolvedEdge {
    source_id: NodeId,         // the calling/importing symbol
    target_id: NodeId,         // the resolved target symbol
    edge_kind: EdgeKind,       // Imports, Calls, Implements, Extends, Uses
    confidence: f32,           // 0.0-1.0; how confident is the resolution?
    resolution_method: String, // "tree-sitter-query" | "lsp" | "heuristic-name-match"
}

trait SymbolResolver {
    /// Resolve all unresolved references in the graph.
    /// Idempotent: calling twice produces the same edges.
    fn resolve(&self, graph: &Graph) -> Result<Vec<ResolvedEdge>, ResolveError>;
    
    /// Which languages does this resolver handle?
    fn supported_languages(&self) -> Vec<Language>;
}
```

**`confidence` field rationale.** Resolution is not binary. A name match across files might be wrong (shadowing, re-exports). The confidence score lets downstream tools filter: `get_impact` can require `confidence >= 0.8`; `query_graph` returns everything. Confidence thresholds are NOT hardcoded in the spec — they are parameters to the tools that consume resolved edges. Source: this follows the same pattern as stage-1's `relevance_score` — preserved but not acted upon at the producing stage.

---

## 7. Storage port

The interface the graph database must satisfy. The `dba` agent picks the implementation.

```rust
trait GraphStore {
    // --- Schema ---
    fn create_schema(&mut self) -> Result<(), StoreError>;
    
    // --- Write ---
    fn insert_node(&mut self, node: &GraphNode) -> Result<NodeId, StoreError>;
    fn insert_edge(&mut self, edge: &GraphEdge) -> Result<EdgeId, StoreError>;
    fn update_node_property(&mut self, id: NodeId, key: &str, value: &Value) -> Result<(), StoreError>;
    fn bulk_insert_nodes(&mut self, nodes: &[GraphNode]) -> Result<Vec<NodeId>, StoreError>;
    fn bulk_insert_edges(&mut self, edges: &[GraphEdge]) -> Result<Vec<EdgeId>, StoreError>;
    
    // --- Read ---
    fn get_node(&self, id: NodeId) -> Result<Option<GraphNode>, StoreError>;
    fn get_node_by_qualified_name(&self, qname: &str) -> Result<Option<GraphNode>, StoreError>;
    fn get_edges_from(&self, id: NodeId, kind: Option<EdgeKind>) -> Result<Vec<GraphEdge>, StoreError>;
    fn get_edges_to(&self, id: NodeId, kind: Option<EdgeKind>) -> Result<Vec<GraphEdge>, StoreError>;
    
    // --- Query ---
    fn execute_query(&self, query: &str) -> Result<QueryResult, StoreError>;
    
    // --- Graph algorithms ---
    fn shortest_path(&self, from: NodeId, to: NodeId) -> Result<Vec<NodeId>, StoreError>;
    fn transitive_closure(&self, from: NodeId, edge_kinds: &[EdgeKind], max_depth: u32) -> Result<Vec<(NodeId, u32)>, StoreError>;
    
    // --- Lifecycle ---
    fn open(path: &Path) -> Result<Self, StoreError> where Self: Sized;
    fn flush(&mut self) -> Result<(), StoreError>;
    fn close(self) -> Result<(), StoreError>;
}
```

### 7.1 What the storage port NEEDS (requirements for `dba`)

| Requirement | Why | Non-negotiable? |
|---|---|---|
| Property graph (typed nodes + typed edges + properties on both) | The schema in section 5 requires it | Yes |
| Embedded (in-process, no server) | The MCP runs as a single process; no Docker, no sidecar | Yes |
| ACID transactions (at minimum atomic writes) | Crash safety — same discipline as stages 1-2 | Yes |
| Query language (Cypher or equivalent) | `query_graph` exposes raw queries to agents | Yes |
| Full-text search on string properties | `search_codebase` BM25 component | Yes (for 3d) |
| Vector index on float arrays | `search_codebase` semantic component | Yes (for 3d) |
| Graph algorithm support (shortest path, community detection) | `get_impact`, `cluster_graph` | Preferred; can be done in Rust if not available |
| Rust crate on crates.io | Build integration | Yes |
| MIT or Apache-2.0 license | Partner program constraint | Yes |

---

## 8. Search port

The interface MCP tools call when they need to find symbols.

```rust
/// A search query — text, structured, or both.
enum SearchQuery {
    /// Full-text search over symbol names, docstrings, file paths.
    Text { query: String, limit: u32 },
    
    /// Semantic similarity search using embeddings.
    Semantic { query: String, limit: u32 },
    
    /// Hybrid: BM25 + semantic + RRF fusion.
    Hybrid { query: String, limit: u32, bm25_weight: f32, semantic_weight: f32 },
    
    /// Structured: find by label, property filter, edge constraint.
    Structured { filters: Vec<PropertyFilter>, edge_constraints: Vec<EdgeConstraint>, limit: u32 },
}

struct SearchResult {
    node_id: NodeId,
    qualified_name: String,
    label: NodeLabel,
    score: f32,              // 0.0-1.0, normalized
    score_components: ScoreComponents,  // { bm25: f32, semantic: f32, rrf: f32 }
    snippet: String,         // context around the match
    file_path: PathBuf,
    span: Span,
}

trait CodeSearch {
    fn search(&self, query: SearchQuery) -> Result<Vec<SearchResult>, SearchError>;
    
    /// Index all nodes for search. Called after index_codebase or resolve_graph.
    fn build_index(&mut self, graph: &dyn GraphStore) -> Result<IndexStats, SearchError>;
}
```

### 8.1 Embedding port

```rust
trait Embedder {
    /// Embed a batch of text snippets into vectors.
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError>;
    
    /// Embedding dimension (e.g., 384 for all-MiniLM-L6-v2).
    fn dimension(&self) -> usize;
    
    /// Model identifier for provenance.
    fn model_id(&self) -> &str;
}
```

The embedding model is NOT prescribed. The infrastructure adapter injects it. The search port accepts any `Embedder` implementation.

### 8.2 RRF scoring

Source: Cormack, Clarke, Buttcher 2009. Formula:

```
RRF(d) = sum over rankers r of: 1 / (k + rank_r(d))
```

where `k = 60` (the paper's recommended constant; Cormack et al. 2009, Section 3). `k` is a named constant in the implementation: `const RRF_K: u32 = 60; // Cormack et al. 2009 §3`.

---

## 9. On-disk layout

Extension of stages 1-2. Per finding, per run:

```
runs/<run_id>/findings/<finding_id>/
  stage-1.source.json          (stage 1)
  stage-1.extracted.json       (stage 1)
  stage-1.refined.json         (stage 1)
  stage-2.session.json         (stage 2)
  stage-2.verified.json        (stage 2)
  stage-3.graph/               (stage 3 — the graph database directory)
    <backend-specific files>   (e.g., kuzu data files, or sqlite DB)
  stage-3.index.json           (stage 3 — metadata about the indexed graph)
```

### 9.1 `stage-3.index.json` schema

```json
{
  "type": "object",
  "required": [
    "run_id", "finding_id", "codebase_path", "indexed_at",
    "node_count", "edge_count", "file_count", "language_stats",
    "indexer_version", "phases_completed", "graph_path"
  ],
  "additionalProperties": false,
  "properties": {
    "run_id":           { "type": "string" },
    "finding_id":       { "type": "string" },
    "codebase_path":    { "type": "string", "description": "Absolute path to the indexed directory." },
    "indexed_at":       { "type": "string", "format": "date-time" },
    "node_count":       { "type": "integer", "minimum": 0 },
    "edge_count":       { "type": "integer", "minimum": 0 },
    "file_count":       { "type": "integer", "minimum": 0 },
    "language_stats": {
      "type": "object",
      "additionalProperties": {
        "type": "object",
        "properties": {
          "file_count":  { "type": "integer" },
          "line_count":  { "type": "integer" },
          "node_count":  { "type": "integer" },
          "parse_error_count": { "type": "integer" }
        }
      }
    },
    "indexer_version":  { "type": "string" },
    "phases_completed": {
      "type": "array",
      "items": { "enum": ["structure", "parse", "resolve", "cluster", "process", "search"] },
      "description": "Ordered list of completed phases. Monotonically grows as sub-stages run."
    },
    "graph_path":       { "type": "string", "description": "Relative path to stage-3.graph/ directory." },
    "resolution_stats": {
      "type": ["object", "null"],
      "properties": {
        "total_refs":    { "type": "integer" },
        "resolved_refs": { "type": "integer" },
        "resolution_rate": { "type": "number" }
      }
    },
    "cluster_stats": {
      "type": ["object", "null"],
      "properties": {
        "community_count": { "type": "integer" },
        "modularity":      { "type": "number" },
        "resolution_param": { "type": "number" }
      }
    },
    "search_stats": {
      "type": ["object", "null"],
      "properties": {
        "bm25_indexed":    { "type": "boolean" },
        "vector_indexed":  { "type": "boolean" },
        "embedding_model": { "type": ["string", "null"] },
        "embedding_dim":   { "type": ["integer", "null"] }
      }
    }
  }
}
```

### 9.2 Run index update

`runs/<run_id>/index.json` gains per-finding fields when stage 3 indexes:

```json
{
  "indexed_at":    "<indexed_at>",
  "indexed":       true,
  "stage3_path":   "findings/<finding_id>/stage-3.index.json",
  "node_count":    1234,
  "edge_count":    5678
}
```

Atomic update rules: same as stages 1-2 (tempfile + rename).

---

## 10. What stage 3 must never lose

1. **Every symbol's source location** (`file_path` + `span`). Without it, no tool can point the agent back to the actual code.
2. **The containment tree** (Directory -> File -> Module -> Symbol). Without it, "where is this symbol?" has no answer.
3. **Every import and call site, even unresolved ones.** An unresolved reference is data ("we saw this but couldn't resolve it"). Dropping it silently makes the graph look complete when it isn't.
4. **The `is_resolved` flag on Import and CallSite nodes.** Stage 4 must distinguish "resolved with confidence 0.95" from "never attempted resolution" from "attempted, failed."
5. **Parse errors per file.** Tree-sitter error recovery means partial parses succeed. The error count per file is a quality signal; dropping it hides broken parses behind clean node counts.
6. **The `phases_completed` array.** Stage 4 must know whether this graph has been through resolution, clustering, and search indexing, or only structure+parse.
7. **Resolution confidence scores.** Without confidence, downstream tools cannot distinguish a definite call edge from a heuristic guess.

Stage 3 is **allowed** to lose:
- Whitespace, comments, and formatting (the AST is the boundary; raw source text is not persisted in the graph, only spans for re-reading).
- Tree-sitter's full CST (concrete syntax tree). We extract symbols and references; internal expression structure is not modeled.
- Dead code that tree-sitter parses but no edge ever touches. The nodes exist, but their isolation is data, not a defect.

---

## 11. MCP tool schemas

### 11.1 `index_codebase` (sub-stage 3a)

```json
{
  "type": "object",
  "required": ["run_id", "finding_id", "output_dir", "codebase_path"],
  "additionalProperties": false,
  "properties": {
    "run_id":         { "type": "string" },
    "finding_id":     { "type": "string" },
    "output_dir":     { "type": "string", "pattern": "^/.+" },
    "codebase_path":  { "type": "string", "pattern": "^/.+", "description": "Absolute path to the directory to index." },
    "languages":      { "type": "array", "items": { "type": "string" }, "description": "Languages to parse. Default: auto-detect from file extensions." },
    "exclude_patterns": { "type": "array", "items": { "type": "string" }, "description": "Glob patterns to exclude (e.g. 'target/**', 'node_modules/**')." }
  }
}
```

**Preconditions.** (i) `stage-2.verified.json` exists for this finding and `verified: true`. (ii) `codebase_path` is a readable directory. (iii) `run_id`/`finding_id` safe-ID per stage-1.md section 5.1.4.

**Postconditions.** Creates `stage-3.graph/` directory with the populated graph. Writes `stage-3.index.json` with `phases_completed: ["structure", "parse"]`. Updates `runs/<run_id>/index.json`. All writes atomic.

**Output.** `{ stage: 3, status: "ok", node_count, edge_count, file_count, language_stats, elapsed_ms, graph_path, index_path }`.

### 11.2 `query_graph` (sub-stage 3a)

```json
{
  "type": "object",
  "required": ["run_id", "finding_id", "output_dir", "query"],
  "additionalProperties": false,
  "properties": {
    "run_id":      { "type": "string" },
    "finding_id":  { "type": "string" },
    "output_dir":  { "type": "string", "pattern": "^/.+" },
    "query":       { "type": "string", "minLength": 1, "description": "Query in the storage backend's native language." },
    "limit":       { "type": "integer", "minimum": 1, "maximum": 1000, "default": 100 }
  }
}
```

**Output.** `{ stage: 3, status: "ok", rows: [...], row_count, elapsed_ms }`.

### 11.3 `get_symbol` (sub-stage 3a)

```json
{
  "type": "object",
  "required": ["run_id", "finding_id", "output_dir", "qualified_name"],
  "additionalProperties": false,
  "properties": {
    "run_id":          { "type": "string" },
    "finding_id":      { "type": "string" },
    "output_dir":      { "type": "string", "pattern": "^/.+" },
    "qualified_name":  { "type": "string", "minLength": 1 },
    "depth":           { "type": "integer", "minimum": 0, "maximum": 3, "default": 1, "description": "How many hops of edges to include." }
  }
}
```

**Output.** `{ stage: 3, status: "ok", node: {...}, edges_out: [...], edges_in: [...], depth_reached }`.

### 11.4 `resolve_graph` (sub-stage 3b)

```json
{
  "type": "object",
  "required": ["run_id", "finding_id", "output_dir"],
  "additionalProperties": false,
  "properties": {
    "run_id":      { "type": "string" },
    "finding_id":  { "type": "string" },
    "output_dir":  { "type": "string", "pattern": "^/.+" },
    "min_confidence": { "type": "number", "minimum": 0, "maximum": 1, "default": 0.5, "description": "Only persist edges with confidence >= this threshold." }
  }
}
```

**Preconditions.** `stage-3.index.json` exists, `phases_completed` includes "structure" and "parse".

**Postconditions.** Adds resolved edges to the graph. Updates `is_resolved` on Import/CallSite nodes. Updates `stage-3.index.json` with `resolution_stats` and appends "resolve" to `phases_completed`.

**Output.** `{ stage: 3, status: "ok", total_refs, resolved_refs, resolution_rate, edges_added, elapsed_ms }`.

### 11.5 `cluster_graph` (sub-stage 3c)

```json
{
  "type": "object",
  "required": ["run_id", "finding_id", "output_dir"],
  "additionalProperties": false,
  "properties": {
    "run_id":           { "type": "string" },
    "finding_id":       { "type": "string" },
    "output_dir":       { "type": "string", "pattern": "^/.+" },
    "resolution_param": { "type": "number", "minimum": 0.0, "default": 1.0, "description": "Leiden resolution parameter. Higher = more communities. Source: Traag, Waltman, van Eck 2019." }
  }
}
```

**Preconditions.** `phases_completed` includes "resolve".

**Postconditions.** Creates Community nodes and MemberOf edges. Updates `stage-3.index.json` with `cluster_stats` and appends "cluster" to `phases_completed`.

**Output.** `{ stage: 3, status: "ok", community_count, modularity, elapsed_ms }`.

### 11.6 `get_processes` (sub-stage 3c)

```json
{
  "type": "object",
  "required": ["run_id", "finding_id", "output_dir"],
  "additionalProperties": false,
  "properties": {
    "run_id":     { "type": "string" },
    "finding_id": { "type": "string" },
    "output_dir": { "type": "string", "pattern": "^/.+" },
    "entry_kind": { "type": "string", "enum": ["main", "handler", "test", "all"], "default": "all" }
  }
}
```

**Preconditions.** `phases_completed` includes "resolve" and "cluster".

**Postconditions.** Creates Process nodes, EntryPointOf and ParticipatesIn edges. Appends "process" to `phases_completed`.

**Output.** `{ stage: 3, status: "ok", process_count, processes: [{ name, entry_kind, symbol_count, community_ids }] }`.

### 11.7 `get_impact` (sub-stage 3c)

```json
{
  "type": "object",
  "required": ["run_id", "finding_id", "output_dir", "qualified_name"],
  "additionalProperties": false,
  "properties": {
    "run_id":         { "type": "string" },
    "finding_id":     { "type": "string" },
    "output_dir":     { "type": "string", "pattern": "^/.+" },
    "qualified_name": { "type": "string", "minLength": 1 },
    "max_depth":      { "type": "integer", "minimum": 1, "maximum": 10, "default": 5 },
    "min_confidence": { "type": "number", "minimum": 0, "maximum": 1, "default": 0.7 }
  }
}
```

**Output.** `{ stage: 3, status: "ok", root: {...}, affected: [{ node, depth, path, confidence, community }], affected_count, communities_affected }`.

### 11.8 `search_codebase` (sub-stage 3d)

```json
{
  "type": "object",
  "required": ["run_id", "finding_id", "output_dir", "query"],
  "additionalProperties": false,
  "properties": {
    "run_id":      { "type": "string" },
    "finding_id":  { "type": "string" },
    "output_dir":  { "type": "string", "pattern": "^/.+" },
    "query":       { "type": "string", "minLength": 1 },
    "mode":        { "type": "string", "enum": ["text", "semantic", "hybrid"], "default": "hybrid" },
    "limit":       { "type": "integer", "minimum": 1, "maximum": 100, "default": 20 },
    "label_filter": { "type": "array", "items": { "type": "string" }, "description": "Only return nodes with these labels." }
  }
}
```

**Preconditions.** `phases_completed` includes "search".

**Output.** `{ stage: 3, status: "ok", results: [{ qualified_name, label, score, score_components, snippet, file_path, span }], total_matched }`.

### 11.9 `detect_changes` (sub-stage 3d)

```json
{
  "type": "object",
  "required": ["run_id", "finding_id", "output_dir", "diff"],
  "additionalProperties": false,
  "properties": {
    "run_id":      { "type": "string" },
    "finding_id":  { "type": "string" },
    "output_dir":  { "type": "string", "pattern": "^/.+" },
    "diff":        { "type": "string", "description": "Unified diff text (output of `git diff`)." },
    "max_depth":   { "type": "integer", "minimum": 1, "maximum": 10, "default": 3 }
  }
}
```

**Output.** `{ stage: 3, status: "ok", changed_symbols: [...], affected_symbols: [...], affected_processes: [...], affected_communities: [...] }`.

---

## 12. Constants

Every hardcoded value, sourced:

| Constant | Value | Source |
|---|---|---|
| `INDEXER_VERSION` | `"1.0.0"` | Compile-time constant, same pattern as `EXTRACTOR_VERSION` in stage 1 |
| `RRF_K` | `60` | Cormack, Clarke, Buttcher 2009, Section 3 |
| `DEFAULT_LEIDEN_RESOLUTION` | `1.0` | Traag, Waltman, van Eck 2019 — default resolution parameter |
| `DEFAULT_IMPACT_MAX_DEPTH` | `5` | Implementation-defined; balances completeness vs. combinatorial explosion on large graphs |
| `DEFAULT_SEARCH_LIMIT` | `20` | Implementation-defined; matches typical LLM context-window budget for tool results |
| `MAX_SEARCH_LIMIT` | `100` | Implementation-defined; prevents runaway result sets |
| `DEFAULT_MIN_CONFIDENCE` | `0.5` | Implementation-defined; excludes low-quality heuristic resolutions by default |
| `SAFE_ID_MAX_LEN` | `128` | Inherited from stage 1 (stage-1.md section 9.3 Q4) |

---

## 13. Invariants (stage-3 specific)

Stage 3 inherits all stage-1 and stage-2 invariants. Additional:

1. **Deterministic indexing.** Same codebase at same commit, same language set, same exclude patterns -> same graph (modulo node ID generation strategy, which must be deterministic from qualified names).
2. **Phase ordering.** `phases_completed` is monotonically ordered: structure < parse < resolve < cluster < process < search. No phase may run before its prerequisites.
3. **Resolution idempotency.** Running `resolve_graph` twice produces the same set of resolved edges. The second run is a no-op if nothing changed.
4. **No writes to the codebase.** Stage 3 is read-only with respect to the target codebase directory. It never modifies, creates, or deletes files in `codebase_path`.
5. **Graph is append-only within a session.** Once a node/edge is written, it is never deleted within a single pipeline run. Re-indexing creates a new graph, not a mutated one.
6. **Verified precondition.** `index_codebase` requires `stage-2.verified.json` with `verified: true`. An unverified finding does not get indexed.

---

## 14. Source citations

| Claim | Source |
|---|---|
| GitNexus 6-phase pipeline + 16 tools | `stages/stage-3-research.md` section 1.1, 1.2 (sourced from GitNexus README + blog posts) |
| Tree-sitter Rust bindings, MIT license | `crates.io/crates/tree-sitter` |
| Leiden algorithm | Traag, Waltman, van Eck 2019, "From Louvain to Leiden: guaranteeing well-connected communities", *Scientific Reports* 9:5233 |
| RRF formula + k=60 | Cormack, Clarke, Buttcher 2009, "Reciprocal Rank Fusion Outperforms Condorcet and Individual Rank Learning Methods", SIGIR 2009 |
| BM25 origin | Robertson, Walker, Jones, Hancock-Beaulieu, Gatford 1994, "Okapi at TREC-3" |
| On-disk layout, atomic writes, safe-ID | stages/stage-1.md sections 4.4, 5.1, 5.2 |
| Single-file session, ACID | stages/stage-2.md section 12.3 |
| GitNexus PolyForm Noncommercial license (clean-room constraint) | `stages/stage-3-research.md` section 1.5 |
| Swift reference types (DependencyNodeType, DependencyEdgeType) | `ai-architect-prd-builder` domain model (user-reported, not copied) |

---

## 15. Open questions

1. **Storage backend selection.** Kuzu 0.11.3, LadybugDB, or alternatives? Determines whether clustering/FTS/vector are built-in or require external crates. **Decision: deferred to `dba` agent.**
2. **Embedding model for semantic search.** Which model, what dimension, local vs. remote? **Decision: deferred to infrastructure adapter via the Embedder port.** The spec does not prescribe.
3. **Multi-language resolution strategy.** v1 ships Rust-only. For Python/TypeScript/Java/Go, do we (a) write hand-coded resolvers per language, (b) integrate LSP servers, or (c) accept lower-confidence heuristic resolution? **Decision: deferred until v1 Rust resolver is validated.** I don't know which approach gives the best cost/quality tradeoff without empirical data from v1.
4. **Graph-per-finding vs. graph-per-codebase.** The current layout (section 9) puts the graph under the finding directory. If two findings target the same codebase, should they share a graph? **Recommendation: graph-per-finding in v1 (simplicity, isolation). Shared graph is an optimization for a future stage.** Needs user confirmation.
5. **Leiden resolution parameter tuning.** Default 1.0 per the paper. Optimal value depends on codebase size and density. Should `cluster_graph` accept this as a parameter (yes, it does in the schema) and should we provide heuristics for auto-selection? **Deferred: ship with manual parameter, add auto-selection after empirical data from multiple codebases.**
