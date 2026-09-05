// graph_store::columns — per-label/per-relationship LogicalType column maps
// for the UNWIND bulk-insert path.
//
// Extracted from serialize.rs (Fowler "Move Function") to keep both files
// under the §4.1 cap. Pure move; `use super::*` provides the shared store
// vocabulary exactly as when this lived in one module.

use super::*;

// ---------------------------------------------------------------------------
// Schema column-type map for the UNWIND bulk path.
//
// The UNWIND + Struct parameter path requires strongly-typed Value variants
// matching each column's declared type. The lookup below mirrors
// node_table_ddl() / rel_table_ddl() exactly — it is the single source of
// truth for "what LogicalType does this (label, property) expect".
// source: stages/stage-3.md §schema, stages/stage-3b.md §2, stages/stage-3c.md §4.
// ---------------------------------------------------------------------------

pub(crate) type ColTypes = &'static [(&'static str, LogicalType)];

// Schema tables, grouped by shape. Mirrors node_table_ddl() columns.
pub(crate) const COLS_DIRECTORY: ColTypes = &[
    ("id", LogicalType::String),
    ("path", LogicalType::String),
    ("name", LogicalType::String),
];
pub(crate) const COLS_FILE: ColTypes = &[
    ("id", LogicalType::String),
    ("path", LogicalType::String),
    ("name", LogicalType::String),
    ("extension", LogicalType::String),
    ("size_bytes", LogicalType::Int64),
    // source: stages/stage-3.md §10.5 — must mirror the NODE_FILE DDL.
    ("parse_errors", LogicalType::Int64),
];
// source: Spike B' BUG #5 + #9 — every symbol-bearing label gets a
// `language` String column; Struct/Enum/Trait additionally gain `bases`.
// Module intentionally has no language (it's a logical aggregation, not
// source); it still uses COLS_MODULE which keeps the pre-Spike-B' shape.
pub(crate) const COLS_MODULE: ColTypes = &[
    ("id", LogicalType::String),
    ("name", LogicalType::String),
    ("qualified_name", LogicalType::String),
];
pub(crate) const COLS_VARIANT: ColTypes = &[
    ("id", LogicalType::String),
    ("name", LogicalType::String),
    ("qualified_name", LogicalType::String),
    // source: stages/stage-3.md §10.1 — must mirror the NODE_VARIANT DDL.
    ("start_line", LogicalType::Int64),
    ("end_line", LogicalType::Int64),
    ("language", LogicalType::String),
];
pub(crate) const COLS_FUNCTION: ColTypes = &[
    ("id", LogicalType::String),
    ("name", LogicalType::String),
    ("qualified_name", LogicalType::String),
    ("start_line", LogicalType::Int64),
    ("end_line", LogicalType::Int64),
    ("visibility", LogicalType::String),
    ("is_async", LogicalType::Bool),
    // source: issue #92 — Uses-edge inputs (return type + constructed types).
    ("return_type", LogicalType::String),
    ("constructed_types", LogicalType::String),
    ("language", LogicalType::String),
    ("entry_kind", LogicalType::String),
];
pub(crate) const COLS_METHOD: ColTypes = &[
    ("id", LogicalType::String),
    ("name", LogicalType::String),
    ("qualified_name", LogicalType::String),
    ("start_line", LogicalType::Int64),
    ("end_line", LogicalType::Int64),
    ("visibility", LogicalType::String),
    ("is_async", LogicalType::Bool),
    ("receiver_type", LogicalType::String),
    ("trait_name", LogicalType::String),
    // source: issue #92 — Uses-edge inputs (return type + constructed types).
    ("return_type", LogicalType::String),
    ("constructed_types", LogicalType::String),
    ("language", LogicalType::String),
];
pub(crate) const COLS_TYPEDECL: ColTypes = &[
    ("id", LogicalType::String),
    ("name", LogicalType::String),
    ("qualified_name", LogicalType::String),
    ("start_line", LogicalType::Int64),
    ("end_line", LogicalType::Int64),
    ("visibility", LogicalType::String),
    ("language", LogicalType::String),
    ("bases", LogicalType::String),
    ("implements", LogicalType::String),
];
// source: stages/stage-3.md §10.1 — Field/Constant/TypeAlias/Import gain span
// columns; §10.4 — Import/CallSite gain is_resolved. Each const MUST mirror the
// corresponding node DDL exactly (column name + order feed the UNWIND type map).
pub(crate) const COLS_FIELD: ColTypes = &[
    ("id", LogicalType::String),
    ("name", LogicalType::String),
    ("type_annotation", LogicalType::String),
    ("visibility", LogicalType::String),
    ("start_line", LogicalType::Int64),
    ("end_line", LogicalType::Int64),
    ("language", LogicalType::String),
];
pub(crate) const COLS_CONSTANT: ColTypes = &[
    ("id", LogicalType::String),
    ("name", LogicalType::String),
    ("qualified_name", LogicalType::String),
    ("type_annotation", LogicalType::String),
    ("start_line", LogicalType::Int64),
    ("end_line", LogicalType::Int64),
    ("language", LogicalType::String),
];
pub(crate) const COLS_TYPE_ALIAS: ColTypes = &[
    ("id", LogicalType::String),
    ("name", LogicalType::String),
    ("qualified_name", LogicalType::String),
    ("target_type", LogicalType::String),
    ("start_line", LogicalType::Int64),
    ("end_line", LogicalType::Int64),
    ("language", LogicalType::String),
];
pub(crate) const COLS_IMPORT: ColTypes = &[
    ("id", LogicalType::String),
    ("path", LogicalType::String),
    ("alias", LogicalType::String),
    ("is_glob", LogicalType::Bool),
    ("start_line", LogicalType::Int64),
    ("end_line", LogicalType::Int64),
    ("is_resolved", LogicalType::Bool),
    ("language", LogicalType::String),
];
pub(crate) const COLS_CALL_SITE: ColTypes = &[
    ("id", LogicalType::String),
    ("callee_name", LogicalType::String),
    ("line", LogicalType::Int64),
    ("col", LogicalType::Int64),
    ("is_resolved", LogicalType::Bool),
    ("language", LogicalType::String),
];
pub(crate) const COLS_COMMUNITY: ColTypes = &[
    ("id", LogicalType::String),
    ("name", LogicalType::String),
    ("algorithm", LogicalType::String),
    ("resolution_param", LogicalType::Double),
    ("member_count", LogicalType::Int64),
    ("modularity_contribution", LogicalType::Double),
];
pub(crate) const COLS_PROCESS: ColTypes = &[
    ("id", LogicalType::String),
    ("name", LogicalType::String),
    ("entry_point_id", LogicalType::String),
    ("entry_kind", LogicalType::String),
    ("entry_confidence", LogicalType::Double),
    ("depth", LogicalType::Int64),
    ("symbol_count", LogicalType::Int64),
];
// History layer — mirrors the NODE_COMMIT / NODE_VERSION DDL exactly.
pub(crate) const COLS_COMMIT: ColTypes = &[
    ("id", LogicalType::String),
    ("sha", LogicalType::String),
    ("author", LogicalType::String),
    ("author_email", LogicalType::String),
    ("committed_at", LogicalType::Int64),
    ("message", LogicalType::String),
];
pub(crate) const COLS_VERSION: ColTypes = &[
    ("id", LogicalType::String),
    ("entity_id", LogicalType::String),
    ("entity_kind", LogicalType::String),
    ("qualified_name", LogicalType::String),
    ("change_type", LogicalType::String),
    ("commit_sha", LogicalType::String),
    ("committed_at", LogicalType::Int64),
    ("lines_changed", LogicalType::Int64),
];
// Infrastructure-as-code layer (issue #63) — mirror the NODE_IAC_* DDL exactly.
pub(crate) const COLS_IAC_RESOURCE: ColTypes = &[
    ("id", LogicalType::String),
    ("name", LogicalType::String),
    ("qualified_name", LogicalType::String),
    ("resource_kind", LogicalType::String),
    ("api_version", LogicalType::String),
    ("namespace", LogicalType::String),
    ("image", LogicalType::String),
    ("ports", LogicalType::String),
    ("entrypoint", LogicalType::String),
    ("workdir", LogicalType::String),
    ("source", LogicalType::String),
    ("path", LogicalType::String),
    ("start_line", LogicalType::Int64),
];
pub(crate) const COLS_IAC_MODULE: ColTypes = &[
    ("id", LogicalType::String),
    ("name", LogicalType::String),
    ("qualified_name", LogicalType::String),
    ("resource_kind", LogicalType::String),
    ("source", LogicalType::String),
    ("path", LogicalType::String),
    ("start_line", LogicalType::Int64),
];
pub(crate) const COLS_IAC_IMAGE: ColTypes = &[
    ("id", LogicalType::String),
    ("reference", LogicalType::String),
    ("name", LogicalType::String),
    ("tag", LogicalType::String),
    ("registry", LogicalType::String),
];
// Full-AST layer — mirrors the NODE_AST_NODE DDL exactly. `FileContent` has
// no entry here: its `content_zstd` column is `BLOB`, which the UNWIND bulk
// path's `literal_to_value` cannot produce (it only parses String/Int64/
// Bool/Double text literals — a compressed byte string is not valid UTF-8
// text). `FileContent` rows go through `GraphStore::insert_file_content`
// instead, which builds a typed `Value::Blob` directly.
pub(crate) const COLS_AST_NODE: ColTypes = &[
    ("id", LogicalType::String),
    ("file_id", LogicalType::String),
    ("kind", LogicalType::String),
    ("is_named", LogicalType::Bool),
    ("start_byte", LogicalType::Int64),
    ("end_byte", LogicalType::Int64),
    ("start_line", LogicalType::Int64),
    ("start_col", LogicalType::Int64),
    ("end_line", LogicalType::Int64),
    ("end_col", LogicalType::Int64),
    ("field_name", LogicalType::String),
    ("child_index", LogicalType::Int64),
    ("parent_id", LogicalType::String),
    ("language", LogicalType::String),
];

