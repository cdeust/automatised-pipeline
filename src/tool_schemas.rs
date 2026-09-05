// tool_schemas — MCP tool JSON Schema definitions.
//
// Pure data: every function returns a `serde_json::Value` containing the
// JSON-RPC tool schema. No logic, no I/O. Extracted from main.rs when
// tools_list() exceeded 200 LOC (NOTES.md growth rule).

use serde_json::{json, Value};

#[path = "tool_schemas_shared_params.rs"]
mod shared_params;

#[path = "tool_schemas_index_codebase.rs"]
mod index_codebase;
use index_codebase::index_codebase_schema;

#[path = "tool_schemas_analyze_codebase.rs"]
mod analyze_codebase;
use analyze_codebase::analyze_codebase_schema;

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
            index_status_schema(),
            ingest_traces_schema(),
            query_graph_schema(),
            get_symbol_schema(),
            resolve_graph_schema(),
            cluster_graph_schema(),
            get_processes_schema(),
            get_impact_schema(),
            index_history_schema(),
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

/// The canonical one-line summary of a tool: the first sentence of its
/// `tools_list()` description. This is the SINGLE source of truth for what a
/// tool does — the prompts layer (`mcp_prompts`) composes workflow text from
/// this rather than hand-copying descriptions, so adding or renaming a tool
/// cannot leave two descriptions that disagree (§1.2 / issue #65 criterion 3).
///
/// Precondition: `name` is a candidate tool name (any string).
/// Postcondition: returns `Some(first_sentence)` iff a tool with that exact
/// name is registered in `tools_list()`; `None` otherwise. The returned string
/// is the description text up to and including the first ". " boundary (or the
/// whole description when it contains no such boundary).
pub fn tool_summary(name: &str) -> Option<String> {
    let payload = tools_list();
    let tools = payload.get("tools").and_then(Value::as_array)?;
    let description = tools
        .iter()
        .find(|tool| tool.get("name").and_then(Value::as_str) == Some(name))
        .and_then(|tool| tool.get("description").and_then(Value::as_str))?;
    // First sentence: text up to and including the first ". " boundary. AP's
    // descriptions open with a stage tag then a sentence (e.g. "Stage 3a — ….")
    // which is exactly the orientation a workflow prompt needs.
    let first = match description.split_once(". ") {
        Some((head, _)) => format!("{head}."),
        None => description.to_string(),
    };
    Some(first)
}

