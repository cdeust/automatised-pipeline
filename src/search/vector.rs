// search::vector — TF-IDF vector search for semantic similarity.
//
// Neural embeddings (fastembed) were evaluated but add ~200MB ONNX Runtime
// dep. TF-IDF over tokenized symbol names provides semantic matching
// (e.g., "process incoming requests" finds "handle_tool_call" via shared
// vocabulary patterns) with zero additional dependencies.
//
// Tokenization: split on `_`, `::`, camelCase → lowercase terms.
// Weighting: TF-IDF (term frequency * inverse document frequency).
// Similarity: cosine similarity between query vector and document vectors.

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::Path;

use crate::graph_store::GraphStore;
use super::bm25::tokenize_symbol;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A single document in the vector index.
struct VectorDoc {
    qualified_name: String,
    name: String,
    label: String,
    file_path: String,
    tf_idf: Vec<f32>,
}

/// In-memory vector index. Built once, queried many times.
pub struct VectorIndex {
    docs: Vec<VectorDoc>,
    vocabulary: Vec<String>,
}

pub struct VectorResult {
    pub qualified_name: String,
    #[allow(dead_code)]
    pub name: String,
    #[allow(dead_code)]
    pub label: String,
    #[allow(dead_code)]
    pub file_path: String,
    #[allow(dead_code)]
    pub score: f32,
}

// ---------------------------------------------------------------------------
// Index building
// ---------------------------------------------------------------------------

const SEARCHABLE_LABELS: &[&str] = &[
    "Function", "Method", "Struct", "Enum", "Trait",
    "Module", "Constant", "TypeAlias",
];

/// Builds a TF-IDF vector index from all symbol nodes in the graph.
/// Persists to `index_dir/vector_index.bin` for later loading.
pub fn build_index(store: &GraphStore, index_dir: &Path) -> Result<usize, String> {
    std::fs::create_dir_all(index_dir)
        .map_err(|e| format!("create vector index dir: {e}"))?;

    // Collect all documents with their raw token lists
    let mut raw_docs: Vec<(String, String, String, String, Vec<String>)> = Vec::new();
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
            // Combine name + label + qualified_name for richer vectors
            let combined = format!("{} {} {}", name_val, label, qn);
            let tokens = tokenize_to_terms(&combined);
            raw_docs.push((
                qn.clone(),
                name_val.clone(),
                label.to_string(),
                file_path,
                tokens,
            ));
        }
    }

    if raw_docs.is_empty() {
        return Ok(0);
    }

    // Build vocabulary and IDF
    let (vocabulary, idf) = build_vocabulary_idf(&raw_docs);

    // Build TF-IDF vectors
    let docs: Vec<VectorDoc> = raw_docs.into_iter().map(|(qn, name, label, fp, tokens)| {
        let tf_idf = compute_tf_idf(&tokens, &vocabulary, &idf);
        VectorDoc { qualified_name: qn, name, label, file_path: fp, tf_idf }
    }).collect();

    let doc_count = docs.len();

    // Persist to disk
    let index = VectorIndex { docs, vocabulary };
    save_index(&index, index_dir)?;

    Ok(doc_count)
}

/// Loads a persisted vector index and queries it with cosine similarity.
pub fn query_index(
    index_dir: &Path,
    query_str: &str,
    limit: usize,
) -> Result<Vec<VectorResult>, String> {
    let index_path = index_dir.join("vector_index.bin");
    if !index_path.exists() {
        return Ok(Vec::new());
    }

    let index = load_index(index_dir)?;
    if index.docs.is_empty() {
        return Ok(Vec::new());
    }

    // Compute query vector
    let query_tokens = tokenize_to_terms(query_str);
    // Need IDF from the index — reconstruct from stored vocabulary + docs
    let idf = reconstruct_idf(&index);
    let query_vec = compute_tf_idf(&query_tokens, &index.vocabulary, &idf);

    // Score all docs by cosine similarity
    let mut scored: Vec<(usize, f32)> = index.docs.iter().enumerate()
        .map(|(i, doc)| (i, cosine_similarity(&query_vec, &doc.tf_idf)))
        .filter(|(_, s)| *s > 0.0)
        .collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(limit);

    Ok(scored.into_iter().map(|(i, score)| {
        let doc = &index.docs[i];
        VectorResult {
            qualified_name: doc.qualified_name.clone(),
            name: doc.name.clone(),
            label: doc.label.clone(),
            file_path: doc.file_path.clone(),
            score,
        }
    }).collect())
}

// ---------------------------------------------------------------------------
// TF-IDF computation
// ---------------------------------------------------------------------------

fn tokenize_to_terms(s: &str) -> Vec<String> {
    tokenize_symbol(s)
        .split_whitespace()
        .map(|t| t.to_string())
        .collect()
}

fn build_vocabulary_idf(
    raw_docs: &[(String, String, String, String, Vec<String>)],
) -> (Vec<String>, Vec<f32>) {
    // Count document frequency for each term
    let mut df: HashMap<String, usize> = HashMap::new();
    for (_, _, _, _, tokens) in raw_docs {
        let unique: std::collections::HashSet<&String> = tokens.iter().collect();
        for t in unique {
            *df.entry(t.clone()).or_insert(0) += 1;
        }
    }

    let n = raw_docs.len() as f32;
    let mut vocabulary: Vec<String> = df.keys().cloned().collect();
    vocabulary.sort();

    let idf: Vec<f32> = vocabulary.iter()
        .map(|term| {
            let d = df.get(term).copied().unwrap_or(1) as f32;
            (n / d).ln() + 1.0 // smoothed IDF
        })
        .collect();

    (vocabulary, idf)
}

