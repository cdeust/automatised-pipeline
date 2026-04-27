// tool_schemas — MCP tool JSON Schema definitions.
//
// Pure data: every function returns a `serde_json::Value` containing the
// JSON-RPC tool schema. No logic, no I/O. Extracted from main.rs when
// tools_list() exceeded 200 LOC (NOTES.md growth rule).

use serde_json::{json, Value};

/// Returns the full `tools/list` response payload.
pub fn tools_list() -> Value {
    json!({
        "tools": [
            health_check_schema(),
            extract_finding_schema(),
            refine_finding_schema(),
            start_verification_schema(),
            append_clarification_schema(),
            finalize_verification_schema(),
            abort_verification_schema(),
            index_codebase_schema(),
            query_graph_schema(),
            get_symbol_schema(),
            resolve_graph_schema(),
            cluster_graph_schema(),
            get_processes_schema(),
            get_impact_schema(),
            search_codebase_schema(),
            get_context_schema(),
            analyze_codebase_schema(),
            detect_changes_schema(),
            lsp_resolve_schema(),
            prepare_prd_input_schema(),
            validate_prd_against_graph_schema(),
            check_security_gates_schema(),
            verify_semantic_diff_schema(),
        ]
    })
}

fn health_check_schema() -> Value {
    json!({
        "name": "health_check",
        "description": "Stage 0 — Healthcheck + handshake verification. Returns server identity, protocol version, and the registered stage count. Use this before calling any other stage tool to confirm the MCP is live.",
        "inputSchema": {
            "type": "object",
            "properties": {},
            "additionalProperties": false
        }
    })
}

fn extract_finding_schema() -> Value {
    json!({
        "name": "extract_finding",
        "description": "Stage 1a — Deterministic extraction. Normalizes one incoming finding (inline object or absolute path to a .json file) to the canonical schema, writes stage-1.source.json + stage-1.extracted.json atomically under <output_dir>/runs/<run_id>/findings/<finding_id>/, and creates or updates an index.json entry. Does NOT call an LLM. The caller runs the orchestrator refinement and then calls refine_finding with the payload.",
        "inputSchema": {
            "type": "object",
            "required": ["finding", "output_dir"],
            "additionalProperties": false,
            "properties": {
                "finding": {
                    "description": "Either an inline finding object (must match spec §3.2) or an absolute path to a .json file containing a finding or a {findings: [...]} wrapper with exactly one entry. .md paths are rejected in v1 (spec §9.3 Q1).",
                    "oneOf": [
                        { "type": "object" },
                        { "type": "string", "pattern": "^/.+\\.json$" }
                    ]
                },
                "output_dir": {
                    "type": "string",
                    "pattern": "^/.+",
                    "description": "Absolute directory where the artifact will be staged."
                },
                "run_id": {
                    "type": "string",
                    "description": "Optional run identifier. Auto-generated as YYYYMMDD-HHMMSS-<6 lowercase alphanumeric> (UTC) when absent."
                }
            }
        }
    })
}

fn refine_finding_schema() -> Value {
    json!({
        "name": "refine_finding",
        "description": "Stage 1b — Orchestrator-aware persistence. Reads an existing stage-1.extracted.json, composes stage-1.refined.json with the agent-produced refined_prompt + refinement payload, and updates index.json atomically. Pure persistence — no LLM call, no network. Requires extract_finding to have been called first for the same (run_id, finding_id).",
        "inputSchema": {
            "type": "object",
            "required": ["run_id", "finding_id", "output_dir", "refined_prompt", "refinement"],
            "additionalProperties": false,
            "properties": {
                "run_id":     { "type": "string" },
                "finding_id": { "type": "string" },
                "output_dir": { "type": "string", "pattern": "^/.+" },
                "refined_prompt": refined_prompt_schema(),
                "refinement": refinement_schema(),
            }
        }
    })
}