pub(crate) fn node_column_types(label: &str) -> Result<ColTypes, String> {
    match label {
        NODE_DIRECTORY => Ok(COLS_DIRECTORY),
        NODE_FILE => Ok(COLS_FILE),
        NODE_MODULE => Ok(COLS_MODULE),
        NODE_VARIANT => Ok(COLS_VARIANT),
        NODE_FUNCTION => Ok(COLS_FUNCTION),
        NODE_METHOD => Ok(COLS_METHOD),
        NODE_STRUCT | NODE_ENUM | NODE_TRAIT => Ok(COLS_TYPEDECL),
        NODE_FIELD => Ok(COLS_FIELD),
        NODE_CONSTANT => Ok(COLS_CONSTANT),
        NODE_TYPE_ALIAS => Ok(COLS_TYPE_ALIAS),
        NODE_IMPORT => Ok(COLS_IMPORT),
        NODE_CALL_SITE => Ok(COLS_CALL_SITE),
        NODE_COMMUNITY => Ok(COLS_COMMUNITY),
        NODE_PROCESS => Ok(COLS_PROCESS),
        NODE_COMMIT => Ok(COLS_COMMIT),
        NODE_VERSION => Ok(COLS_VERSION),
        NODE_IAC_RESOURCE => Ok(COLS_IAC_RESOURCE),
        NODE_IAC_MODULE => Ok(COLS_IAC_MODULE),
        NODE_IAC_IMAGE => Ok(COLS_IAC_IMAGE),
        NODE_AST_NODE => Ok(COLS_AST_NODE),
        other => Err(format!("unknown node label for bulk insert: {other}")),
    }
}

