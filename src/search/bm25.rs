// search::bm25 — Tantivy-backed BM25 full-text search index.
//
// Builds a Tantivy index over all symbol nodes extracted from the graph.
// Documents: qualified_name (stored+indexed), name (indexed, boosted),
// label (stored+faceted), file_path (stored).
//
// Source: Tantivy 0.26 (quickwit-oss, MIT). BM25 scoring is Tantivy's
// default ranking model.

use std::path::Path;
use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::{Schema, STORED, TEXT, Field, Value as _};
use tantivy::{Index, IndexWriter, TantivyDocument, doc};

use crate::graph_store::GraphStore;

// ---------------------------------------------------------------------------
// Schema fields
// ---------------------------------------------------------------------------

pub struct Bm25Fields {
    pub qualified_name: Field,
    pub name: Field,
    pub label: Field,
    pub file_path: Field,
}

pub fn build_schema() -> (Schema, Bm25Fields) {
    let mut builder = Schema::builder();
    let qualified_name = builder.add_text_field("qualified_name", TEXT | STORED);
    let name = builder.add_text_field("name", TEXT | STORED);
    let label = builder.add_text_field("label", STORED);
    let file_path = builder.add_text_field("file_path", STORED);
    let schema = builder.build();
    (schema, Bm25Fields { qualified_name, name, label, file_path })
}

// ---------------------------------------------------------------------------
// Index building
// ---------------------------------------------------------------------------

const SEARCHABLE_LABELS: &[&str] = &[
    "Function", "Method", "Struct", "Enum", "Trait",
    "Module", "Constant", "TypeAlias",
];

/// Builds a Tantivy BM25 index from all symbol nodes in the graph.
/// Writes the index to `index_dir`.
pub fn build_index(store: &GraphStore, index_dir: &Path) -> Result<usize, String> {
    std::fs::create_dir_all(index_dir)
        .map_err(|e| format!("create index dir: {e}"))?;

    let (schema, fields) = build_schema();
    let index = Index::create_in_dir(index_dir, schema)
        .map_err(|e| format!("tantivy create index: {e}"))?;
    let mut writer: IndexWriter = index.writer(50_000_000)
        .map_err(|e| format!("tantivy writer: {e}"))?;

    let mut doc_count = 0usize;
    for &label in SEARCHABLE_LABELS {
        let cypher = format!(
            "MATCH (n:{label}) RETURN n.qualified_name, n.name, n.id"
        );
        let qr = match store.execute_query(&cypher) {
            Ok(qr) => qr,
            Err(_) => continue,
        };
        for row in &qr.rows {
            if row.len() < 3 { continue; }
            let qn = &row[0];
            let name_val = &row[1];
            let file_path = extract_file_path(qn);

            // Tokenize name by splitting on _ and :: for better BM25 matching.
            // "handle_tool_call" → "handle tool call" so BM25 finds "handle tool".
            let name_tokenized = tokenize_symbol(name_val);
            let qn_tokenized = tokenize_symbol(qn);

            writer.add_document(doc!(
                fields.qualified_name => qn_tokenized,
                fields.name => name_tokenized,
                fields.label => label.to_string(),
                fields.file_path => file_path,
            )).map_err(|e| format!("tantivy add doc: {e}"))?;
            doc_count += 1;
        }
    }

    writer.commit().map_err(|e| format!("tantivy commit: {e}"))?;
    Ok(doc_count)
}

// ---------------------------------------------------------------------------
// Querying
// ---------------------------------------------------------------------------

/// A single BM25 search result with its score.
#[allow(dead_code)]
pub struct Bm25Result {
    pub qualified_name: String,
    pub name: String,
    pub label: String,
    pub file_path: String,
    pub score: f32,
}

/// Queries the Tantivy index at `index_dir` and returns ranked results.
pub fn query_index(
    index_dir: &Path,
    query_str: &str,
    limit: usize,
) -> Result<Vec<Bm25Result>, String> {
    if !index_dir.exists() {
        return Ok(Vec::new());
    }

    let (schema, fields) = build_schema();
    let index = Index::open_in_dir(index_dir)
        .map_err(|e| format!("tantivy open index: {e}"))?;
    let reader = index.reader()
        .map_err(|e| format!("tantivy reader: {e}"))?;
    let searcher = reader.searcher();

    // Tokenize query the same way we tokenize symbol names
    let tokenized_query = tokenize_symbol(query_str);

    let mut parser = QueryParser::for_index(&index, vec![fields.name, fields.qualified_name]);
    // Boost name field 2x over qualified_name
    parser.set_field_boost(fields.name, 2.0);

    let query = parser.parse_query(&tokenized_query)
        .map_err(|e| format!("tantivy parse query: {e}"))?;

    let top_docs = searcher.search(&query, &TopDocs::with_limit(limit))
        .map_err(|e| format!("tantivy search: {e}"))?;

    let mut results = Vec::with_capacity(top_docs.len());
    for (score, doc_addr) in top_docs {
        let doc: TantivyDocument = searcher.doc(doc_addr)
            .map_err(|e| format!("tantivy doc retrieve: {e}"))?;
        let qn = field_text(&doc, &schema, fields.qualified_name);
        let name = field_text(&doc, &schema, fields.name);
        let label = field_text(&doc, &schema, fields.label);
        let fp = field_text(&doc, &schema, fields.file_path);

        // Reverse the tokenization for display
        results.push(Bm25Result {
            qualified_name: qn,
            name,
            label,
            file_path: fp,
            score,
        });
    }
    Ok(results)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn field_text(doc: &TantivyDocument, _schema: &Schema, field: Field) -> String {
    doc.get_first(field)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

fn extract_file_path(qualified_name: &str) -> String {
    if let Some(idx) = qualified_name.find("::") {
        qualified_name[..idx].to_string()
    } else {
        qualified_name.to_string()
    }
}

/// Tokenizes a symbol name for BM25 indexing/querying.
/// Splits on `_`, `::`, `/`, `.`, and camelCase boundaries.
/// "handle_tool_call" → "handle tool call"
/// "GraphStore" → "graph store"
/// "src/main.rs::handle_tool_call" → "src main rs handle tool call"
pub fn tokenize_symbol(s: &str) -> String {
    let mut tokens = Vec::new();
    // First split on :: / _ . /
    for part in s.split(|c: char| c == ':' || c == '_' || c == '/' || c == '.') {
        if part.is_empty() { continue; }
        // Split camelCase
        let mut current = String::new();
        for ch in part.chars() {
            if ch.is_uppercase() && !current.is_empty() {
                tokens.push(current.to_lowercase());
                current = String::new();
            }
            current.push(ch);
        }
        if !current.is_empty() {
            tokens.push(current.to_lowercase());
        }
    }
    tokens.join(" ")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tokenize_symbol() {
        assert_eq!(tokenize_symbol("handle_tool_call"), "handle tool call");
        assert_eq!(tokenize_symbol("GraphStore"), "graph store");
        assert_eq!(
            tokenize_symbol("src/main.rs::handle_tool_call"),
            "src main rs handle tool call"
        );
    }

    #[test]
    fn test_extract_file_path() {
        assert_eq!(extract_file_path("src/main.rs::main"), "src/main.rs");
        assert_eq!(extract_file_path("src/lib.rs"), "src/lib.rs");
    }
}