fn health_check_schema() -> Value {
    json!({
        "name": "health_check",
        "description": "Stage 0 — Handshake and liveness probe. CALL THIS FIRST, before any other tool, to confirm the MCP is live and to learn which tool profile is active (core vs full) so you know which tools are registered this session. Returns: server identity + version, the MCP protocol version, and the count of registered tools. Needs no graph and no arguments. This is a cheap read-only sanity check, not a data tool — for a graph's node/edge counts and indexing coverage use index_status instead.",
        "annotations": { "readOnlyHint": true },
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
        "annotations": { "destructiveHint": true },
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
        "annotations": { "destructiveHint": true },
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
        "annotations": { "destructiveHint": true },
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
        "annotations": { "destructiveHint": true },
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
        "annotations": { "destructiveHint": true },
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
        "annotations": { "destructiveHint": true },
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

fn ingest_traces_schema() -> Value {
    json!({
        "name": "ingest_traces",
        "description": "Stage 3 — Fold RUNTIME caller→callee observations (from OTel spans, a profiler, or coverage traces) into the graph (issue #58): static analysis + runtime truth. For each trace it either ANNOTATES the matching static Calls edge with observed_count (the call happened, and static resolution already knew it — now weighted by real traffic), or CREATES an OBSERVED_CALLS edge where static resolution found NO edge (the divergence signal — a call that really happens but the static analyzer missed, e.g. via dynamic dispatch/reflection). Endpoints that are not Function/Method nodes are reported as unresolved. Enterprise uses: dead-code claims backed by production ABSENCE (a symbol with zero observed calls), and hot paths weighted by real traffic. Response: {matched, unmatched_created, unresolved_names} + a capped list of the created divergences and unresolved names. Query the results via query_graph: `MATCH ()-[r:OBSERVED_CALLS_Function_Function]->() RETURN r` for divergences, or Calls_* edges' observed_count for weighted hot paths.",
        "annotations": { "destructiveHint": true },
        "inputSchema": {
            "type": "object",
            "required": ["graph_path", "traces"],
            "additionalProperties": false,
            "properties": {
                "graph_path": {
                    "type": "string",
                    "description": "Absolute path to the graph directory (must be indexed + resolved so Calls edges exist to match against)."
                },
                "traces": {
                    "type": "array",
                    "description": "Runtime caller→callee observations. Each item is {caller, callee, count?} where caller/callee are qualified names ('file::symbol', e.g. 'src/main.rs::handle') and count is the observed call frequency (default 1). Repeated pairs are summed.",
                    "items": {
                        "type": "object",
                        "required": ["caller", "callee"],
                        "additionalProperties": false,
                        "properties": {
                            "caller": { "type": "string", "description": "Qualified name of the calling Function/Method." },
                            "callee": { "type": "string", "description": "Qualified name of the called Function/Method." },
                            "count": { "type": "integer", "minimum": 1, "default": 1, "description": "Observed call count (real traffic weight)." }
                        }
                    }
                }
            }
        }
    })
}

fn index_status_schema() -> Value {
    json!({
        "name": "index_status",
        "description": "Stage 3a — Report an indexed graph's status and its indexing-COVERAGE (issue #57): node/edge counts, files indexed, and which files the indexer could NOT fully cover — 'parse_incomplete' (WERE indexed, but tree-sitter left ERROR/MISSING line ranges, so constructs there MAY be missing from the graph — grep those ranges), 'skipped' (not indexed at all: oversized/read-failure/parse-timeout), and 'quarantined' (the parser panicked; isolated so it could not kill the index). Counts are exact; example lists are capped (full lists in the index_coverage.json sidecar). IMPORTANT: absence of a flag is NOT a completeness guarantee — the signal only marks what the indexer can detect. Use before trusting graph completeness on a file; for a structural view of the misses use query_graph(graph=\"missed\").",
        "annotations": { "readOnlyHint": true },
        "inputSchema": {
            "type": "object",
            "required": ["graph_path"],
            "additionalProperties": false,
            "properties": {
                "graph_path": {
                    "type": "string",
                    "description": "Absolute path to the graph directory (the path returned by index_codebase)."
                }
            }
        }
    })
}

fn query_graph_schema() -> Value {
    json!({
        "name": "query_graph",
        "description": "Stage 3a — Run a read-only Cypher query against the code graph: the ESCAPE HATCH for structural questions the typed tools do not cover (arbitrary MATCH patterns, aggregations, multi-hop traversals). USE THIS INSTEAD OF scripting grep across files for structural facts like 'which functions call X and live in module Y'. Read-only: mutation keywords are rejected. Response: 'columns' + 'rows' (rows as compact arrays), a human-readable 'result' table, 'total_count', 'order_stable', and 'next_offset' — page a large result by re-calling with offset=next_offset, but paging is cursor-safe ONLY when your query declares ORDER BY (order_stable reports whether it does). TOKEN SURFACE (issue #56): format='tabular' drops the redundant human 'result' string (rows+columns already carry the data); detail='ids' collapses to the first column's values for a cheap sweep. COVERAGE HONESTY (issue #57): pass graph=\"missed\" to enumerate what the index does NOT cover (parse-incomplete + skipped + quarantined files) so you know where to prefer grep. IMPORTANT: absence from graph results — or from the 'missed' list — is NOT a completeness guarantee; the signal only marks what the indexer can detect.",
        "annotations": { "readOnlyHint": true },
        "inputSchema": {
            "type": "object",
            "required": ["graph_path"],
            "additionalProperties": false,
            "properties": {
                "graph_path": {
                    "type": "string",
                    "description": "Absolute path to the graph directory (the path returned by index_codebase)."
                },
                "graph": {
                    "type": "string",
                    "enum": ["default", "missed"],
                    "default": "default",
                    "description": "Which view to query. 'default': run the Cypher 'query' against the code graph. 'missed' (issue #57): ignore 'query' and return the coverage report — the files the index could NOT fully cover (parse_incomplete with flagged line ranges, skipped, quarantined), so an agent can pivot to grep. Absence from the missed list is NOT a completeness guarantee."
                },
                "query": {
                    "type": "string",
                    "description": "Cypher query to execute against the graph. Required unless graph=\"missed\"."
                },
                "offset": {
                    "type": "integer",
                    "minimum": 0,
                    "default": 0,
                    "description": "Number of result rows to skip before filling the byte budget (cursor pagination). Page through a large result by re-calling with offset = the previous response's next_offset until next_offset is absent. IMPORTANT: paging is only cursor-safe when your query declares an ORDER BY — the response reports order_stable; without ORDER BY the row order is unspecified and pages may skip or duplicate rows."
                },
                "detail": detail_param(),
                "format": {
                    "type": "string",
                    "enum": ["json", "tabular"],
                    "default": "json",
                    "description": "Token surface (issue #56). query_graph already returns 'rows' as compact arrays with 'columns' declared once. 'json' (default) additionally includes a human-readable 'result' text table. 'tabular' OMITS that 'result' string (the rows+columns already carry the data), cutting tokens. detail='ids' collapses to the first column's values."
                }
            }
        }
    })
}

fn get_symbol_schema() -> Value {
    json!({
        "name": "get_symbol",
        "description": "Stage 3a — Exact symbol lookup by qualified name. USE THIS INSTEAD OF opening a file and reading around a definition: given a qualified name ('file_path::symbol_name', e.g. 'src/main.rs::handle_tool_call'), it returns the node's properties (kind, visibility, line span, language) plus EVERY incoming and outgoing edge (defines, calls, imports, implements, …) — the symbol's immediate neighborhood in one call. If you have a keyword but not the qualified name, run search_codebase first; for a grouped 360° view use get_context; for reverse-dependency blast radius use get_impact. Response: the node object + its edge lists. COVERAGE CAVEAT (issue #57): if the symbol is not found it may be UNINDEXED (its file parse-incomplete or skipped) rather than nonexistent — check index_status before concluding it does not exist. STALENESS (fleet-watch#112): the response's 'graph_freshness' object ('state': fresh|stale|unknown, plus dirty_files/checked_files/commits_behind/commits_ahead) tells you whether the working tree has moved since this graph was indexed — in EITHER direction: 'commits_ahead' > 0 means HEAD sits BEHIND the indexed commit (a checkout to an older revision), which makes the graph just as stale as a forward move. It rides on every response this tool returns, its not-found and error envelopes included — there it is what separates 'this does not exist' from 'this graph predates it'. Distinct key from index_codebase's string-valued 'graph_state'; do not type the two as one field.",
        "annotations": { "readOnlyHint": true },
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
                },
                "sibling_graphs": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional cross-repo bridge: paths to sibling repo graph directories. When the symbol is not defined in this graph, the bridge looks for its definition in these siblings and returns repo-tagged foreign_definitions (re-query the owning repo via the returned `repo`). Omit for single-repo lookups."
                }
            }
        }
    })
}

