// graph_store::writes — the write-side surface of `GraphStore`.
//
// Moved verbatim from mod.rs (Fowler "Move Function", no behavior change) to
// keep each `impl GraphStore` block within the §4.3 size cap along a concern
// boundary: this file owns every method that MUTATES the graph (single and
// bulk inserts, resolution-state flips); mod.rs keeps lifecycle and the
// read/query surface.

use super::columns::{edge_column_types, node_column_types};
use super::ddl::parse_rel_endpoints;
use super::serialize::{
    build_edge_struct_rows, build_edge_unwind, build_node_unwind, build_struct_rows,
    edge_prop_order, format_props, node_prop_order,
};
use super::{cypher_str, GraphStore, PropEdge, BULK_BATCH_SIZE};
use lbug::{LogicalType, Value};

impl GraphStore {
    /// Inserts a single node. `properties` are `(key, cypher_literal)` pairs.
    /// Values are interpolated as-is into Cypher — caller must quote strings.
    pub fn insert_node(&self, label: &str, properties: &[(&str, &str)]) -> Result<(), String> {
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
    pub fn bulk_insert_edges(&self, rel_table: &str, edges: &[PropEdge]) -> Result<u64, String> {
        if edges.is_empty() {
            return Ok(0);
        }
        let (from_label, to_label) = parse_rel_endpoints(rel_table)?;
        let prop_schema = edge_column_types(rel_table);
        let prop_order = edge_prop_order(edges, prop_schema);
        let (cypher, row_type) = build_edge_unwind(rel_table, from_label, to_label, &prop_order);
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
        // source: Kuzu PK-index scan — inline `{id: ..}` avoids the A×B
        // CrossProduct the comma+WHERE form plans (see build_edge_unwind).
        let cypher = format!(
            "MATCH (a:{from_label} {{id: {from_lit}}}) \
             MATCH (b:{to_label} {{id: {to_lit}}}) \
             CREATE (a)-[:{rel_type}{props_clause}]->(b)"
        );
        self.run(&cypher)?;
        Ok(())
    }

    /// Inserts an edge only when that exact `(rel_type, from_id, to_id)` triple
    /// is absent, and reports whether the graph now holds it.
    ///
    /// Why (review finding 3): `insert_edge` emits `CREATE`, which appends a
    /// parallel edge every time it runs. That is correct for the static
    /// resolver, which deduplicates in its `EdgeBuffer` before writing, but the
    /// LSP pass writes one edge per call site as it goes. It has no in-memory
    /// set, and it flips `CallSite.is_resolved` only at end of run — so an
    /// interrupted run leaves edges written and sites still marked unresolved,
    /// and the next run inserts every one of them a second time, inflating
    /// `get_impact` counts with no way to tell the duplicates apart or clean
    /// them up. Two call sites in one caller reaching the same callee produce
    /// the same duplication inside a single run.
    ///
    /// Checking first makes the write idempotent regardless of run boundaries,
    /// which is the property the caller actually needs. The probe is a
    /// PK-indexed two-endpoint seek, and it runs once per candidate edge
    /// against an LSP round trip that is orders of magnitude slower.
    pub fn insert_edge_if_absent(
        &self,
        rel_type: &str,
        from_id: &str,
        to_id: &str,
        properties: &[(&str, &str)],
    ) -> Result<(), String> {
        if self.edge_exists(rel_type, from_id, to_id)? {
            return Ok(());
        }
        self.insert_edge(rel_type, from_id, to_id, properties)
    }

    /// True when `rel_type` already connects `from_id` to `to_id`.
    pub fn edge_exists(&self, rel_type: &str, from_id: &str, to_id: &str) -> Result<bool, String> {
        let (from_label, to_label) = parse_rel_endpoints(rel_type)?;
        let cypher = format!(
            "MATCH (a:{from_label} {{id: {}}})-[r:{rel_type}]->(b:{to_label} {{id: {}}}) \
             RETURN r LIMIT 1",
            cypher_str(from_id),
            cypher_str(to_id)
        );
        Ok(!self.execute_query(&cypher)?.rows.is_empty())
    }

    /// Old graphs discarded Rust entry attributes. Adding an empty column
    /// cannot recover them: reparse every file through the full-index handler.
    /// Read-only so callers can check compatibility before mutating a graph.
    pub fn require_entry_metadata(&self) -> Result<(), String> {
        let info = self.execute_query("CALL table_info('Function') RETURN *")?;
        if info
            .rows
            .iter()
            .any(|row| row.get(1).is_some_and(|name| name == "entry_kind"))
        {
            Ok(())
        } else {
            Err("graph lacks Rust entry attribute metadata; full reindex required (index_codebase with full: true)".into())
        }
    }

    /// Adds `column` to node table `label` when the table does not already
    /// carry it, and reports whether it had to be added.
    ///
    /// Why (review finding 4): a graph indexed before a column existed does not
    /// grow one when the code that reads it ships. Referencing a missing
    /// property is a hard binder error ("Binder exception: Cannot find property
    /// .. for n", measured 2026-08-24 on lbug 0.19.1), so a reader added later
    /// takes the whole tool down on an older graph rather than degrading. The
    /// column list comes from `table_info`, so this is a no-op on an
    /// up-to-date graph and never depends on parsing an error string.
    ///
    /// `definition` is the DDL fragment after the column name, e.g.
    /// `"BOOLEAN DEFAULT false"`. A DEFAULT backfills the existing rows.
    pub fn ensure_node_column(
        &self,
        label: &str,
        column: &str,
        definition: &str,
    ) -> Result<bool, String> {
        let info =
            self.execute_query(&format!("CALL table_info({}) RETURN *", cypher_str(label)))?;
        // table_info columns are (property id, name, type, default, primary key).
        let present = info
            .rows
            .iter()
            .any(|row| row.get(1).is_some_and(|name| name == column));
        if present {
            return Ok(false);
        }
        self.run(&format!("ALTER TABLE {label} ADD {column} {definition}"))?;
        Ok(true)
    }

    /// Inserts one `FileContent` row: the file's zstd-compressed source
    /// bytes, keyed by the file's relative path (matches `File.id`).
    ///
    /// Bypasses the Cypher-string-literal path (`insert_node`/`cypher_str`)
    /// entirely and the bulk UNWIND path (`bulk_insert_nodes`) — both go
    /// through `literal_to_value`, which parses UTF-8 text into typed
    /// values; compressed bytes are not valid UTF-8 and cannot be safely
    /// represented as a Cypher string literal. This builds a typed
    /// `Value::Blob` directly and binds it via a prepared statement with
    /// named parameters, the same FFI path `bulk_insert_nodes` uses for its
    /// struct rows, just without the UNWIND/List wrapper (one row per call).
    ///
    /// Cached like every other prepared statement on this store (see
    /// `run_prepared`): the cypher text is identical on every call (only the
    /// parameter VALUES differ per file), so caching turns N calls into one
    /// plan + N binds instead of N plans.
    pub(crate) fn insert_file_content(
        &self,
        file_id: &str,
        content_zstd: Vec<u8>,
        original_size: i64,
    ) -> Result<(), String> {
        let compressed_size = content_zstd.len() as i64;
        let cypher = "CREATE (:FileContent {id: $id, content_zstd: $content, \
                       original_size: $original_size, compressed_size: $compressed_size})";
        let params = vec![
            ("id", Value::String(file_id.to_string())),
            ("content", Value::Blob(content_zstd)),
            ("original_size", Value::Int64(original_size)),
            ("compressed_size", Value::Int64(compressed_size)),
        ];
        self.run_prepared_params(cypher, params)
    }

    /// Flips `is_resolved = true` on all nodes of `label` whose id is in `ids`.
    ///
    /// Uses the codebase's prepared-UNWIND convention (parameterized `$rows`, no
    /// Cypher string interpolation of data — mirrors bulk_insert_nodes) so a
    /// codebase with tens of thousands of resolved imports/calls costs one
    /// prepared statement per chunk. `label` is a fixed schema constant
    /// ("Import"/"CallSite"), safe to embed. source: stages/stage-3.md §10.4.
    pub(crate) fn mark_nodes_resolved(&self, label: &str, ids: &[&str]) -> Result<(), String> {
        if ids.is_empty() {
            return Ok(());
        }
        // source: Kuzu PK-index scan — inline `{id: rid}` seeks the index per
        // row; the `MATCH (n) WHERE n.id = rid` form scans all N nodes per row
        // (O(rows·N)) on large graphs. Same fix class as the edge queries.
        let cypher =
            format!("UNWIND $rows AS rid MATCH (n:{label} {{id: rid}}) SET n.is_resolved = true");
        for chunk in ids.chunks(BULK_BATCH_SIZE) {
            let values: Vec<Value> = chunk
                .iter()
                .map(|id| Value::String((*id).to_string()))
                .collect();
            let list = Value::List(LogicalType::String, values);
            self.run_prepared(&cypher, list)?;
        }
        Ok(())
    }
}