fn refined_prompt_schema() -> Value {
    json!({
        "type": "object",
        "required": ["text", "role_hint"],
        "additionalProperties": false,
        "properties": {
            "text":           { "type": "string", "minLength": 1 },
            "role_hint":      { "type": "string" },
            "token_estimate": { "type": ["integer", "null"] }
        }
    })
}

fn refinement_schema() -> Value {
    json!({
        "type": "object",
        "required": ["added_context", "orchestrator_version"],
        "additionalProperties": false,
        "properties": {
            "added_context": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["kind", "content"],
                    "additionalProperties": false,
                    "properties": {
                        "kind":       { "type": "string" },
                        "content":    { "type": "string" },
                        "provenance": { "type": "string" }
                    }
                }
            },
            "orchestrator_version": { "type": "string" }
        }
    })
}

fn start_verification_schema() -> Value {
    json!({
        "name": "start_verification",
        "description": "Stage 2a — Create a clarification session for a refined finding. Verifies stage-1.refined.json exists and parses (schema_ok), then atomically writes stage-2.session.json with state 'open'. Rejects if an existing session is finalized; overwrites an aborted session. No LLM call.",
        "inputSchema": {
            "type": "object",
            "required": ["run_id", "finding_id", "output_dir"],
            "additionalProperties": false,
            "properties": {
                "run_id":     { "type": "string" },
                "finding_id": { "type": "string" },
                "output_dir": { "type": "string", "pattern": "^/.+" }
            }
        }
    })
}

fn append_clarification_schema() -> Value {
    json!({
        "name": "append_clarification",
        "description": "Stage 2b — Append one turn (agent_question or user_answer) to stage-2.session.json. Enforces the alternation invariant (two consecutive same-kind turns rejected) and the §3 state machine. Whole-file atomic rewrite per spec §12.3. No LLM call.",
        "inputSchema": {
            "type": "object",
            "required": ["run_id", "finding_id", "output_dir", "kind", "content"],
            "additionalProperties": false,
            "properties": {
                "run_id":     { "type": "string" },
                "finding_id": { "type": "string" },
                "output_dir": { "type": "string", "pattern": "^/.+" },
                "kind":       { "enum": ["agent_question", "user_answer"] },
                "content":    { "type": "string", "minLength": 1 },
                "meta":       { "type": "object" }
            }
        }
    })
}

fn finalize_verification_schema() -> Value {
    json!({
        "name": "finalize_verification",
        "description": "Stage 2c — Consume the user-ready signal. Rejects from state 'open' (no_clarification_round) or 'waiting_for_user' (unanswered_question) per spec §12.2. Computes sha256 over the canonical transcript bytes, writes stage-2.verified.json atomically, flips the session to 'finalized', and updates index.json with verified+stage2_path. No LLM call.",
        "inputSchema": {
            "type": "object",
            "required": ["run_id", "finding_id", "output_dir"],
            "additionalProperties": false,
            "properties": {
                "run_id":     { "type": "string" },
                "finding_id": { "type": "string" },
                "output_dir": { "type": "string", "pattern": "^/.+" }
            }
        }
    })
}

fn abort_verification_schema() -> Value {
    json!({
        "name": "abort_verification",
        "description": "Stage 2d — Kill a non-terminal session. Atomically rewrites stage-2.session.json with state 'aborted', aborted_at, and optional abort_reason. Does NOT touch index.json (aborted sessions are invisible to stage 3). A fresh start_verification after abort overwrites the session.",
        "inputSchema": {
            "type": "object",
            "required": ["run_id", "finding_id", "output_dir"],
            "additionalProperties": false,
            "properties": {
                "run_id":     { "type": "string" },
                "finding_id": { "type": "string" },
                "output_dir": { "type": "string", "pattern": "^/.+" },
                "reason":     { "type": "string" }
            }
        }
    })
}

