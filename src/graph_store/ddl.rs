// graph_store::ddl — CREATE NODE/REL TABLE DDL string generation.
//
// Extracted from graph_store.rs (Fowler "Move Function") to keep the file
// under the §4.1 cap. Pure move; `use super::*` provides the shared store
// vocabulary exactly as when this lived in one module.

use super::*;

const NODE_TABLE_SCHEMAS: &[(&str, &str)] = &[
        // source: stages/stage-3.md §schema
        (NODE_DIRECTORY, "id STRING, path STRING, name STRING"),
        // source: stages/stage-3.md §10.5 — `parse_errors` records the count of
        // tree-sitter ERROR/MISSING nodes for this file's parse. A file that
        // parses to few/zero symbols with parse_errors > 0 is a degraded parse
        // (e.g. wrong grammar dialect), not a genuinely empty file; downstream
        // tools must be able to tell the two apart.
        (NODE_FILE, "id STRING, path STRING, name STRING, extension STRING, size_bytes INT64, parse_errors INT64"),
        (NODE_MODULE, "id STRING, name STRING, qualified_name STRING"),
        // source: Spike B' BUG #5 fix — every symbol-bearing node gets a
        // `language` STRING column populated by the indexer from the file's
        // extension (python/rust/typescript). Previously every symbol came
        // back with `language: None` in the JSON dump.
        // source: issue #92 — `return_type` and `constructed_types` carry the
        // function's return-type annotation and the space-joined set of types it
        // constructs; resolve_uses reads both to emit Uses_Function_<Type> edges.
        // Empty ("") for languages that have not adopted the extraction.
        (NODE_FUNCTION,
            "id STRING, name STRING, qualified_name STRING, \
             start_line INT64, end_line INT64, visibility STRING, is_async BOOLEAN, \
             return_type STRING, constructed_types STRING, language STRING, entry_kind STRING"),
        // source: implements fix — `trait_name` carries the trait a method
        // belongs to in an `impl Trait for Type` block (already extracted by
        // the parser at parser/rust.rs but previously dropped for lack of a
        // column). resolve_implements reads it to emit the Type→Trait edge.
        // source: issue #92 — `return_type`/`constructed_types` as on Function.
        (NODE_METHOD,
            "id STRING, name STRING, qualified_name STRING, \
             start_line INT64, end_line INT64, visibility STRING, is_async BOOLEAN, \
             receiver_type STRING, trait_name STRING, return_type STRING, \
             constructed_types STRING, language STRING"),
        // source: Spike B' BUG #9 fix — `bases STRING` column carries a CSV
        // of unresolved base-class names emitted by the parser. The resolver
        // reads this in resolve_extends, looks each name up in the symbol
        // index, and emits the resolved Extends_X_Y edges. Indexer can't
        // route Extends refs directly because their to_qualified_name is a
        // raw NAME (e.g., "Animal"), not a QN — name→QN resolution happens
        // server-side in the resolver pass after all nodes are indexed.
        //
        // source: implements fix — `implements STRING` is the same mechanism
        // for the implemented-trait/interface names (`#[derive(...)]`, Java
        // `implements`). resolve_implements resolves each name to a local
        // Trait or a stdlib trait. Trait carries the column for schema
        // uniformity but never populates it (a trait implements nothing).
        (NODE_STRUCT,
            "id STRING, name STRING, qualified_name STRING, \
             start_line INT64, end_line INT64, visibility STRING, language STRING, \
             bases STRING, implements STRING"),
        (NODE_ENUM,
            "id STRING, name STRING, qualified_name STRING, \
             start_line INT64, end_line INT64, visibility STRING, language STRING, \
             bases STRING, implements STRING"),
        // source: stages/stage-3.md §10.1 — every symbol carries its source
        // span. The parser already emits start_line/end_line for these nodes;
        // the columns were previously missing so the spans were dropped at persist.
        (NODE_VARIANT,
            "id STRING, name STRING, qualified_name STRING, \
             start_line INT64, end_line INT64, language STRING"),
        (NODE_TRAIT,
            "id STRING, name STRING, qualified_name STRING, \
             start_line INT64, end_line INT64, visibility STRING, language STRING, \
             bases STRING, implements STRING"),
        (NODE_FIELD,
            "id STRING, name STRING, type_annotation STRING, visibility STRING, \
             start_line INT64, end_line INT64, language STRING"),
        (NODE_CONSTANT,
            "id STRING, name STRING, qualified_name STRING, type_annotation STRING, \
             start_line INT64, end_line INT64, language STRING"),
        (NODE_TYPE_ALIAS,
            "id STRING, name STRING, qualified_name STRING, target_type STRING, \
             start_line INT64, end_line INT64, language STRING"),
        // source: stages/stage-3.md §10.1 (span) + §10.4 (`is_resolved` on Import
        // and CallSite — Stage 4 must distinguish "resolved" from "attempted,
        // failed" from "never attempted"; the indexer writes false, the resolver
        // flips it to true when it emits the resolved edge).
        (NODE_IMPORT,
            "id STRING, path STRING, alias STRING, is_glob BOOLEAN, \
             start_line INT64, end_line INT64, is_resolved BOOLEAN, language STRING"),
        (NODE_CALL_SITE,
            "id STRING, callee_name STRING, line INT64, col INT64, \
             is_resolved BOOLEAN, language STRING"),
        // 3c Community + Process — source: stages/stage-3c.md §4.1
        (NODE_COMMUNITY,
            "id STRING, name STRING, algorithm STRING, \
             resolution_param DOUBLE, member_count INT64, \
             modularity_contribution DOUBLE"),
        (NODE_PROCESS,
            "id STRING, name STRING, entry_point_id STRING, \
             entry_kind STRING, entry_confidence DOUBLE, \
             depth INT64, symbol_count INT64"),
        // source: stages/stage-3b-v2.md §5 Layer 5 — StdlibSymbol carries
        // language + canonical_path (= id) + receiver_type + name.
        (NODE_STDLIB_SYMBOL,
            "id STRING, name STRING, language STRING, \
             receiver_type STRING, canonical_path STRING"),
        // History layer — source: second-brain history requirement.
        // Commit: one git commit. id = sha. committed_at is unix seconds.
        (NODE_COMMIT,
            "id STRING, sha STRING, author STRING, author_email STRING, \
             committed_at INT64, message STRING"),
        // Version: one revision of an entity (File or symbol) at a commit.
        // id = "<entity_id>@<sha>". entity_kind discriminates File/Function/
        // Method/Struct/Enum/Trait so the version spine generalizes to any
        // entity type (code today, documents tomorrow). qualified_name mirrors
        // the entity's qn (or path, for File) for direct lookup.
        (NODE_VERSION,
            "id STRING, entity_id STRING, entity_kind STRING, \
             qualified_name STRING, change_type STRING, commit_sha STRING, \
             committed_at INT64, lines_changed INT64"),
        // Infrastructure-as-code layer (issue #63). One wide IacResource shape
        // covers both K8s documents and Dockerfile build targets; a bulk insert
        // binds only the columns actually present per row (see node_prop_order),
        // so k8s-only columns (api_version/namespace) and dockerfile-only columns
        // (ports/entrypoint/workdir) coexist without null-padding.
        // source: issue #63 criteria 1-2; column set mirrors pass_k8s.c manifest
        // fields + pass_infrascan.c cbm_dockerfile_result_t.
        // `qualified_name` mirrors `id` here — it exists so the shared read-side
        // reverse-dependency walker (`clustering::get_impact`, which binds
        // `a.qualified_name` on every Imports_* `from` node) does not fail its
        // binder check on an IaC source node. Without it, lbug rejects the query
        // for the IaC rel tables and the edges are silently dropped from impact.
        (NODE_IAC_RESOURCE,
            "id STRING, name STRING, qualified_name STRING, resource_kind STRING, \
             api_version STRING, namespace STRING, image STRING, ports STRING, \
             entrypoint STRING, workdir STRING, source STRING, path STRING, \
             start_line INT64"),
        (NODE_IAC_MODULE,
            "id STRING, name STRING, qualified_name STRING, resource_kind STRING, \
             source STRING, path STRING, start_line INT64"),
        (NODE_IAC_IMAGE,
            "id STRING, reference STRING, name STRING, tag STRING, registry STRING"),
        // Full-AST layer. `id` = "{file_id}::ast::{preorder_counter}" — stable
        // and deterministic (same source -> same ids). `parent_id` is "" for
        // the tree root. `child_index` is the node's 0-based position among
        // ALL of its parent's children (named AND anonymous), the same order
        // `Node::child(i)`/`TreeCursor` iterate — replaying rows ordered by
        // (parent_id, child_index) reconstructs the tree exactly.
        // source: full-AST persistence contract.
        (NODE_AST_NODE,
            "id STRING, file_id STRING, kind STRING, is_named BOOLEAN, \
             start_byte INT64, end_byte INT64, start_line INT64, start_col INT64, \
             end_line INT64, end_col INT64, field_name STRING, child_index INT64, \
             parent_id STRING, language STRING"),
        // One row per parsed file: the whole source, zstd-compressed. Every
        // `AstNode.start_byte`/`end_byte` indexes into the DECOMPRESSED bytes
        // of this row — exact text recoverable from the store alone, no file
        // read, no re-parse. `id` = the file's relative path (matches File.id).
        (NODE_FILE_CONTENT,
            "id STRING, content_zstd BLOB, original_size INT64, compressed_size INT64"),
];

