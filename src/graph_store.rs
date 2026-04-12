// graph_store — LadybugDB port for the code-intelligence graph.
//
// This module wraps `lbug::Database` + `lbug::Connection` behind a clean
// interface. The rest of the codebase depends only on `GraphStore` methods,
// never on `lbug` directly. If we ever swap the backing store, only this
// file changes.

use lbug::{Connection, Database, SystemConfig, Value};
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
        Ok(GraphStore { _db: db, conn })
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
        let cypher = format!(
            "MATCH (a:{from_label}), (b:{to_label}) \
             WHERE a.id = '{from_id}' AND b.id = '{to_id}' \
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

}