fn resolve_graph_schema() -> Value {
    json!({
        "name": "resolve_graph",
        "description": "Stage 3b — Resolve cross-file edges in the code graph. Runs AFTER index_codebase. Adds Imports, Calls, Implements, Extends, and Uses edges by matching string references to concrete target nodes. Returns resolution statistics including edge counts and resolution rate.",
        "annotations": { "destructiveHint": true },
        "inputSchema": {
            "type": "object",
            "required": ["graph_path"],
            "additionalProperties": false,
            "properties": {
                "graph_path": {
                    "type": "string",
                    "description": "Path to the graph directory (created by index_codebase)."
                },
                "sibling_graphs": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional cross-repo bridge: paths to sibling repo graph directories. Reports cross_repo_resolvable — how many of this graph's unresolved references a sibling repo can define (a cross-service edge rather than a true third-party dependency) — plus a sample. Omit for single-repo resolution."
                }
            }
        }
    })
}

fn cluster_graph_schema() -> Value {
    json!({
        "name": "cluster_graph",
        "description": "Stage 3c — Run community detection and process tracing on an indexed+resolved graph. Groups symbols into functional communities via Louvain+C2 repair, detects entry points (main, test, proof, handler, lib_entry; proof denotes a source harness, not a verified result), and traces BFS call chains to create Process nodes. Requires resolve_graph to have been called first.",
        "annotations": { "destructiveHint": true },
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
        "description": "Stage 3c — List all detected processes (execution flows from entry points). Each process has an entry point, entry kind (main/test/proof/handler/lib_entry; proof denotes a source harness, not a verified result), BFS depth, and symbol count. Requires cluster_graph to have been called first.",
        "annotations": { "readOnlyHint": true },
        "inputSchema": {
            "type": "object",
            "required": ["graph_path"],
            "additionalProperties": false,
            "properties": {
                "graph_path": {
                    "type": "string",
                    "description": "Path to the graph directory."
                },
                "offset": {
                    "type": "integer",
                    "minimum": 0,
                    "default": 0,
                    "description": "Number of processes to skip before filling the byte budget (cursor pagination). Processes are returned in a stable total order (name, then entry_point). Page through them all by re-calling with offset = the previous response's next_offset until next_offset is absent."
                },
                "detail": detail_param(),
                "format": format_param(),
                "sibling_graphs": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Accepted for cross-repo API symmetry but NOT acted on: a Process is an intra-graph execution flow (BFS over one graph's call edges), and the bridge topology forbids the super-graph merge that a cross-repo process would require. To trace a flow across repos, follow get_impact's foreign_callers / get_symbol's foreign_definitions into the sibling graph and call get_processes there."
                }
            }
        }
    })
}