pub(crate) fn node_table_ddl() -> Vec<String> {
    NODE_TABLE_SCHEMAS
        .iter()
        .map(|(label, columns)| ddl_node(label, columns))
        .collect()
}

pub(crate) fn ddl_node(label: &str, columns: &str) -> String {
    format!("CREATE NODE TABLE IF NOT EXISTS {label}({columns}, PRIMARY KEY(id))")
}

pub(crate) fn rel_table_ddl() -> Vec<String> {
    REL_TABLES
        .iter()
        .map(|(name, from, to)| {
            format!(
                "CREATE REL TABLE IF NOT EXISTS {name}(FROM {from} TO {to}{})",
                rel_properties(name)
            )
        })
        .collect()
}

// Property lists preserve the schema contracts: temporal coupling and observed
// calls (issue #58), resolution provenance (stage-3b §2 and Spike B' bug #4),
// process entry/participation (stage-3c §4.2), and the full-AST child contract.
// Only dispatch moved out of the statement formatter; SQL columns are unchanged.
fn rel_properties(name: &str) -> &'static str {
    if is_cochange_rel(name) {
        ", cochange_count INT64, support INT64, coupling DOUBLE, jaccard DOUBLE, last_co_change INT64"
    } else if is_observed_calls_rel(name) {
        ", observed_count INT64"
    } else if is_observable_static_calls_rel(name) {
        ", confidence DOUBLE, resolution_method STRING, observed_count INT64"
    } else if is_resolution_rel(name) || is_structural_provenance_rel(name) {
        ", confidence DOUBLE, resolution_method STRING"
    } else if is_entrypoint_rel(name) {
        ", confidence DOUBLE"
    } else if is_participates_rel(name) {
        ", depth INT64"
    } else if is_ast_child_rel(name) {
        ", child_index INT64, field_name STRING"
    } else {
        ""
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Maps a compound rel table name back to (from_label, to_label).
pub(crate) fn parse_rel_endpoints(rel_type: &str) -> Result<(&str, &str), String> {
    for &(name, from, to) in REL_TABLES.iter() {
        if name == rel_type {
            return Ok((from, to));
        }
    }
    Err(format!("unknown relationship type: {rel_type}"))
}
