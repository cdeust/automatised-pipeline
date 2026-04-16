// scalability_bench — verifies that bulk-insert refactor keeps large
// codebases indexable in reasonable wall-clock time.
//
// Generates a synthetic 500-file Rust fixture inline, indexes it end-to-end,
// and asserts the run completes under 60 s on a warm machine.
//
// source: Fermi audit April 2026 — the dense per-row CREATE path could not
// scale past ~10K LOC; after bulk_insert_nodes/edges the same workload must
// finish in a small multiple of parse time.

use ai_architect_mcp::indexer;
use std::fs;
use std::path::PathBuf;
use std::time::Instant;

/// Number of synthetic Rust files generated for the benchmark.
/// 500 files × ~20 LOC each ≈ 10K LOC, roughly the size of this crate's src/.
const NUM_FILES: usize = 500;

/// Per-test wall-clock budget in milliseconds.
/// 60 s is far more than the expected ~5 s on a warm M-series Mac; the goal
/// is to catch catastrophic regressions, not micro-variations.
const MAX_WALL_CLOCK_MS: u128 = 60_000;

#[test]
fn test_index_500_file_synthetic_fixture() {
    let tmp_root = std::env::temp_dir().join(format!(
        "scalability_bench_{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&tmp_root);
    let src_dir = tmp_root.join("src");
    fs::create_dir_all(&src_dir).expect("create src dir");

    generate_fixture(&src_dir);

    let graph_dir = tmp_root.join("graph");
    let start = Instant::now();
    let result = indexer::index_codebase(&src_dir, &graph_dir)
        .expect("index_codebase should succeed");
    let elapsed = start.elapsed();

    assert_eq!(
        result.files_indexed, NUM_FILES as u64,
        "expected {NUM_FILES} files indexed, got {}",
        result.files_indexed
    );
    assert!(
        result.node_count > (NUM_FILES as u64) * 5,
        "expected >{} nodes, got {}",
        NUM_FILES * 5,
        result.node_count
    );
    eprintln!(
        "scalability_bench: indexed {} files ({} nodes, {} edges) in {} ms",
        NUM_FILES,
        result.node_count,
        result.edge_count,
        elapsed.as_millis()
    );
    assert!(
        elapsed.as_millis() < MAX_WALL_CLOCK_MS,
        "indexing {NUM_FILES} files took {} ms, budget is {MAX_WALL_CLOCK_MS} ms",
        elapsed.as_millis()
    );

    let _ = fs::remove_dir_all(&tmp_root);
}

/// Produces `NUM_FILES` synthetic Rust files. Each file defines:
///   - 1 struct with 2 fields,
///   - 1 enum with 2 variants,
///   - 1 trait with 1 method,
///   - 2 free functions, one of which calls the other.
/// This exercises every node label the indexer extracts, plus ExtractedRef
/// edges of Defines, HasMethod, HasField, HasVariant, and CallSite.
fn generate_fixture(src_dir: &PathBuf) {
    for i in 0..NUM_FILES {
        let path = src_dir.join(format!("mod_{i:04}.rs"));
        fs::write(&path, synthesize_file(i)).expect("write synthetic file");
    }
}

fn synthesize_file(idx: usize) -> String {
    format!(
        r#"pub struct Config{idx} {{
    pub name: String,
    pub retries: u32,
}}

pub enum Status{idx} {{
    Active,
    Inactive,
}}

pub trait Handler{idx} {{
    fn handle(&self, input: &str) -> String;
}}

pub fn do_work_{idx}() -> u32 {{
    helper_{idx}()
}}

pub fn helper_{idx}() -> u32 {{
    42
}}
"#
    )
}