fn get_impact_schema() -> Value {
    json!({
        "name": "get_impact",
        "description": "Stage 3c — Reverse-dependency blast radius for a symbol. USE THIS INSTEAD OF grepping a name across the repo to find who depends on it: it returns callers (reverse Calls), importers (reverse Imports), users (reverse Uses), and implementors (reverse Implements), each a re-queryable {id, qualified_name, label, confidence} handle you traverse further via get_symbol/get_context/query_graph — plus the communities and processes affected. Cursor: 'callers' is the PRIMARY paged list (page via next_offset, stable order); importers/users/implementors are byte-capped SUMMARIES from index 0 (secondary_lists_paged=false) — page one at scale via query_graph with ORDER BY. 'dependents_total' is the true pre-truncation size. TOKEN SURFACE (issue #56): detail='ids' → bare qualified names across all four sections; format='tabular' → rows-as-arrays under one 'columns' header. TEMPORAL (issue #58): 'cochange_partners' lists the files that historically change WITH this symbol's file (FILE_CHANGES_WITH, strongest coupling first) — impact candidates the static call graph cannot see (the architect agent's churning-pairs signal). EPISTEMIC HONESTY: 'epistemic'='lower-bound' when the target is reached via dynamic dispatch or heuristically-resolved edges — real impact may exceed what is shown; 'epistemic_reasons' names the carriers. STALENESS (fleet-watch#112): the response's 'graph_freshness' object ('state': fresh|stale|unknown, plus dirty_files/checked_files/commits_behind/commits_ahead) tells you whether the working tree has moved since this graph was indexed — in EITHER direction: 'commits_ahead' > 0 means HEAD sits BEHIND the indexed commit (a checkout to an older revision), which makes the graph just as stale as a forward move. It rides on every response this tool returns, its not-found and error envelopes included — there it is what separates 'this does not exist' from 'this graph predates it'. Distinct key from index_codebase's string-valued 'graph_state'; do not type the two as one field — a 'stale' state means treat this blast radius as a lower-confidence snapshot. Prereqs: a resolved graph (cluster_graph adds the community/process fields).",
        "annotations": { "readOnlyHint": true },
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
                },
                "offset": {
                    "type": "integer",
                    "minimum": 0,
                    "default": 0,
                    "description": "Number of CALLERS to skip before filling the byte budget (cursor pagination). 'callers' is the PRIMARY paged list, ordered by a stable total order (qualified_name, then id); page through every caller via next_offset. The other reverse-dependency lists (importers, users, implementors) are byte-capped SUMMARIES starting at index 0, not cursored (secondary_lists_paged=false) — to page one of those at scale, query it directly via query_graph with an explicit ORDER BY."
                },
                "detail": detail_param(),
                "format": format_param(),
                "sibling_graphs": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional cross-repo bridge: paths to sibling repo graph directories. Adds a foreign_callers section — callers of this symbol that live in OTHER repos, repo-tagged and kept separate from the local callers (and from dependents_total) so blast radius distinguishes local from cross-repo impact. Foreign callers are name-matched without a shared linker (confidence 0.50) and make the blast radius a lower bound. Omit for single-repo impact."
                }
            }
        }
    })
}

