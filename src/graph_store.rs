// graph_store — LadybugDB port for the code-intelligence graph.
//
// This module wraps `lbug::Database` + `lbug::Connection` behind a clean
// interface. The rest of the codebase depends only on `GraphStore` methods,
// never on `lbug` directly. If we ever swap the backing store, only this
// file changes.

use lbug::{Connection, Database, LogicalType, PreparedStatement, SystemConfig, Value};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;

// ---------------------------------------------------------------------------
// Node labels — source: stages/stage-3.md §schema (Shannon spec, 3a subset)
// ---------------------------------------------------------------------------

pub const NODE_DIRECTORY: &str = "Directory"; // source: stages/stage-3.md §schema
pub const NODE_FILE: &str = "File"; // source: stages/stage-3.md §schema
pub const NODE_MODULE: &str = "Module"; // source: stages/stage-3.md §schema
pub const NODE_FUNCTION: &str = "Function"; // source: stages/stage-3.md §schema
pub const NODE_METHOD: &str = "Method"; // source: stages/stage-3.md §schema
pub const NODE_STRUCT: &str = "Struct"; // source: stages/stage-3.md §schema
pub const NODE_ENUM: &str = "Enum"; // source: stages/stage-3.md §schema
pub const NODE_VARIANT: &str = "Variant"; // source: stages/stage-3.md §schema
pub const NODE_TRAIT: &str = "Trait"; // source: stages/stage-3.md §schema
pub const NODE_FIELD: &str = "Field"; // source: stages/stage-3.md §schema
pub const NODE_CONSTANT: &str = "Constant"; // source: stages/stage-3.md §schema
pub const NODE_TYPE_ALIAS: &str = "TypeAlias"; // source: stages/stage-3.md §schema
pub const NODE_IMPORT: &str = "Import"; // source: stages/stage-3.md §schema
pub const NODE_CALL_SITE: &str = "CallSite"; // source: stages/stage-3.md §schema
pub const NODE_COMMUNITY: &str = "Community"; // source: stages/stage-3c.md §4.1
pub const NODE_PROCESS: &str = "Process"; // source: stages/stage-3c.md §4.1

// ---------------------------------------------------------------------------
// Edge kinds — source: stages/stage-3.md §schema (Shannon spec, 3a subset)
// ---------------------------------------------------------------------------

#[allow(dead_code)] // used in stage 3b — resolution edge kind lookup
pub const EDGE_CONTAINS: &str = "Contains"; // source: stages/stage-3.md §schema
#[allow(dead_code)] // used in stage 3b — resolution edge kind lookup
pub const EDGE_DEFINES: &str = "Defines"; // source: stages/stage-3.md §schema
#[allow(dead_code)] // used in stage 3b — resolution edge kind lookup
pub const EDGE_HAS_METHOD: &str = "HasMethod"; // source: stages/stage-3.md §schema
#[allow(dead_code)] // used in stage 3b — resolution edge kind lookup
pub const EDGE_HAS_FIELD: &str = "HasField"; // source: stages/stage-3.md §schema
#[allow(dead_code)] // used in stage 3b — resolution edge kind lookup
pub const EDGE_HAS_VARIANT: &str = "HasVariant"; // source: stages/stage-3.md §schema

// ---------------------------------------------------------------------------
// Cypher string escaping — security-critical.
//
// LadybugDB's Cypher dialect (lbug 0.15) exposes no parameterized-query API in
// the Rust crate, so every user-controlled value is interpolated as a string
// literal. An unescaped single quote (or backslash-escape of one) closes the
// literal and allows arbitrary Cypher injection (including `DETACH DELETE`).
//
// Rules (order matters):
//   1. `\\` → `\\\\` (escape backslashes FIRST to avoid double-processing),
//   2. `'`  → `\\'`  (escape quotes after),
//   3. wrap result in single quotes.
//
// source: Neo4j Cypher Manual §"Literals" — string literals use `'` delimiters,
// backslash is the escape character. Applies to LadybugDB which mirrors openCypher.
// ---------------------------------------------------------------------------

/// Escapes a raw string for safe use as a Cypher single-quoted string literal.
/// Returns the string already wrapped in single quotes.
/// ALL user-controlled values heading into Cypher must go through this helper.
pub fn cypher_str(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() + 2);
    out.push('\'');
    for ch in raw.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            _ => out.push(ch),
        }
    }
    out.push('\'');
    out
}

// ---------------------------------------------------------------------------
// Bulk-insert batch size.
//
// source: Kuzu/LadybugDB practitioner guidance — batching multiple CREATE
// clauses in a single Cypher query amortizes parser + planner + lock costs.
// 500 is a common sweet spot: large enough to dominate per-call overhead,
// small enough to keep the generated Cypher text under typical statement
// limits. Tunable; not derived from a paper.
// source: empirical — per-row CREATE was measured as the dominant indexing
// cost in the Fermi scalability audit of this codebase (April 2026).
pub const BULK_BATCH_SIZE: usize = 500;

// ---------------------------------------------------------------------------
// QueryResult — thin wrapper returned by execute_query
// ---------------------------------------------------------------------------

/// Result of a Cypher query: column names + row data as strings.
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

// ---------------------------------------------------------------------------
// GraphStore — owns the lbug Database + Connection
// ---------------------------------------------------------------------------