fn index_codebase_schema() -> Value {
    json!({
        "name": "index_codebase",
        "description": "Stage 3a — Index a codebase. Walks the directory, parses source files with tree-sitter (Rust, Python, TypeScript), and persists a code-intelligence graph (nodes: functions, structs/classes, enums, traits/interfaces, etc.; edges: contains, defines, has_method, etc.) into a LadybugDB database at <output_dir>/graph/. Returns node/edge counts and elapsed time.",
        "inputSchema": {
            "type": "object",
            "required": ["path", "output_dir"],
            "additionalProperties": false,
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Absolute path to the codebase root to index."
                },
                "language": {
                    "type": "string",
                    "enum": ["auto", "rust", "python", "typescript", "java", "kotlin", "swift", "objc", "c", "cpp", "go"],
                    "default": "auto",
                    "description": "Language to parse. 'auto' detects per-file by extension (.rs, .py, .ts/.tsx, .java, .kt/.kts, .swift, .m/.mm, .c/.h, .cc/.cpp/.hpp, .go). Specific values restrict to that language only."
                },
                "output_dir": {
                    "type": "string",
                    "description": "Absolute directory where the graph will be stored (at <output_dir>/graph/)."
                }
            }
        }
    })
}

fn query_graph_schema() -> Value {
    json!({
        "name": "query_graph",
        "description": "Stage 3a — Execute a Cypher query against an indexed code graph. The graph must have been created by a prior index_codebase call. Returns column names and rows.",
        "inputSchema": {
            "type": "object",
            "required": ["graph_path", "query"],
            "additionalProperties": false,
            "properties": {
                "graph_path": {
                    "type": "string",
                    "description": "Absolute path to the graph directory (the path returned by index_codebase)."
                },
                "query": {
                    "type": "string",
                    "description": "Cypher query to execute against the graph."
                }
            }
        }
    })
}

fn get_symbol_schema() -> Value {
    json!({
        "name": "get_symbol",
        "description": "Stage 3a — Look up a symbol by qualified name in the code graph. Returns the node properties plus all incoming and outgoing edges. Qualified names follow the pattern 'file_path::symbol_name' (e.g., 'src/main.rs::handle_tool_call').",
        "inputSchema": {
            "type": "object",
            "required": ["graph_path", "qualified_name"],
            "additionalProperties": false,
            "properties": {
                "graph_path": {
                    "type": "string",
                    "description": "Absolute path to the graph directory."
                },
                "qualified_name": {
                    "type": "string",
                    "description": "The qualified name of the symbol to look up (e.g., 'src/main.rs::handle_tool_call')."
                }
            }
        }
    })
}

fn resolve_graph_schema() -> Value {
    json!({
        "name": "resolve_graph",
        "description": "Stage 3b — Resolve cross-file edges in the code graph. Runs AFTER index_codebase. Adds Imports, Calls, Implements, Extends, and Uses edges by matching string references to concrete target nodes. Returns resolution statistics including edge counts and resolution rate.",
        "inputSchema": {
            "type": "object",
            "required": ["graph_path"],
            "additionalProperties": false,
            "properties": {
                "graph_path": {
                    "type": "string",
                    "description": "Path to the graph directory (created by index_codebase)."
                }
            }
        }
    })
}

fn cluster_graph_schema() -> Value {
    json!({
        "name": "cluster_graph",
        "description": "Stage 3c — Run community detection and process tracing on an indexed+resolved graph. Groups symbols into functional communities via Louvain+C2 repair, detects entry points (main, test, handler, lib_entry), and traces BFS call chains to create Process nodes. Requires resolve_graph to have been called first.",
        "inputSchema": {
            "type": "object",
            "required": ["graph_path"],
            "additionalProperties": false,
            "properties": {
                "graph_path": {
                    "type": "string",
                    "description": "Path to the graph directory (created by index_codebase, resolved by resolve_graph)."
                },
                "resolution_param": {
                    "type": "number",
                    "default": 1.0,
                    "description": "Resolution parameter gamma for community detection. Higher = more, smaller communities. Default 1.0."
                }
            }
        }
    })
}