fn index_history_schema() -> Value {
    json!({
        "name": "index_history",
        "description": "History layer — ingests git commit history into an already-indexed graph as a traversable version spine (not a flat diff report). Creates Commit nodes with author/timestamp/message, PreviousVersion commit ancestry, and a Version node per (entity, commit) for every File and symbol a commit changed — linked by ChangedIn (version→commit) and VersionOf (version→entity), and chained by PreviousVersion (version→prior version). Lets a consumer walk: entity ← VersionOf ← Version → ChangedIn → Commit → PreviousVersion → Commit, and the reverse. File attribution is exact; symbol attribution maps changed lines onto the current graph's symbol ranges (best-effort on older commits). Call after index_codebase + resolve_graph on the same graph_path.",
        "annotations": { "destructiveHint": true },
        "inputSchema": {
            "type": "object",
            "required": ["graph_path", "codebase_path"],
            "additionalProperties": false,
            "properties": {
                "graph_path": {
                    "type": "string",
                    "description": "Path to the graph directory (must already be indexed)."
                },
                "codebase_path": {
                    "type": "string",
                    "description": "Path to the git working tree that was indexed. Must be inside a git repository."
                },
                "max_commits": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Maximum number of commits to walk, newest first. Defaults to 200."
                }
            }
        }
    })
}