pub struct GraphStore {
    _db: Database,
    conn: Connection<'static>,
    // Cache of prepared UNWIND statements keyed by the exact Cypher text.
    // A fresh prepare() is ~0.5-2 ms; with one bulk call per file per label
    // the 500-file fixture produces ~4000 bulk calls so uncached prepare
    // alone costs multiple seconds. Keyed by Cypher rather than by label
    // because node rows with different property subsets generate different
    // UNWIND statements (File rows without size_bytes, etc.).
    // source: scalability_bench regression observed April 2026 when
    // prepare-per-call was introduced; caching restores linear scaling.
    stmt_cache: RefCell<HashMap<String, PreparedStatement>>,
    // Safety: `_db` is heap-allocated via lbug's C++ bridge (UniquePtr).
    // `Connection` borrows `Database` through a raw pointer on the C++ side,
    // not through Rust's borrow checker. Moving the Rust struct does not
    // invalidate the C++ pointer because `Database.db` is `UnsafeCell<UniquePtr>`
    // — the heap allocation is stable. lbug marks both Database and Connection
    // as Send+Sync. This self-referential pattern is safe here because:
    //   (a) `_db` is never moved out or dropped before `conn`,
    //   (b) lbug's own test suite uses the same stack-lifetime pattern,
    //   (c) struct fields drop in declaration order (conn drops before _db).
}

impl GraphStore {
    /// Opens (or creates) a LadybugDB database at `path`.
    pub fn open_or_create(path: &Path) -> Result<Self, String> {
        let db = Database::new(path, SystemConfig::default())
            .map_err(|e| format!("lbug database open failed: {e}"))?;
        // Safety: see comment on the struct. The Database is heap-stable and
        // outlives the Connection because struct fields drop in declaration order.
        let conn: Connection<'static> = unsafe {
            std::mem::transmute::<Connection<'_>, Connection<'static>>(
                Connection::new(&db)
                    .map_err(|e| format!("lbug connection failed: {e}"))?,
            )
        };
        Ok(GraphStore {
            _db: db,
            conn,
            stmt_cache: RefCell::new(HashMap::new()),
        })
    }

    /// Creates the full 3a schema (node tables + relationship tables).
    pub fn create_schema(&self) -> Result<(), String> {
        for ddl in node_table_ddl() {
            self.exec_ddl(&ddl)?;
        }
        for ddl in rel_table_ddl() {
            self.exec_ddl(&ddl)?;
        }
        Ok(())
    }

    /// Inserts a single node. `properties` are `(key, cypher_literal)` pairs.
    /// Values are interpolated as-is into Cypher — caller must quote strings.
    pub fn insert_node(
        &self,
        label: &str,
        properties: &[(&str, &str)],
    ) -> Result<(), String> {
        let props = format_props(properties);
        let cypher = format!("CREATE (:{label} {{{props}}})");
        self.run(&cypher)?;
        Ok(())
    }

    /// Bulk-inserts many nodes of the same label using the UNWIND + prepared
    /// statement pattern with typed `LogicalType::Struct` parameters.
    ///
    /// Strategy: one prepared statement per unique (label, property-subset)
    /// combination, cached on the `GraphStore`, executed per chunk of
    /// BULK_BATCH_SIZE rows. Each chunk flows through the FFI as a
    /// `Value::List(Struct{...}, rows)` — no Cypher string interpolation,
    /// no per-row parse/plan.
    ///
    /// Note: per-call explicit BEGIN/COMMIT was tried and measured slower
    /// on the 500-file fixture (72s vs 38s) because lbug already uses
    /// per-query auto-commit and each explicit tx adds two round-trips.
    /// dba's 8x figure came from wrapping many small writes inside ONE tx,
    /// not from wrapping every bulk call in its own tx.
    ///
    /// source: dba probe_2 in tests/lbug_bulk_investigation.rs confirmed
    /// list-of-structs UNWIND works and is dramatically faster than
    /// per-row CREATE (38x measured on large edge batches).
    pub fn bulk_insert_nodes(
        &self,
        label: &str,
        rows: &[Vec<(String, String)>],
    ) -> Result<u64, String> {
        if rows.is_empty() {
            return Ok(0);
        }
        let schema = node_column_types(label)?;
        let prop_order = node_prop_order(rows, schema);
        let (cypher, row_type) = build_node_unwind(label, &prop_order);
        let mut inserted: u64 = 0;
        for chunk in rows.chunks(BULK_BATCH_SIZE) {
            let values = build_struct_rows(chunk, &prop_order)?;
            let list = Value::List(row_type.clone(), values);
            self.run_prepared(&cypher, list)
                .map_err(|e| format!("bulk_insert_nodes execute: {e}"))?;
            inserted += chunk.len() as u64;
        }
        Ok(inserted)
    }

    /// Bulk-inserts many edges that share the same relationship table using
    /// UNWIND + MATCH + CREATE in a single prepared statement per chunk.
    /// Each edge is `(from_id, to_id, properties)`.
    ///
    /// Strategy mirrors bulk_insert_nodes: one prepared UNWIND statement
    /// per (rel_table, property-subset), cached on the `GraphStore`,
    /// executed per BULK_BATCH_SIZE chunk, values passed as typed
    /// `Value::List(Struct{...}, rows)`. The (from_label, to_label, rel)
    /// triple is known at prep time from REL_TABLES, so one prepared
    /// statement covers every edge of a given kind.
    ///
    /// source: dba probe_4 + probe_9 in tests/lbug_bulk_investigation.rs.
    pub fn bulk_insert_edges(
        &self,
        rel_table: &str,
        edges: &[(String, String, Vec<(String, String)>)],
    ) -> Result<u64, String> {
        if edges.is_empty() {
            return Ok(0);
        }
        let (from_label, to_label) = parse_rel_endpoints(rel_table)?;
        let prop_schema = edge_column_types(rel_table);
        let prop_order = edge_prop_order(edges, prop_schema);
        let (cypher, row_type) =
            build_edge_unwind(rel_table, from_label, to_label, &prop_order);
        let mut inserted: u64 = 0;
        for chunk in edges.chunks(BULK_BATCH_SIZE) {
            let values = build_edge_struct_rows(chunk, &prop_order)?;
            let list = Value::List(row_type.clone(), values);
            self.run_prepared(&cypher, list)
                .map_err(|e| format!("bulk_insert_edges execute: {e}"))?;
            inserted += chunk.len() as u64;
        }
        Ok(inserted)
    }

    /// Inserts a single edge between two nodes identified by their `id` property.
    pub fn insert_edge(
        &self,
        rel_type: &str,
        from_id: &str,
        to_id: &str,
        properties: &[(&str, &str)],
    ) -> Result<(), String> {
        let (from_label, to_label) = parse_rel_endpoints(rel_type)?;
        let props_clause = if properties.is_empty() {
            String::new()
        } else {
            format!(" {{{}}}", format_props(properties))
        };
        let from_lit = cypher_str(from_id);
        let to_lit = cypher_str(to_id);
        let cypher = format!(
            "MATCH (a:{from_label}), (b:{to_label}) \
             WHERE a.id = {from_lit} AND b.id = {to_lit} \
             CREATE (a)-[:{rel_type}{props_clause}]->(b)"
        );
        self.run(&cypher)?;
        Ok(())
    }

    /// Executes an arbitrary Cypher query and returns columns + rows.
    pub fn execute_query(&self, cypher: &str) -> Result<QueryResult, String> {
        let mut result = self.run(cypher)?;
        let columns = result.get_column_names();
        let rows: Vec<Vec<String>> = result
            .by_ref()
            .map(|tuple| tuple.iter().map(value_to_string).collect())
            .collect();
        Ok(QueryResult { columns, rows })
    }

    /// Returns the total number of nodes across all node tables.
    pub fn node_count(&self) -> Result<u64, String> {
        let mut total: u64 = 0;
        for label in NODE_LABELS {
            let cypher = format!("MATCH (n:{label}) RETURN count(n)");
            match self.run(&cypher) {
                Ok(mut r) => {
                    if let Some(row) = r.next() {
                        total += value_to_u64(&row[0]);
                    }
                }
                Err(_) => continue,
            }
        }
        Ok(total)
    }

    /// Returns the total number of edges across all relationship tables.
    pub fn edge_count(&self) -> Result<u64, String> {
        let mut total: u64 = 0;
        for &(rel, _, _) in REL_TABLES {
            let cypher = format!("MATCH ()-[r:{rel}]->() RETURN count(r)");
            match self.run(&cypher) {
                Ok(mut r) => {
                    if let Some(row) = r.next() {
                        total += value_to_u64(&row[0]);
                    }
                }
                Err(_) => continue,
            }
        }
        Ok(total)
    }

    // -- private helpers ----------------------------------------------------

    fn exec_ddl(&self, ddl: &str) -> Result<(), String> {
        self.conn
            .query(ddl)
            .map_err(|e| format!("DDL failed [{ddl}]: {e}"))?;
        Ok(())
    }

    fn run(&self, cypher: &str) -> Result<lbug::QueryResult<'_>, String> {
        self.conn
            .query(cypher)
            .map_err(|e| format!("query failed [{cypher}]: {e}"))
    }

    /// Runs one UNWIND execute() against the cached prepared statement for
    /// `cypher`, preparing and inserting into the cache on first use.
    /// The cache is critical: the per-call prepare() cost dominates small
    /// bulk chunks (common when indexing small files), and caching turns
    /// the whole bulk path into a single plan-once/execute-many loop.
    fn run_prepared(&self, cypher: &str, rows: Value) -> Result<(), String> {
        let mut cache = self.stmt_cache.borrow_mut();
        if !cache.contains_key(cypher) {
            let stmt = self
                .conn
                .prepare(cypher)
                .map_err(|e| format!("prepare failed [{cypher}]: {e}"))?;
            cache.insert(cypher.to_string(), stmt);
        }
        let stmt = cache
            .get_mut(cypher)
            .expect("statement just inserted into cache");
        self.conn
            .execute(stmt, vec![("rows", rows)])
            .map(|_| ())
            .map_err(|e| format!("execute [{cypher}]: {e}"))
    }
}

