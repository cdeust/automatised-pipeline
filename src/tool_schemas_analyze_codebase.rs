//! `analyze_codebase`'s tool schema.
//!
//! Second file in the per-tool split `tool_schemas.rs` is being broken into
//! (review round 2, finding 5). At 55 lines this schema is over §4.2's
//! function cap for the same reason `index_codebase`'s was: it is one JSON
//! literal documenting a behavioural contract, and the honest fix is to stop
//! packing every tool into one 900-line file rather than to compress the prose
//! a caller depends on.

use super::shared_params::{dependency_scope_param, include_dependencies_param};
use serde_json::{json, Value};

pub(super) fn analyze_codebase_schema() -> Value {
    json!({
        "name": "analyze_codebase",
        "description": "Stage 3 — All-in-one: runs index_codebase + resolve_graph + cluster_graph in sequence, producing a fully searchable, resolved, clustered graph in ONE call. USE THIS FIRST on a new repo instead of calling the three stages separately. Auto-detects language by extension (Rust, Python, TypeScript, Java, Kotlin, Swift, Obj-C, C, C++, Go). Returns combined statistics from every phase (nodes/edges, resolution counts, communities/processes) AND a coverage report (issue #57) listing files that were parse-incomplete / skipped / quarantined. COVERAGE CAVEAT: absence of a flag is NOT a completeness guarantee — before trusting a negative graph result on a specific file, consult index_status or query_graph(graph=\"missed\") and grep the flagged files/ranges. Afterward, explore with search_codebase / get_context / get_impact / query_graph; re-run after edits (indexing is incremental by default).",
        "annotations": { "destructiveHint": true },
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
                    "description": "Enable LSP-enhanced resolution after the static resolve pass. Required for inferred Rust receiver calls; the language server must be installed. Default: false. Response lsp_status distinguishes disabled, completed, and failed (with error; analysis continues on the available graph, which may include partial LSP results). Completed does not imply all sites resolved; lsp_resolve retains counts. The resolve receipt describes the static phase only."
                },
                "dependency_scope": dependency_scope_param(),
                "include_dependencies": include_dependencies_param(),
                "exclude_dirs": exclude_dirs_param()
            }
        }
    })
}

/// `analyze_codebase`'s own text, which differs from `index_codebase`'s:
/// that tool documents a `full=true` caveat this one has no equivalent for.
fn exclude_dirs_param() -> Value {
    json!({
                    "type": "array",
                    "items": { "type": "string" },
                    "default": [],
                    "description": "Issue #249 — directory paths (relative to 'path', no leading '/' or '..') or bare directory names to prune from the walk, in addition to the built-in build/dependency skip list. An entry WITHOUT a path separator (e.g. \"secrets\") is a bare name matched anywhere in the tree, like the built-in list; an entry WITH one (e.g. \"config/secrets\") matches exactly that one subtree relative to 'path'. No glob support. Exclusion WINS over every 'dependency_scope' tier, including 'full' — this is for directories that must never be read, not a performance prune. Pruned directories are NEVER silently dropped: each is reported in the coverage sidecar as skipped (reason 'user_excluded'), and the response's coverage.skipped.user_excluded_count carries the total. A directory the OS refuses to read (permission denied) is handled independently and automatically — see the coverage 'unreadable' reason — even without listing it here."
    })
}