fn compute_tf_idf(tokens: &[String], vocabulary: &[String], idf: &[f32]) -> Vec<f32> {
    // Term frequency
    let mut tf: HashMap<&str, f32> = HashMap::new();
    for t in tokens {
        *tf.entry(t.as_str()).or_insert(0.0) += 1.0;
    }
    let max_tf = tf.values().copied().fold(0.0f32, f32::max).max(1.0);

    // Build vector aligned to vocabulary
    vocabulary.iter().enumerate().map(|(i, term)| {
        let raw_tf = tf.get(term.as_str()).copied().unwrap_or(0.0);
        let norm_tf = 0.5 + 0.5 * (raw_tf / max_tf); // augmented TF
        norm_tf * idf[i] * if raw_tf > 0.0 { 1.0 } else { 0.0 }
    }).collect()
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() { return 0.0; }
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a < 1e-10 || norm_b < 1e-10 { return 0.0; }
    dot / (norm_a * norm_b)
}

fn reconstruct_idf(index: &VectorIndex) -> Vec<f32> {
    let n = index.docs.len() as f32;
    let mut df = vec![0usize; index.vocabulary.len()];
    for doc in &index.docs {
        for (i, val) in doc.tf_idf.iter().enumerate() {
            if *val > 0.0 {
                df[i] += 1;
            }
        }
    }
    df.iter().map(|&d| {
        let d = d.max(1) as f32;
        (n / d).ln() + 1.0
    }).collect()
}

// ---------------------------------------------------------------------------
// Persistence — simple line-based text format
// ---------------------------------------------------------------------------

fn save_index(index: &VectorIndex, dir: &Path) -> Result<(), String> {
    let path = dir.join("vector_index.bin");
    let mut f = std::io::BufWriter::new(
        std::fs::File::create(&path)
            .map_err(|e| format!("create vector index: {e}"))?,
    );

    // Header: vocabulary size, doc count
    writeln!(f, "{} {}", index.vocabulary.len(), index.docs.len())
        .map_err(|e| format!("write header: {e}"))?;

    // Vocabulary
    for term in &index.vocabulary {
        writeln!(f, "{}", term).map_err(|e| format!("write vocab: {e}"))?;
    }

    // Documents: qn\tname\tlabel\tfile_path\tvec_values...
    for doc in &index.docs {
        let vec_str: String = doc.tf_idf.iter()
            .map(|v| format!("{:.6}", v))
            .collect::<Vec<_>>()
            .join(",");
        writeln!(f, "{}\t{}\t{}\t{}\t{}",
            doc.qualified_name, doc.name, doc.label, doc.file_path, vec_str
        ).map_err(|e| format!("write doc: {e}"))?;
    }

    Ok(())
}

fn load_index(dir: &Path) -> Result<VectorIndex, String> {
    let path = dir.join("vector_index.bin");
    let file = std::fs::File::open(&path)
        .map_err(|e| format!("open vector index: {e}"))?;
    let reader = std::io::BufReader::new(file);
    let mut lines = reader.lines();

    // Header
    let header = lines.next()
        .ok_or("empty vector index")?
        .map_err(|e| format!("read header: {e}"))?;
    let parts: Vec<&str> = header.split_whitespace().collect();
    if parts.len() < 2 {
        return Err("malformed vector index header".to_string());
    }
    let vocab_size: usize = parts[0].parse()
        .map_err(|_| "bad vocab size".to_string())?;
    let doc_count: usize = parts[1].parse()
        .map_err(|_| "bad doc count".to_string())?;

    // Vocabulary
    let mut vocabulary = Vec::with_capacity(vocab_size);
    for _ in 0..vocab_size {
        let line = lines.next()
            .ok_or("truncated vocabulary")?
            .map_err(|e| format!("read vocab: {e}"))?;
        vocabulary.push(line);
    }

    // Documents
    let mut docs = Vec::with_capacity(doc_count);
    for _ in 0..doc_count {
        let line = lines.next()
            .ok_or("truncated docs")?
            .map_err(|e| format!("read doc: {e}"))?;
        let parts: Vec<&str> = line.splitn(5, '\t').collect();
        if parts.len() < 5 { continue; }
        let tf_idf: Vec<f32> = parts[4].split(',')
            .filter_map(|s| s.parse().ok())
            .collect();
        docs.push(VectorDoc {
            qualified_name: parts[0].to_string(),
            name: parts[1].to_string(),
            label: parts[2].to_string(),
            file_path: parts[3].to_string(),
            tf_idf,
        });
    }

    Ok(VectorIndex { docs, vocabulary })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn extract_file_path(qualified_name: &str) -> String {
    if let Some(idx) = qualified_name.find("::") {
        qualified_name[..idx].to_string()
    } else {
        qualified_name.to_string()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cosine_identical() {
        let v = vec![1.0, 2.0, 3.0];
        let sim = cosine_similarity(&v, &v);
        assert!((sim - 1.0).abs() < 1e-5, "identical vectors: {sim}");
    }

    #[test]
    fn test_cosine_orthogonal() {
        let a = vec![1.0, 0.0];
        let b = vec![0.0, 1.0];
        let sim = cosine_similarity(&a, &b);
        assert!(sim.abs() < 1e-5, "orthogonal vectors: {sim}");
    }

    #[test]
    fn test_tokenize_to_terms() {
        let terms = tokenize_to_terms("handle_tool_call");
        assert_eq!(terms, vec!["handle", "tool", "call"]);
    }
}