// ---------------------------------------------------------------------------
// Schema DDL generators
// ---------------------------------------------------------------------------

const NODE_LABELS: &[&str] = &[
    NODE_DIRECTORY, NODE_FILE, NODE_MODULE, NODE_FUNCTION, NODE_METHOD,
    NODE_STRUCT, NODE_ENUM, NODE_VARIANT, NODE_TRAIT, NODE_FIELD,
    NODE_CONSTANT, NODE_TYPE_ALIAS, NODE_IMPORT, NODE_CALL_SITE,
    NODE_COMMUNITY, NODE_PROCESS,
];

/// Single source of truth for all relationship tables: (name, from, to).
/// Used for DDL generation, endpoint lookup, and edge counting.
/// LadybugDB does not support REL TABLE GROUP in the lbug 0.15.x Rust crate
/// (no references in source). We create one rel table per (source, target) pair
/// with a naming convention: `{Kind}_{From}_{To}`.
pub const REL_TABLES: &[(&str, &str, &str)] = &[
    // Contains — source: stages/stage-3.md §schema
    ("Contains_Dir_File", NODE_DIRECTORY, NODE_FILE),
    ("Contains_Dir_Dir", NODE_DIRECTORY, NODE_DIRECTORY),
    ("Contains_File_Module", NODE_FILE, NODE_MODULE),
    // Defines — source: stages/stage-3.md §schema
    ("Defines_File_Function", NODE_FILE, NODE_FUNCTION),
    ("Defines_File_Struct", NODE_FILE, NODE_STRUCT),
    ("Defines_File_Enum", NODE_FILE, NODE_ENUM),
    ("Defines_File_Trait", NODE_FILE, NODE_TRAIT),
    ("Defines_File_Constant", NODE_FILE, NODE_CONSTANT),
    ("Defines_File_TypeAlias", NODE_FILE, NODE_TYPE_ALIAS),
    // source: B1 fix — Q9/Q14 expect File->Import edges, and resolver
    // also needs to walk a Module->Import parent for mod-nested uses.
    ("Defines_File_Import", NODE_FILE, NODE_IMPORT),
    ("Defines_Module_Import", NODE_MODULE, NODE_IMPORT),
    ("Defines_Module_Function", NODE_MODULE, NODE_FUNCTION),
    ("Defines_Module_Struct", NODE_MODULE, NODE_STRUCT),
    ("Defines_Module_Enum", NODE_MODULE, NODE_ENUM),
    ("Defines_Module_Trait", NODE_MODULE, NODE_TRAIT),
    ("Defines_Module_Constant", NODE_MODULE, NODE_CONSTANT),
    ("Defines_Module_TypeAlias", NODE_MODULE, NODE_TYPE_ALIAS),
    // HasMethod — source: stages/stage-3.md §schema
    ("HasMethod_Struct_Method", NODE_STRUCT, NODE_METHOD),
    ("HasMethod_Enum_Method", NODE_ENUM, NODE_METHOD),
    ("HasMethod_Trait_Method", NODE_TRAIT, NODE_METHOD),
    // HasField — source: stages/stage-3.md §schema
    ("HasField_Struct_Field", NODE_STRUCT, NODE_FIELD),
    ("HasField_Enum_Field", NODE_ENUM, NODE_FIELD),
    // HasVariant — source: stages/stage-3.md §schema
    ("HasVariant_Enum_Variant", NODE_ENUM, NODE_VARIANT),
    // Imports — source: stages/stage-3b.md §2, §3
    ("Imports_File_File", NODE_FILE, NODE_FILE),
    ("Imports_File_Module", NODE_FILE, NODE_MODULE),
    ("Imports_File_Function", NODE_FILE, NODE_FUNCTION),
    ("Imports_File_Struct", NODE_FILE, NODE_STRUCT),
    ("Imports_File_Enum", NODE_FILE, NODE_ENUM),
    ("Imports_File_Trait", NODE_FILE, NODE_TRAIT),
    ("Imports_File_Constant", NODE_FILE, NODE_CONSTANT),
    ("Imports_File_TypeAlias", NODE_FILE, NODE_TYPE_ALIAS),
    ("Imports_Module_Function", NODE_MODULE, NODE_FUNCTION),
    ("Imports_Module_Struct", NODE_MODULE, NODE_STRUCT),
    ("Imports_Module_Enum", NODE_MODULE, NODE_ENUM),
    ("Imports_Module_Trait", NODE_MODULE, NODE_TRAIT),
    ("Imports_Module_Constant", NODE_MODULE, NODE_CONSTANT),
    ("Imports_Module_TypeAlias", NODE_MODULE, NODE_TYPE_ALIAS),
    // Calls — source: stages/stage-3b.md §2, §3
    ("Calls_Function_Function", NODE_FUNCTION, NODE_FUNCTION),
    ("Calls_Function_Method", NODE_FUNCTION, NODE_METHOD),
    ("Calls_Method_Function", NODE_METHOD, NODE_FUNCTION),
    ("Calls_Method_Method", NODE_METHOD, NODE_METHOD),
    // Implements — source: stages/stage-3b.md §2, §3
    ("Implements_Struct_Trait", NODE_STRUCT, NODE_TRAIT),
    ("Implements_Enum_Trait", NODE_ENUM, NODE_TRAIT),
    // Extends — source: stages/stage-3b.md §2, §3
    ("Extends_Trait_Trait", NODE_TRAIT, NODE_TRAIT),
    // Uses — source: stages/stage-3b.md §2, §3
    ("Uses_Function_Struct", NODE_FUNCTION, NODE_STRUCT),
    ("Uses_Function_Enum", NODE_FUNCTION, NODE_ENUM),
    ("Uses_Function_Trait", NODE_FUNCTION, NODE_TRAIT),
    ("Uses_Function_TypeAlias", NODE_FUNCTION, NODE_TYPE_ALIAS),
    ("Uses_Method_Struct", NODE_METHOD, NODE_STRUCT),
    ("Uses_Method_Enum", NODE_METHOD, NODE_ENUM),
    ("Uses_Method_Trait", NODE_METHOD, NODE_TRAIT),
    ("Uses_Method_TypeAlias", NODE_METHOD, NODE_TYPE_ALIAS),
    ("Uses_Struct_Struct", NODE_STRUCT, NODE_STRUCT),
    ("Uses_Struct_Enum", NODE_STRUCT, NODE_ENUM),
    ("Uses_Struct_Trait", NODE_STRUCT, NODE_TRAIT),
    ("Uses_Field_Struct", NODE_FIELD, NODE_STRUCT),
    ("Uses_Field_Enum", NODE_FIELD, NODE_ENUM),
    ("Uses_Field_Trait", NODE_FIELD, NODE_TRAIT),
    ("Uses_Field_TypeAlias", NODE_FIELD, NODE_TYPE_ALIAS),
    // 3c MemberOf — source: stages/stage-3c.md §4.2
    ("MemberOf_Function_Community", NODE_FUNCTION, NODE_COMMUNITY),
    ("MemberOf_Method_Community", NODE_METHOD, NODE_COMMUNITY),
    ("MemberOf_Struct_Community", NODE_STRUCT, NODE_COMMUNITY),
    ("MemberOf_Enum_Community", NODE_ENUM, NODE_COMMUNITY),
    ("MemberOf_Trait_Community", NODE_TRAIT, NODE_COMMUNITY),
    ("MemberOf_Constant_Community", NODE_CONSTANT, NODE_COMMUNITY),
    ("MemberOf_TypeAlias_Community", NODE_TYPE_ALIAS, NODE_COMMUNITY),
    ("MemberOf_Module_Community", NODE_MODULE, NODE_COMMUNITY),
    // 3c EntryPointOf — source: stages/stage-3c.md §4.2
    ("EntryPointOf_Function_Process", NODE_FUNCTION, NODE_PROCESS),
    ("EntryPointOf_Method_Process", NODE_METHOD, NODE_PROCESS),
    // 3c ParticipatesIn — source: stages/stage-3c.md §4.2
    ("ParticipatesIn_Function_Process", NODE_FUNCTION, NODE_PROCESS),
    ("ParticipatesIn_Method_Process", NODE_METHOD, NODE_PROCESS),
];

