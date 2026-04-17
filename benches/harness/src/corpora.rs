// corpora.rs — load corpus configs + ground truth labels.
//
// Layout per spec:
//   benches/corpora/<name>/corpus.toml       — declares path/language/name
//   benches/corpora/<name>/ground_truth.json — hand labels (see §2.4)
//
// Schema errors are hard errors: a malformed label file fails the load
// rather than silently contributing zeros to the aggregate.  That's a
// zetetic-standard non-negotiable.

use serde::Deserialize;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

/// Parsed corpus.toml manifest.
#[derive(Debug, Clone, Deserialize)]
pub struct CorpusManifest {
    pub name: String,
    pub language: String,
    /// Path relative to the corpus directory OR absolute.
    pub path: String,
    /// Optional friendly description. Read from disk but not surfaced by the
    /// runner; kept on the struct so `toml::from_str` accepts the field.
    #[serde(default)]
    #[allow(dead_code)]
    pub description: String,
}

/// A single labeled ground-truth entry.  Matches the on-disk schema in
/// §"Ground truth format" of the task brief.
#[derive(Debug, Clone, Deserialize)]
pub struct GroundTruthLabel {
    pub query_id: String,
    pub input: Value,
    pub expected: Value,
}

/// Full ground_truth.json document.
#[derive(Debug, Clone, Deserialize)]
pub struct GroundTruthDoc {
    pub corpus: String,
    pub language: String,
    #[serde(default)]
    #[allow(dead_code)]
    pub version: String,
    pub labels: Vec<GroundTruthLabel>,
}

/// Resolved corpus ready to hand to the runner.
#[derive(Debug, Clone)]
pub struct CorpusConfig {
    pub name: String,
    pub language: String,
    /// Absolute path to the source tree to index.
    pub source_path: PathBuf,
    pub labels: Vec<GroundTruthLabel>,
    /// True iff labels is empty (a stub corpus).
    pub is_stub: bool,
}

/// Load one corpus by name.  Returns Err if the directory doesn't exist,
/// corpus.toml is missing/malformed, or ground_truth.json schema is broken.
/// An empty labels array is allowed (stub corpus) and produces is_stub=true.
pub fn load_one(corpora_root: &Path, name: &str) -> Result<CorpusConfig, String> {
    let dir = corpora_root.join(name);
    let manifest_path = dir.join("corpus.toml");
    let truth_path = dir.join("ground_truth.json");

    let manifest = read_manifest(&manifest_path)?;
    let truth = read_truth(&truth_path)?;

    if manifest.name != truth.corpus {
        return Err(format!(
            "corpus mismatch: manifest={} truth={}",
            manifest.name, truth.corpus
        ));
    }
    if manifest.language != truth.language {
        return Err(format!(
            "language mismatch in {}: manifest={} truth={}",
            name, manifest.language, truth.language
        ));
    }

    let source_path = resolve_source_path(&dir, &manifest.path)?;
    let is_stub = truth.labels.is_empty();
    Ok(CorpusConfig {
        name: manifest.name,
        language: manifest.language,
        source_path,
        labels: truth.labels,
        is_stub,
    })
}

/// Discover and load every corpus under corpora_root that has a non-empty
/// labels array.  Stubs (is_stub=true) are skipped but logged to stderr so
/// the operator knows they exist.
pub fn discover_all(corpora_root: &Path) -> Result<Vec<CorpusConfig>, String> {
    let mut out = Vec::new();
    let entries = fs::read_dir(corpora_root)
        .map_err(|e| format!("read corpora root {:?}: {e}", corpora_root))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("iterate corpora: {e}"))?;
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let dir = entry.path();
        if !dir.join("corpus.toml").exists() {
            continue;
        }
        match load_one(corpora_root, &name) {
            Ok(c) if c.is_stub => {
                eprintln!("[bench] skipping stub corpus: {} (no labels yet)", name);
            }
            Ok(c) => out.push(c),
            Err(e) => return Err(format!("corpus {name}: {e}")),
        }
    }
    Ok(out)
}

fn read_manifest(path: &Path) -> Result<CorpusManifest, String> {
    let raw = fs::read_to_string(path)
        .map_err(|e| format!("read {:?}: {e}", path))?;
    toml::from_str::<CorpusManifest>(&raw)
        .map_err(|e| format!("parse {:?}: {e}", path))
}

fn read_truth(path: &Path) -> Result<GroundTruthDoc, String> {
    let raw = fs::read_to_string(path)
        .map_err(|e| format!("read {:?}: {e}", path))?;
    serde_json::from_str::<GroundTruthDoc>(&raw)
        .map_err(|e| format!("parse {:?}: {e}", path))
}

fn resolve_source_path(corpus_dir: &Path, raw: &str) -> Result<PathBuf, String> {
    let candidate = PathBuf::from(raw);
    let absolute = if candidate.is_absolute() {
        candidate
    } else {
        corpus_dir.join(candidate)
    };
    let canonical = absolute
        .canonicalize()
        .map_err(|e| format!("canonicalize {:?}: {e}", absolute))?;
    if !canonical.exists() {
        return Err(format!("source path does not exist: {:?}", canonical));
    }
    Ok(canonical)
}