fn get_processes_schema() -> Value {
    json!({
        "name": "get_processes",
        "description": "Stage 3c — List all detected processes (execution flows from entry points). Each process has an entry point, entry kind (main/test/handler/lib_entry), BFS depth, and symbol count. Requires cluster_graph to have been called first.",
        "inputSchema": {
            "type": "object",
            "required": ["graph_path"],
            "additionalProperties": false,
            "properties": {
                "graph_path": {
                    "type": "string",
                    "description": "Path to the graph directory."
                }
            }
        }
    })
}

fn get_impact_schema() -> Value {
    json!({
        "name": "get_impact",
        "description": "Stage 3c — Blast radius analysis for a symbol. Returns which communities the symbol belongs to and which processes it participates in. Requires cluster_graph to have been called first.",
        "inputSchema": {
            "type": "object",
            "required": ["graph_path", "qualified_name"],
            "additionalProperties": false,
            "properties": {
                "graph_path": {
                    "type": "string",
                    "description": "Path to the graph directory."
                },
                "qualified_name": {
                    "type": "string",
                    "description": "The qualified name of the symbol to analyze (e.g., 'src/main.rs::handle_tool_call')."
                }
            }
        }
    })
}

fn search_codebase_schema() -> Value {
    json!({
        "name": "search_codebase",
        "description": "Stage 3d — Search the code graph by keyword. Returns ranked symbols with name, kind, file path, community, process participation, and relevance score. Use this to find symbols without knowing their exact qualified names. Requires index_codebase + resolve_graph + cluster_graph to have been called first (or use analyze_codebase for all-in-one).",
        "inputSchema": {
            "type": "object",
            "required": ["graph_path", "query"],
            "additionalProperties": false,
            "properties": {
                "graph_path": {
                    "type": "string",
                    "description": "Path to the graph directory."
                },
                "query": {
                    "type": "string",
                    "description": "Search query — one or more keywords (e.g., 'handle_tool', 'GraphStore', 'search result')."
                },
                "limit": {
                    "type": "integer",
                    "default": 20,
                    "description": "Maximum number of results to return."
                },
                "label_filter": {
                    "type": "string",
                    "enum": ["Function", "Method", "Struct", "Enum", "Trait", "Module", "Constant", "TypeAlias"],
                    "description": "Optional: only return symbols of this kind."
                }
            }
        }
    })
}

fn get_context_schema() -> Value {
    json!({
        "name": "get_context",
        "description": "Stage 3d — 360° symbol view. Returns the symbol plus ALL its relationships grouped by kind: what it imports, what imports it, what it calls, what calls it, what it implements, what implements it, community membership, and process participation. Richer than get_symbol — use this when you need full context for PRD generation or impact analysis.",
        "inputSchema": {
            "type": "object",
            "required": ["graph_path", "qualified_name"],
            "additionalProperties": false,
            "properties": {
                "graph_path": {
                    "type": "string",
                    "description": "Path to the graph directory."
                },
                "qualified_name": {
                    "type": "string",
                    "description": "The qualified name of the symbol (e.g., 'src/main.rs::handle_tool_call')."
                }
            }
        }
    })
}

fn analyze_codebase_schema() -> Value {
    json!({
        "name": "analyze_codebase",
        "description": "Stage 3 — All-in-one codebase analysis. Runs index_codebase + resolve_graph + cluster_graph in sequence, producing a fully searchable code graph in one call. Supports Rust, Python, and TypeScript (auto-detected by extension). Returns combined statistics from all phases.",
        "inputSchema": {
            "type": "object",
            "required": ["path", "output_dir"],
            "additionalProperties": false,
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Absolute path to the codebase root to index."
                },
                "language": {
                    "type": "string",
                    "enum": ["auto", "rust", "python", "typescript", "java", "kotlin", "swift", "objc", "c", "cpp", "go"],
                    "default": "auto",
                    "description": "Language to parse. 'auto' detects per-file by extension (.rs, .py, .ts/.tsx, .java, .kt/.kts, .swift, .m/.mm, .c/.h, .cc/.cpp/.hpp, .go). Specific values restrict to that language only."
                },
                "output_dir": {
                    "type": "string",
                    "description": "Absolute directory where the graph will be stored (at <output_dir>/graph/)."
                },
                "resolution_param": {
                    "type": "number",
                    "default": 1.0,
                    "description": "Resolution parameter for community detection. Higher = more, smaller communities."
                },
                "lsp": {
                    "type": "boolean",
                    "default": false,
                    "description": "Enable LSP-enhanced resolution after the static resolve pass. Requires the language server to be installed. Default: false."
                }
            }
        }
    })
}