fn node_table_ddl() -> Vec<String> {
    vec![
        // source: stages/stage-3.md §schema
        ddl_node(NODE_DIRECTORY, "id STRING, path STRING, name STRING"),
        ddl_node(NODE_FILE, "id STRING, path STRING, name STRING, extension STRING, size_bytes INT64"),
        ddl_node(NODE_MODULE, "id STRING, name STRING, qualified_name STRING"),
        ddl_node(NODE_FUNCTION,
            "id STRING, name STRING, qualified_name STRING, \
             start_line INT64, end_line INT64, visibility STRING, is_async BOOLEAN"),
        ddl_node(NODE_METHOD,
            "id STRING, name STRING, qualified_name STRING, \
             start_line INT64, end_line INT64, visibility STRING, is_async BOOLEAN, \
             receiver_type STRING"),
        ddl_node(NODE_STRUCT,
            "id STRING, name STRING, qualified_name STRING, \
             start_line INT64, end_line INT64, visibility STRING"),
        ddl_node(NODE_ENUM,
            "id STRING, name STRING, qualified_name STRING, \
             start_line INT64, end_line INT64, visibility STRING"),
        ddl_node(NODE_VARIANT, "id STRING, name STRING, qualified_name STRING"),
        ddl_node(NODE_TRAIT,
            "id STRING, name STRING, qualified_name STRING, \
             start_line INT64, end_line INT64, visibility STRING"),
        ddl_node(NODE_FIELD,
            "id STRING, name STRING, type_annotation STRING, visibility STRING"),
        ddl_node(NODE_CONSTANT,
            "id STRING, name STRING, qualified_name STRING, type_annotation STRING"),
        ddl_node(NODE_TYPE_ALIAS,
            "id STRING, name STRING, qualified_name STRING, target_type STRING"),
        ddl_node(NODE_IMPORT, "id STRING, path STRING, alias STRING, is_glob BOOLEAN"),
        ddl_node(NODE_CALL_SITE, "id STRING, callee_name STRING, line INT64, col INT64"),
        // 3c Community + Process — source: stages/stage-3c.md §4.1
        ddl_node(NODE_COMMUNITY,
            "id STRING, name STRING, algorithm STRING, \
             resolution_param DOUBLE, member_count INT64, \
             modularity_contribution DOUBLE"),
        ddl_node(NODE_PROCESS,
            "id STRING, name STRING, entry_point_id STRING, \
             entry_kind STRING, entry_confidence DOUBLE, \
             depth INT64, symbol_count INT64"),
    ]
}