fn search_codebase_schema() -> Value {
    json!({
        "name": "search_codebase",
        "description": "Stage 3d — Ranked keyword search over the code graph. USE THIS INSTEAD OF grep/ripgrep when you know a name or concept but not the exact qualified name: it ranks symbols by hybrid lexical+structural relevance and returns structural context grep cannot — kind, file path, community, process participation, score. Response: 'results' (ranked, cursor-paged), 'total_count', 'by_process' (results grouped by execution flow), and 'next_offset' when more remain — page by re-calling with offset=next_offset until it is absent (stable order: score desc, then qualified_name). TOKEN SURFACE (issue #56): detail='ids' returns only qualified names for a cheap wide sweep before drilling in; format='tabular' streams rows as arrays under a one-line 'columns' header. Narrow with label_filter (Function/Method/Struct/…). COVERAGE CAVEAT (issue #57): a symbol absent from results may simply be UNINDEXED — the graph can be parse-incomplete; if a negative result matters, check index_status / query_graph(graph=\"missed\") and grep the flagged files. DOC CONTENT (fleet-watch#112): also searches the full text of markdown/plain-text/similar doc files (label 'File' in results, kind of hit rather than a symbol) — so a query whose only evidence lives in README/skill/doc prose still returns something, not just code symbols. Built by analyze_codebase's search-index phase; a graph built without it (bare index_codebase/resolve_graph/cluster_graph) falls back to substring search over symbols only. DOC RECALL IS LEXICAL ONLY: doc bodies are indexed by BM25 but NOT by the semantic vector half, so a doc is found by words it actually contains — a query that matches it in meaning but shares no term with it will not surface it, even though a symbol can be found that way STALENESS (fleet-watch#112): the response's 'graph_freshness' object ('state': fresh|stale|unknown, plus dirty_files/checked_files/commits_behind/commits_ahead) tells you whether the working tree has moved since this graph was indexed — in EITHER direction: 'commits_ahead' > 0 means HEAD sits BEHIND the indexed commit (a checkout to an older revision), which makes the graph just as stale as a forward move. It rides on every response this tool returns, its not-found and error envelopes included — there it is what separates 'this does not exist' from 'this graph predates it'. Distinct key from index_codebase's string-valued 'graph_state'; do not type the two as one field. Prereqs: index_codebase + resolve_graph + cluster_graph (or analyze_codebase for all-in-one).",
        "annotations": { "readOnlyHint": true },
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
                    "description": "Maximum number of ranked candidate results to consider. Bounds the universe the cursor pages within."
                },
                "offset": {
                    "type": "integer",
                    "minimum": 0,
                    "default": 0,
                    "description": "Number of ranked results to skip before filling the byte budget (cursor pagination). Results are in a stable total order: descending score, then ascending qualified_name. Page through them via next_offset until it is absent."
                },
                "label_filter": {
                    "type": "string",
                    "enum": ["Function", "Method", "Struct", "Enum", "Trait", "Module", "Constant", "TypeAlias", "File"],
                    "description": "Optional: only return symbols of this kind. 'File' (fleet-watch#112) restricts to doc/prose content hits — markdown, plain text, and similar files the parser does not turn into symbols — rather than code. 'File' REQUIRES the search index built by analyze_codebase: doc text lives only in that index, never in the graph, so on a graph built without it this value is refused with an explanatory error rather than returning an empty result you could mistake for 'no doc matched'."
                },
                "detail": detail_param(),
                "format": format_param(),
                "sibling_graphs": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional cross-repo bridge: paths to sibling repo graph directories. Federates the query across them and returns a repo-tagged foreign_results section, kept separate from the primary cursored results so local pagination stays exact. Omit for single-repo search."
                }
            }
        }
    })
}