fn lsp_resolve_schema() -> Value {
    json!({
        "name": "lsp_resolve",
        "description": "Stage 3b-v2 — LSP-enhanced resolution. Queries a Language Server Protocol server (rust-analyzer, pyright, typescript-language-server) to resolve method calls on inferred types that the static resolver cannot handle. Runs AFTER resolve_graph. Requires the LSP server to be installed; gracefully fails if not found.",
        "inputSchema": {
            "type": "object",
            "required": ["graph_path", "codebase_path"],
            "additionalProperties": false,
            "properties": {
                "graph_path": {
                    "type": "string",
                    "description": "Path to the graph directory (created by index_codebase)."
                },
                "codebase_path": {
                    "type": "string",
                    "description": "Absolute path to the codebase root."
                },
                "language": {
                    "type": "string",
                    "enum": ["auto", "rust", "python", "typescript", "java", "kotlin", "swift", "objc", "c", "cpp", "go"],
                    "default": "auto",
                    "description": "Language for LSP server selection. 'auto' detects from file extensions."
                },
                "lsp_command": {
                    "type": "string",
                    "description": "Override the LSP server command. Default: auto-detect (rust-analyzer, pyright, typescript-language-server)."
                },
                "timeout_ms": {
                    "type": "integer",
                    "default": 30000,
                    "description": "Total timeout in milliseconds for LSP resolution."
                }
            }
        }
    })
}

fn prepare_prd_input_schema() -> Value {
    json!({
        "name": "prepare_prd_input",
        "description": "Stage 4 — Bundle the verified stage-2 finding + graph intel (matched symbols, impacted communities, impacted processes, graph stats) into stage-4.prd_input.json. Read-only against the graph. Writes one JSON artifact under <output_dir>/runs/<run_id>/findings/<finding_id>/ and updates the run's index.json with stage4 markers. Consumed by the TypeScript PRD generator.",
        "inputSchema": {
            "type": "object",
            "required": ["run_id", "finding_id", "output_dir", "graph_path"],
            "additionalProperties": false,
            "properties": {
                "run_id":     { "type": "string" },
                "finding_id": { "type": "string" },
                "output_dir": { "type": "string", "pattern": "^/.+" },
                "graph_path": { "type": "string", "pattern": "^/.+" }
            }
        }
    })
}

fn validate_prd_against_graph_schema() -> Value {
    json!({
        "name": "validate_prd_against_graph",
        "description": "Stage 6 — Validate a PRD against the resolved+clustered graph. Three axes: (1) symbol hallucination — claimed symbols that don't exist (critical); (2) community-consistency — affected symbols spanning multiple Leiden communities (warning/critical); (3) process-impact contradiction — PRD claims 'does not affect X' while a changed symbol participates in X (critical). Contract-first on stage-5.affected_symbols.json with regex fallback from the PRD markdown. LLM-free. Read-only. When run_id+finding_id+output_dir are provided, writes stage-6.validation.json under findings/<finding_id>/.",
        "inputSchema": {
            "type": "object",
            "required": ["prd_path", "graph_path"],
            "additionalProperties": false,
            "properties": {
                "prd_path":    { "type": "string", "pattern": "^/.+" },
                "graph_path":  { "type": "string", "pattern": "^/.+" },
                "affected_symbols_path": { "type": "string", "pattern": "^/.+", "description": "Optional absolute path to stage-5.affected_symbols.json. When absent, regex fallback extracts claims from the PRD text." },
                "output_dir":  { "type": "string", "pattern": "^/.+", "description": "Optional; required together with run_id + finding_id to write stage-6.validation.json." },
                "run_id":      { "type": "string" },
                "finding_id":  { "type": "string" }
            }
        }
    })
}