/// True when `label`'s node table declares `column`.
///
/// The DDL is the single source of truth for this question. lbug raises a hard
/// Binder exception — not a NULL — when a query binds a property the matched
/// label's table does not declare, and that drops the whole query's results, so
/// a read-side traversal MUST gate its RETURN clause on the real column list.
/// Hand-written partitions of "labels with line numbers" drift from the schema:
/// one such copy asserted that Constant and TypeAlias carry no line range when
/// `COLS_CONSTANT` and `COLS_TYPE_ALIAS` both declare `start_line`/`end_line`,
/// which silently dropped those line numbers from every search result and
/// get_context answer for those two kinds.
pub fn label_declares_column(label: &str, column: &str) -> bool {
    node_column_types(label)
        .map(|cols| cols.iter().any(|(name, _)| *name == column))
        .unwrap_or(false)
}

/// Returns the declared property schema for an edge table. Empty for
/// untyped rel tables. source: rel_table_ddl() in this module.
pub(crate) fn edge_column_types(rel_table: &str) -> ColTypes {
    if is_resolution_rel(rel_table) || is_structural_provenance_rel(rel_table) {
        &[
            ("confidence", LogicalType::Double),
            ("resolution_method", LogicalType::String),
        ]
    } else if is_entrypoint_rel(rel_table) {
        &[("confidence", LogicalType::Double)]
    } else if is_participates_rel(rel_table) {
        &[("depth", LogicalType::Int64)]
    } else if is_ast_child_rel(rel_table) {
        &[
            ("child_index", LogicalType::Int64),
            ("field_name", LogicalType::String),
        ]
    } else {
        &[]
    }
}