fn ddl_node(label: &str, columns: &str) -> String {
    format!("CREATE NODE TABLE IF NOT EXISTS {label}({columns}, PRIMARY KEY(id))")
}

/// 3b resolution edge tables carry confidence + resolution_method properties.
/// source: stages/stage-3b.md §2 "Edge properties"
fn is_resolution_rel(name: &str) -> bool {
    name.starts_with("Imports_")
        || name.starts_with("Calls_")
        || name.starts_with("Implements_")
        || name.starts_with("Extends_")
        || name.starts_with("Uses_")
}

fn is_entrypoint_rel(name: &str) -> bool {
    name.starts_with("EntryPointOf_")
}

fn is_participates_rel(name: &str) -> bool {
    name.starts_with("ParticipatesIn_")
}

fn rel_table_ddl() -> Vec<String> {
    REL_TABLES
        .iter()
        .map(|(name, from, to)| {
            if is_resolution_rel(name) {
                // source: stages/stage-3b.md §2 — confidence + resolution_method
                format!(
                    "CREATE REL TABLE IF NOT EXISTS {name}(\
                     FROM {from} TO {to}, \
                     confidence DOUBLE, resolution_method STRING)"
                )
            } else if is_entrypoint_rel(name) {
                // source: stages/stage-3c.md §4.2
                format!(
                    "CREATE REL TABLE IF NOT EXISTS {name}(\
                     FROM {from} TO {to}, confidence DOUBLE)"
                )
            } else if is_participates_rel(name) {
                // source: stages/stage-3c.md §4.2
                format!(
                    "CREATE REL TABLE IF NOT EXISTS {name}(\
                     FROM {from} TO {to}, depth INT64)"
                )
            } else {
                format!("CREATE REL TABLE IF NOT EXISTS {name}(FROM {from} TO {to})")
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Maps a compound rel table name back to (from_label, to_label).
fn parse_rel_endpoints(rel_type: &str) -> Result<(&str, &str), String> {
    for &(name, from, to) in REL_TABLES.iter() {
        if name == rel_type {
            return Ok((from, to));
        }
    }
    Err(format!("unknown relationship type: {rel_type}"))
}

fn format_props(properties: &[(&str, &str)]) -> String {
    properties
        .iter()
        .map(|(k, v)| format!("{k}: {v}"))
        .collect::<Vec<_>>()
        .join(", ")
}

// ---------------------------------------------------------------------------
// Schema column-type map for the UNWIND bulk path.
//
// The UNWIND + Struct parameter path requires strongly-typed Value variants
// matching each column's declared type. The lookup below mirrors
// node_table_ddl() / rel_table_ddl() exactly — it is the single source of
// truth for "what LogicalType does this (label, property) expect".
// source: stages/stage-3.md §schema, stages/stage-3b.md §2, stages/stage-3c.md §4.
// ---------------------------------------------------------------------------

type ColTypes = &'static [(&'static str, LogicalType)];

// Schema tables, grouped by shape. Mirrors node_table_ddl() columns.
const COLS_DIRECTORY: ColTypes = &[
    ("id", LogicalType::String), ("path", LogicalType::String),
    ("name", LogicalType::String),
];
const COLS_FILE: ColTypes = &[
    ("id", LogicalType::String), ("path", LogicalType::String),
    ("name", LogicalType::String), ("extension", LogicalType::String),
    ("size_bytes", LogicalType::Int64),
];
const COLS_NAMED_QN: ColTypes = &[
    ("id", LogicalType::String), ("name", LogicalType::String),
    ("qualified_name", LogicalType::String),
];
const COLS_FUNCTION: ColTypes = &[
    ("id", LogicalType::String), ("name", LogicalType::String),
    ("qualified_name", LogicalType::String),
    ("start_line", LogicalType::Int64), ("end_line", LogicalType::Int64),
    ("visibility", LogicalType::String), ("is_async", LogicalType::Bool),
];
const COLS_METHOD: ColTypes = &[
    ("id", LogicalType::String), ("name", LogicalType::String),
    ("qualified_name", LogicalType::String),
    ("start_line", LogicalType::Int64), ("end_line", LogicalType::Int64),
    ("visibility", LogicalType::String), ("is_async", LogicalType::Bool),
    ("receiver_type", LogicalType::String),
];
const COLS_TYPEDECL: ColTypes = &[
    ("id", LogicalType::String), ("name", LogicalType::String),
    ("qualified_name", LogicalType::String),
    ("start_line", LogicalType::Int64), ("end_line", LogicalType::Int64),
    ("visibility", LogicalType::String),
];
const COLS_FIELD: ColTypes = &[
    ("id", LogicalType::String), ("name", LogicalType::String),
    ("type_annotation", LogicalType::String),
    ("visibility", LogicalType::String),
];
const COLS_CONSTANT: ColTypes = &[
    ("id", LogicalType::String), ("name", LogicalType::String),
    ("qualified_name", LogicalType::String),
    ("type_annotation", LogicalType::String),
];
const COLS_TYPE_ALIAS: ColTypes = &[
    ("id", LogicalType::String), ("name", LogicalType::String),
    ("qualified_name", LogicalType::String),
    ("target_type", LogicalType::String),
];
const COLS_IMPORT: ColTypes = &[
    ("id", LogicalType::String), ("path", LogicalType::String),
    ("alias", LogicalType::String), ("is_glob", LogicalType::Bool),
];
const COLS_CALL_SITE: ColTypes = &[
    ("id", LogicalType::String), ("callee_name", LogicalType::String),
    ("line", LogicalType::Int64), ("col", LogicalType::Int64),
];
const COLS_COMMUNITY: ColTypes = &[
    ("id", LogicalType::String), ("name", LogicalType::String),
    ("algorithm", LogicalType::String),
    ("resolution_param", LogicalType::Double),
    ("member_count", LogicalType::Int64),
    ("modularity_contribution", LogicalType::Double),
];
const COLS_PROCESS: ColTypes = &[
    ("id", LogicalType::String), ("name", LogicalType::String),
    ("entry_point_id", LogicalType::String),
    ("entry_kind", LogicalType::String),
    ("entry_confidence", LogicalType::Double),
    ("depth", LogicalType::Int64),
    ("symbol_count", LogicalType::Int64),
];

fn node_column_types(label: &str) -> Result<ColTypes, String> {
    match label {
        NODE_DIRECTORY => Ok(COLS_DIRECTORY),
        NODE_FILE => Ok(COLS_FILE),
        NODE_MODULE | NODE_VARIANT => Ok(COLS_NAMED_QN),
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
        other => Err(format!("unknown node label for bulk insert: {other}")),
    }
}

/// Returns the declared property schema for an edge table. Empty for
/// untyped rel tables. source: rel_table_ddl() in this module.
fn edge_column_types(rel_table: &str) -> ColTypes {
    if is_resolution_rel(rel_table) {
        &[
            ("confidence", LogicalType::Double),
            ("resolution_method", LogicalType::String),
        ]
    } else if is_entrypoint_rel(rel_table) {
        &[("confidence", LogicalType::Double)]
    } else if is_participates_rel(rel_table) {
        &[("depth", LogicalType::Int64)]
    } else {
        &[]
    }
}

/// Picks the subset of columns that appear in at least one row, preserving
/// the schema's declared order. Callers may omit columns (e.g. File rows
/// without `size_bytes`) so we only bind what was actually provided.
fn node_prop_order(
    rows: &[Vec<(String, String)>],
    schema: ColTypes,
) -> Vec<(&'static str, LogicalType)> {
    let mut present: std::collections::HashSet<&str> =
        std::collections::HashSet::new();
    for row in rows {
        for (k, _) in row {
            present.insert(k.as_str());
        }
    }
    schema
        .iter()
        .filter(|(col, _)| present.contains(*col))
        .map(|(col, ty)| (*col, ty.clone()))
        .collect()
}

/// Edge prop order — schema-driven, only bind columns present in data.
fn edge_prop_order(
    edges: &[(String, String, Vec<(String, String)>)],
    schema: ColTypes,
) -> Vec<(&'static str, LogicalType)> {
    let mut present: std::collections::HashSet<&str> =
        std::collections::HashSet::new();
    for e in edges {
        for (k, _) in &e.2 {
            present.insert(k.as_str());
        }
    }
    schema
        .iter()
        .filter(|(col, _)| present.contains(*col))
        .map(|(col, ty)| (*col, ty.clone()))
        .collect()
}

fn build_node_unwind(
    label: &str,
    prop_order: &[(&'static str, LogicalType)],
) -> (String, LogicalType) {
    let assigns: Vec<String> = prop_order
        .iter()
        .map(|(k, _)| format!("{k}: row.{k}"))
        .collect();
    let cypher = format!(
        "UNWIND $rows AS row CREATE (:{label} {{{}}})",
        assigns.join(", "),
    );
    let fields: Vec<(String, LogicalType)> = prop_order
        .iter()
        .map(|(k, ty)| ((*k).to_string(), ty.clone()))
        .collect();
    (cypher, LogicalType::Struct { fields })
}

fn build_edge_unwind(
    rel_table: &str,
    from_label: &str,
    to_label: &str,
    prop_order: &[(&'static str, LogicalType)],
) -> (String, LogicalType) {
    let props_clause = if prop_order.is_empty() {
        String::new()
    } else {
        let assigns: Vec<String> = prop_order
            .iter()
            .map(|(k, _)| format!("{k}: row.{k}"))
            .collect();
        format!(" {{{}}}", assigns.join(", "))
    };
    let cypher = format!(
        "UNWIND $rows AS row \
         MATCH (a:{from_label}), (b:{to_label}) \
         WHERE a.id = row.from AND b.id = row.to \
         CREATE (a)-[:{rel_table}{props_clause}]->(b)",
    );
    let mut fields: Vec<(String, LogicalType)> = vec![
        ("from".to_string(), LogicalType::String),
        ("to".to_string(), LogicalType::String),
    ];
    for (k, ty) in prop_order {
        fields.push(((*k).to_string(), ty.clone()));
    }
    (cypher, LogicalType::Struct { fields })
}

fn build_struct_rows(
    chunk: &[Vec<(String, String)>],
    prop_order: &[(&'static str, LogicalType)],
) -> Result<Vec<Value>, String> {
    let mut out = Vec::with_capacity(chunk.len());
    for row in chunk {
        let mut fields: Vec<(String, Value)> = Vec::with_capacity(prop_order.len());
        for (col, ty) in prop_order {
            let lit = row.iter().find(|(k, _)| k == *col).map(|(_, v)| v.as_str());
            fields.push(((*col).to_string(), literal_to_value(lit, ty, col)?));
        }
        out.push(Value::Struct(fields));
    }
    Ok(out)
}

fn build_edge_struct_rows(
    edges: &[(String, String, Vec<(String, String)>)],
    prop_order: &[(&'static str, LogicalType)],
) -> Result<Vec<Value>, String> {
    let mut out = Vec::with_capacity(edges.len());
    for (from, to, props) in edges {
        let mut fields: Vec<(String, Value)> = vec![
            ("from".to_string(), Value::String(from.clone())),
            ("to".to_string(), Value::String(to.clone())),
        ];
        for (col, ty) in prop_order {
            let lit = props.iter().find(|(k, _)| k == *col).map(|(_, v)| v.as_str());
            fields.push(((*col).to_string(), literal_to_value(lit, ty, col)?));
        }
        out.push(Value::Struct(fields));
    }
    Ok(out)
}

/// Converts a caller-supplied Cypher literal (the legacy pre-UNWIND format:
/// `'foo'` for strings, `10` for ints, `true`/`false` for bools, `1.23` for
/// doubles) into a typed `Value` matching the declared column type.
///
/// A missing literal yields a typed `Value::Null` — lbug accepts NULL in
/// typed columns. Parsing preserves the security guarantees of cypher_str
/// because the string payload is now passed as a typed parameter, not
/// interpolated into Cypher text.
fn literal_to_value(
    lit: Option<&str>,
    ty: &LogicalType,
    col: &str,
) -> Result<Value, String> {
    let Some(raw) = lit else {
        return Ok(Value::Null(ty.clone()));
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(Value::Null(ty.clone()));
    }
    match ty {
        LogicalType::String => Ok(Value::String(unwrap_cypher_string(trimmed))),
        LogicalType::Int64 => trimmed
            .parse::<i64>()
            .map(Value::Int64)
            .map_err(|e| format!("column {col}: expected INT64, got {raw:?}: {e}")),
        LogicalType::Bool => match trimmed.to_ascii_lowercase().as_str() {
            "true" => Ok(Value::Bool(true)),
            "false" | "" => Ok(Value::Bool(false)),
            _ => Err(format!("column {col}: expected BOOL, got {raw:?}")),
        },
        LogicalType::Double => trimmed
            .parse::<f64>()
            .map(Value::Double)
            .map_err(|e| format!("column {col}: expected DOUBLE, got {raw:?}: {e}")),
        other => Err(format!(
            "column {col}: unsupported bulk-insert type {other:?}"
        )),
    }
}

/// Inverse of `cypher_str`: takes a caller-supplied literal (either an
/// already-quoted Cypher string like `'foo\'s'` or a bare value) and
/// returns the raw string content. The unescape rules mirror cypher_str:
///   \\ → \    \'  → '
/// Any unquoted input is returned as-is (callers sometimes pass raw
/// strings for non-id columns; treat those as literal contents).
fn unwrap_cypher_string(s: &str) -> String {
    if s.len() >= 2 && s.starts_with('\'') && s.ends_with('\'') {
        let inner = &s[1..s.len() - 1];
        let mut out = String::with_capacity(inner.len());
        let mut chars = inner.chars();
        while let Some(ch) = chars.next() {
            if ch == '\\' {
                match chars.next() {
                    Some('\\') => out.push('\\'),
                    Some('\'') => out.push('\''),
                    Some(other) => { out.push('\\'); out.push(other); }
                    None => out.push('\\'),
                }
            } else {
                out.push(ch);
            }
        }
        out
    } else {
        s.to_string()
    }
}

fn value_to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Int64(n) => n.to_string(),
        Value::Int32(n) => n.to_string(),
        Value::Int16(n) => n.to_string(),
        Value::Int8(n) => n.to_string(),
        Value::UInt64(n) => n.to_string(),
        Value::UInt32(n) => n.to_string(),
        Value::UInt16(n) => n.to_string(),
        Value::UInt8(n) => n.to_string(),
        Value::Double(n) => n.to_string(),
        Value::Float(n) => n.to_string(),
        _ => format!("{v:?}"),
    }
}

fn value_to_u64(v: &Value) -> u64 {
    match v {
        Value::Int64(n) => *n as u64,
        Value::UInt64(n) => *n,
        _ => 0,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_create_and_query() {
        let dir = std::env::temp_dir().join("graph_store_test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create temp dir");
        let db_path = dir.join("testdb");

        let store =
            GraphStore::open_or_create(&db_path).expect("open_or_create");
        store.create_schema().expect("create_schema");

        store
            .insert_node(
                NODE_FUNCTION,
                &[
                    ("id", "'fn1'"),
                    ("name", "'main'"),
                    ("qualified_name", "'crate::main'"),
                    ("start_line", "1"),
                    ("end_line", "10"),
                    ("visibility", "'pub'"),
                    ("is_async", "false"),
                ],
            )
            .expect("insert Function node");

        let qr = store
            .execute_query(
                "MATCH (f:Function) WHERE f.name = 'main' RETURN f.name",
            )
            .expect("query");
        assert_eq!(qr.columns, vec!["f.name"]);
        assert!(!qr.rows.is_empty(), "expected at least one row");
        assert_eq!(qr.rows[0][0], "main");

        let count = store.node_count().expect("node_count");
        assert!(count >= 1, "expected node_count >= 1, got {count}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_bulk_insert_nodes_and_edges() {
        let dir = std::env::temp_dir().join("graph_store_bulk_test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create temp dir");
        let db_path = dir.join("testdb");

        let store = GraphStore::open_or_create(&db_path).expect("open");
        store.create_schema().expect("schema");

        let mut files: Vec<Vec<(String, String)>> = Vec::new();
        for i in 0..7 {
            let id = format!("f{i}.rs");
            files.push(vec![
                ("id".into(), cypher_str(&id)),
                ("path".into(), cypher_str(&id)),
                ("name".into(), cypher_str(&id)),
                ("extension".into(), cypher_str("rs")),
                ("size_bytes".into(), "0".into()),
            ]);
        }
        let n = store.bulk_insert_nodes("File", &files).expect("bulk nodes");
        assert_eq!(n, 7);

        let mut edges: Vec<(String, String, Vec<(String, String)>)> = Vec::new();
        for i in 0..6 {
            edges.push((format!("f{i}.rs"), format!("f{}.rs", i + 1), Vec::new()));
        }
        let e = store
            .bulk_insert_edges("Imports_File_File", &edges)
            .expect("bulk edges");
        assert_eq!(e, 6);

        let qr = store
            .execute_query("MATCH (f:File) RETURN count(f)")
            .expect("count");
        let c: u64 = qr.rows[0][0].parse().unwrap_or(0);
        assert_eq!(c, 7);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_cypher_str_escape_rules() {
        // Backslash must be escaped FIRST, then quote.
        // Input: `foo'bar`  → literal should contain `\'`.
        assert_eq!(cypher_str("foo'bar"), "'foo\\'bar'");
        // Input: `a\b`      → literal should contain `\\`.
        assert_eq!(cypher_str("a\\b"), "'a\\\\b'");
        // Input: `x\'y` (naive replace('\'', "\\'") would produce `'x\\\\'y'`
        // which re-opens the literal after `\\`). Our rule:
        //   \  -> \\     (escape backslash first)
        //   '  -> \'     (then escape quotes)
        // So `x\'y` becomes `x\\\'y` inside the quotes.
        assert_eq!(cypher_str("x\\'y"), "'x\\\\\\'y'");
    }

    #[test]
    fn test_cypher_injection_rejected() {
        // An id containing an unescaped single quote used to allow a caller
        // to inject arbitrary Cypher (including DETACH DELETE). After the
        // C1 fix, the injection attempt becomes an ordinary string literal
        // that round-trips through the DB safely.
        let dir = std::env::temp_dir().join("graph_store_inject_test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create temp dir");
        let db_path = dir.join("testdb");

        let store =
            GraphStore::open_or_create(&db_path).expect("open_or_create");
        store.create_schema().expect("create_schema");

        // Insert two File nodes so insert_edge has something to MATCH.
        store
            .insert_node(
                NODE_FILE,
                &[
                    ("id", &cypher_str("src/a.rs")),
                    ("path", &cypher_str("src/a.rs")),
                    ("name", &cypher_str("a.rs")),
                    ("extension", &cypher_str("rs")),
                    ("size_bytes", "0"),
                ],
            )
            .expect("insert file a");

        // Adversarial id: contains `'` and would-be Cypher payload.
        let evil_id = "src/b.rs' DETACH DELETE n //";
        store
            .insert_node(
                NODE_FILE,
                &[
                    ("id", &cypher_str(evil_id)),
                    ("path", &cypher_str(evil_id)),
                    ("name", &cypher_str("b.rs")),
                    ("extension", &cypher_str("rs")),
                    ("size_bytes", "0"),
                ],
            )
            .expect("insert file b");

        // Edge insert used to be the injection site. Must succeed safely now.
        store
            .insert_edge("Imports_File_File", "src/a.rs", evil_id, &[])
            .expect("insert_edge with quote-containing id must not inject");

        // If injection had worked, DETACH DELETE would have wiped nodes.
        // Verify both File nodes are still present.
        let cnt = store
            .execute_query("MATCH (f:File) RETURN count(f)")
            .expect("count query");
        let count_val: u64 = cnt.rows[0][0].parse().unwrap_or(0);
        assert_eq!(count_val, 2, "injection attempt must not delete nodes");

        let _ = fs::remove_dir_all(&dir);
    }
}