fn get_context_schema() -> Value {
    json!({
        "name": "get_context",
        "description": "Stage 3d — 360° symbol view: the symbol plus ALL its relationships grouped and labeled by kind — imports / imported-by, calls / called-by, implements / implemented-by, community membership, process participation. USE THIS INSTEAD OF reading a file and manually tracing what a symbol touches; it is the richest single-symbol tool (get_symbol returns raw edges, this groups them). Ideal for PRD generation, review prep, and understanding a symbol before changing it. Input: a qualified name ('file_path::symbol_name'); run search_codebase first if you only have a keyword, or get_impact for the reverse-dependency blast radius. COVERAGE CAVEAT (issue #57): grouped relationships are only as complete as the graph — a caller in a parse-incomplete file will be absent; verify negatives with index_status or query_graph(graph=\"missed\").",
        "annotations": { "readOnlyHint": true },
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

fn lsp_resolve_schema() -> Value {
    json!({
        "name": "lsp_resolve",
        "description": "Stage 3b-v2 — LSP-enhanced resolution. Queries a Language Server Protocol server (rust-analyzer, pyright, typescript-language-server) to resolve method calls on inferred types that the static resolver cannot handle. Runs AFTER resolve_graph. Requires the LSP server to be installed; gracefully fails if not found.",
        "annotations": { "destructiveHint": true },
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
        "description": "Stage 4 — Bundle graph intel (matched symbols, impacted communities, impacted processes, graph stats) into stage-4.prd_input.json for the PRD generator. Read-only against the graph. TWO modes: (1) FINDING mode — pass finding_id to bundle a VERIFIED stage-2 finding (writes under runs/<run_id>/findings/<finding_id>/ and updates index.json); (2) FEATURE mode — pass feature_description (no finding_id) to ground a free-text feature directly on the code graph, skipping the stage-2 gate (writes under runs/<run_id>/features/<slug>/). Provide finding_id OR feature_description. Grounding trust (v1.1.0, issue #14): each `matched_symbols` entry carries `match_mode` ('verbatim' — identifier cited in backticks and resolved exactly; 'exact_name' — a description word equals the symbol's name exactly) and `confidence`; only these are counted as verified grounding. Substring/fuzzy hits with no exact-identity evidence are returned separately in `candidate_symbols` (same shape, `match_mode: 'lexical'`) and are NEVER folded into matched_symbols or into impacted_communities/impacted_processes — an empty matched_symbols is expected and correct when the description contains no cited or exactly-named identifiers. Cite identifiers in backticks in finding/feature descriptions to get verbatim-priority grounding.",
        "annotations": { "destructiveHint": true },
        "inputSchema": {
            "type": "object",
            "required": ["output_dir", "graph_path"],
            "additionalProperties": false,
            "properties": {
                "run_id":              { "type": "string", "description": "Pipeline run id (path segment). Defaults to 'adhoc' in feature mode." },
                "finding_id":          { "type": "string", "description": "Finding mode: the verified stage-2 finding to bundle. Omit for feature mode." },
                "feature_description": { "type": "string", "description": "Feature mode: free-text feature/intent to ground on the graph. Used when finding_id is absent." },
                "output_dir":          { "type": "string", "pattern": "^/.+" },
                "graph_path":          { "type": "string", "pattern": "^/.+" }
            }
        }
    })
}

fn validate_prd_against_graph_schema() -> Value {
    json!({
        "name": "validate_prd_against_graph",
        "description": "Stage 6 — Validate a PRD against the resolved+clustered graph. Three axes: (1) symbol hallucination — claimed symbols that don't exist (critical); (2) community-consistency — affected symbols spanning multiple Leiden communities (warning/critical); (3) process-impact contradiction — PRD claims 'does not affect X' while a changed symbol participates in X (critical). Contract-first on stage-5.affected_symbols.json with regex fallback from the PRD markdown. LLM-free. Read-only. When run_id+finding_id+output_dir are provided, writes stage-6.validation.json under findings/<finding_id>/.",
        "annotations": { "readOnlyHint": true },
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
        "annotations": { "readOnlyHint": true },
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
        "annotations": { "readOnlyHint": true },
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
        "description": "Stage 3e — Git-diff impact analysis: maps changed lines to the affected symbols, communities, and processes, so you see a diff's blast radius without reading it line by line. USE THIS INSTEAD OF eyeballing a diff to guess what a change touches. Input: either raw unified-diff text, or base_ref/head_ref to run git diff internally. Returns affected symbols with change type (added/modified/deleted), community and process membership, and a heuristic risk score (0.0–1.0) per symbol. Follow up with get_impact on a high-risk symbol to expand its reverse-dependency blast radius. COVERAGE CAVEAT (issue #57): symbols in parse-incomplete files may be missing from the mapping — verify with index_status when a change lands in a flagged file.",
        "annotations": { "readOnlyHint": true },
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

/// The `detail` token-surface parameter (issue #56), shared by list-returning
/// tools. `full` (default) is unchanged behavior; `ids` returns bare identifiers.
fn detail_param() -> Value {
    json!({
        "type": "string",
        "enum": ["full", "ids"],
        "default": "full",
        "description": "Token surface (issue #56). 'full' (default): each result as a complete object. 'ids': return ONLY the bare identifiers (qualified names) plus the total — a cheap wide sweep to enumerate what exists before drilling into specific symbols with get_symbol/get_context. Overrides 'format' (an id list needs no table)."
    })
}

/// The `format` token-surface parameter (issue #56), shared by list-returning
/// tools. `json` (default) is objects; `tabular` streams rows as arrays.
fn format_param() -> Value {
    json!({
        "type": "string",
        "enum": ["json", "tabular"],
        "default": "json",
        "description": "Token surface (issue #56). 'json' (default): results as objects. 'tabular': declare the columns ONCE (in the response's 'columns' header) and stream each result as an array of cells in that column order — homogeneous result sets stop repeating field names, cutting tokens. Read each row positionally against 'columns'. Ignored when detail='ids'."
    })
}
