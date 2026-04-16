// tfidf_size_report — one-off measurement harness. Indexes the crate's own
// src/ and reports the on-disk size of the sparse TF-IDF index so the
// scalability report can compare it against the previous dense CSV format.

use ai_architect_mcp::graph_store::GraphStore;
use ai_architect_mcp::indexer;
use ai_architect_mcp::search::vector;
use std::fs;

#[test]
fn report_tfidf_index_size() {
    let tmp = std::env::temp_dir().join(format!("tfidf_size_{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();
    let graph_dir = tmp.join("graph");
    let index_dir = tmp.join("index");

    indexer::index_codebase(std::path::Path::new("src"), &graph_dir)
        .expect("index_codebase");
    let store = GraphStore::open_or_create(&graph_dir).expect("open graph");
    let doc_count = vector::build_index(&store, &index_dir).expect("build index");

    let size = fs::metadata(index_dir.join("vector_index.bin"))
        .expect("index file")
        .len();
    // Read sparse binary: u32 magic, u32 version, u32 vocab_size, u32 doc_count
    let bytes = fs::read(index_dir.join("vector_index.bin")).unwrap();
    let vocab_size = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
    // Old CSV format: each line per doc had |V| f32 formatted as "%.6}"
    // (~9 chars) joined by commas plus four tab-separated string columns.
    // Conservative estimate: 10 bytes per term per doc.
    let old_dense_bytes = (doc_count as u64) * (vocab_size as u64) * 10
        + (doc_count as u64) * 100
        + (vocab_size as u64) * 20;
    eprintln!(
        "tfidf_size_report: docs={doc_count}, vocab={vocab_size}, \
         sparse_bytes={size}, old_dense_bytes~={old_dense_bytes}, \
         ratio={:.1}x",
        old_dense_bytes as f64 / size as f64
    );
    assert!(doc_count > 0);
    assert!(size > 0);

    let _ = fs::remove_dir_all(&tmp);
}