fn check_security_gates_schema() -> Value {
    json!({
        "name": "check_security_gates",
        "description": "Stage 8 — Graph-aware security gates. Runs five checks on the changed_symbols list: S1 auth-critical community touch (critical), S2 unsafe-symbol touch (info-skip until parser records is_unsafe), S3 public-API surface change (warning), S4 unresolved-import introduction (warning/critical), S5 test-coverage structural gap (warning). Returns gates_passed=true iff zero critical flags. LLM-free. Read-only. When run_id+finding_id+output_dir are provided, writes stage-8.security.json.",
        "inputSchema": {
            "type": "object",
            "required": ["graph_path", "changed_symbols"],
            "additionalProperties": false,
            "properties": {
                "graph_path":      { "type": "string", "pattern": "^/.+" },
                "changed_symbols": { "type": "array", "items": { "type": "string" }, "minItems": 0 },
                "output_dir":      { "type": "string", "pattern": "^/.+", "description": "Optional; required together with run_id + finding_id to write stage-8.security.json." },
                "run_id":          { "type": "string" },
                "finding_id":      { "type": "string" }
            }
        }
    })
}

fn verify_semantic_diff_schema() -> Value {
    json!({
        "name": "verify_semantic_diff",
        "description": "Stage 9 — Compare a post-implementation graph against a pre-implementation graph to flag regressions: nodes added/removed, edges added/removed, dangling references (edges whose target disappeared), new unresolved imports, and new strongly-connected cycles. Returns a heuristic regression_score (cap 10.0, thresholds: <1 clean, <5 concerning, >=5 regression). Read-only against both graphs.",
        "inputSchema": {
            "type": "object",
            "required": ["before_graph_path", "after_graph_path"],
            "additionalProperties": false,
            "properties": {
                "before_graph_path": { "type": "string", "pattern": "^/.+" },
                "after_graph_path":  { "type": "string", "pattern": "^/.+" },
                "report_path":       { "type": "string", "pattern": "^/.+", "description": "Optional absolute path where the full report JSON is written. If absent, the report is returned inline only." }
            }
        }
    })
}

fn detect_changes_schema() -> Value {
    json!({
        "name": "detect_changes",
        "description": "Stage 3e — Git diff impact analysis. Maps changed lines to affected symbols, communities, and processes in the code graph. Accepts either raw unified diff text OR base_ref/head_ref to run git diff internally. Returns affected symbols with change type, community membership, process participation, and a heuristic risk score (0.0-1.0).",
        "inputSchema": {
            "type": "object",
            "required": ["graph_path"],
            "additionalProperties": false,
            "properties": {
                "graph_path": {
                    "type": "string",
                    "description": "Path to the graph directory (created by index_codebase or analyze_codebase)."
                },
                "diff_text": {
                    "type": "string",
                    "description": "Raw unified diff text. Mutually exclusive with base_ref/head_ref."
                },
                "codebase_path": {
                    "type": "string",
                    "description": "Absolute path to the git repository. Required when using base_ref/head_ref."
                },
                "base_ref": {
                    "type": "string",
                    "default": "HEAD~1",
                    "description": "Git ref for the base (e.g., 'HEAD~1', 'main', a commit hash)."
                },
                "head_ref": {
                    "type": "string",
                    "default": "HEAD",
                    "description": "Git ref for the head (e.g., 'HEAD', a branch name)."
                }
            }
        }
    })
}
