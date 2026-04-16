// prd_input — Stage 4: bundle verified finding + graph intel into a single
// artifact for the PRD generator (TypeScript) to consume.
//
// Read-only with respect to the graph. Writes one JSON artifact under
// <output_dir>/runs/<run_id>/findings/<finding_id>/stage-4.prd_input.json
// and updates <output_dir>/runs/<run_id>/index.json with stage4 markers.
//
// Pipeline:
//   1. Load stage-2.verified.json   (must have verified:true)
//   2. Load stage-1.refined.json    (for title/description — the verified
//      receipt intentionally omits the finding body per stage-2.md §5.3)
//   3. Tokenize description → whitespace tokens, lowercased, alpha/_/:: only
//   4. For each token, search_graph top-3 → collect matched qualified_names
//   5. Dedup → for each matched symbol load 1-hop context (community, procs,
//      calls, called_by, uses)
//   6. Write artifact + update index
//
// source: stages/stage-2.md §5.3 (verified schema), stage-1.md §4.2
// (refined schema). The symbol-search-from-description pattern is the
// explicit spec for stage 4 (see the architect's brief embedded in
// docs/stage-4-spec when it lands).

use crate::graph_store::GraphStore;
use crate::search;
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

// source: stage-4 brief — "top-3 matches per token" caps the cost of
// description-driven search. Not paper-backed; chosen to keep output
// compact while capturing the obvious per-token hit plus two backups.
pub const MATCHES_PER_TOKEN: usize = 3;

// source: stage-4 brief — preparer schema version. "1.0.0" = first release.
pub const PREPARER_VERSION: &str = "1.0.0";

// source: stage-4 brief — artifact filename (mirrors stage-1/2 conventions).
pub const PRD_INPUT_FILE_NAME: &str = "stage-4.prd_input.json";

// Minimum token length — single-letter tokens ("a", "i") produce noise.
// source: Lucene StandardAnalyzer default behavior (min length 2+ for
// general-purpose text indexing). Stricter than BM25's bare minimum so
// the per-token search stays focused on symbol-like words.
const MIN_TOKEN_LEN: usize = 3;

// Cap token count so a pathologically-long description can't explode the
// search budget. 32 tokens × 3 matches = 96 candidate symbols, dedup caps
// the real work below that. Chosen to match the stage-4 brief guidance
// ("compact artifact") — no paper source.
const MAX_TOKENS: usize = 32;

// ---------------------------------------------------------------------------
// Public result types
// ---------------------------------------------------------------------------

/// Outcome of a successful prepare_prd_input run. Drives the MCP receipt.
pub struct PrdInputOutcome {
    pub artifact_path: PathBuf,
    pub matched_symbol_count: usize,
    pub impacted_community_count: usize,
    pub impacted_process_count: usize,
}

/// Arguments already validated by the handler in main.rs.
pub struct PrdInputArgs {
    pub run_id: String,
    pub finding_id: String,
    pub output_dir: PathBuf,
    pub graph_path: PathBuf,
}

// ---------------------------------------------------------------------------
// Orchestration
// ---------------------------------------------------------------------------

/// Runs stage 4 end to end.
pub fn prepare(args: &PrdInputArgs, prepared_at: String) -> Result<PrdInputOutcome, String> {
    let finding_dir = finding_dir_for(args);
    let verified = load_verified(&finding_dir)?;
    let summary = load_finding_summary(&finding_dir)?;
    let store = GraphStore::open_or_create(&args.graph_path)?;
    let stats = collect_graph_stats(&store);
    let tokens = tokenize_description(&summary);
    let matched = search_and_enrich(&store, &tokens);
    let impacted_communities = dedup_keys(&matched, |s| s.community_id.clone());
    let impacted_processes = impacted_processes_from_symbols(&matched);
    let artifact = build_artifact(
        args, &verified, &summary, &prepared_at,
        &matched, &impacted_communities, &impacted_processes, &stats,
    );
    let artifact_path = finding_dir.join(PRD_INPUT_FILE_NAME);
    write_json(&artifact_path, &artifact)?;
    update_index(args, &prepared_at)?;
    Ok(PrdInputOutcome {
        artifact_path,
        matched_symbol_count: matched.len(),
        impacted_community_count: impacted_communities.len(),
        impacted_process_count: impacted_processes.len(),
    })
}

fn finding_dir_for(args: &PrdInputArgs) -> PathBuf {
    args.output_dir
        .join("runs")
        .join(&args.run_id)
        .join("findings")
        .join(&args.finding_id)
}

// ---------------------------------------------------------------------------
// Stage-2 verified loader — enforces `verified: true` gate
// ---------------------------------------------------------------------------

struct VerifiedReceipt {
    finalized_at: String,
    stage1_refined_path: String,
    verified: bool,
}

fn load_verified(finding_dir: &Path) -> Result<VerifiedReceipt, String> {
    let path = finding_dir.join("stage-2.verified.json");
    if !path.exists() {
        return Err("stage_2_not_verified: stage-2.verified.json missing".into());
    }
    let raw = fs::read_to_string(&path)
        .map_err(|e| format!("stage_2_not_verified: read {:?}: {}", path, e))?;
    let v: Value = serde_json::from_str(&raw)
        .map_err(|e| format!("stage_2_not_verified: parse {:?}: {}", path, e))?;
    let verified = v.get("verified").and_then(|x| x.as_bool()).unwrap_or(false);
    if !verified {
        return Err("stage_2_not_verified: verified flag is false".into());
    }
    Ok(VerifiedReceipt {
        finalized_at: v
            .get("finalized_at")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        stage1_refined_path: v
            .get("stage1_refined_path")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        verified,
    })
}

// ---------------------------------------------------------------------------
// Stage-1 refined loader — pulls title/description for summary+tokenization
// ---------------------------------------------------------------------------

struct FindingSummary {
    finding_id: String,
    title: String,
    description: String,
    relevance_category: String,
}

fn load_finding_summary(finding_dir: &Path) -> Result<FindingSummary, String> {
    let path = finding_dir.join("stage-1.refined.json");
    if !path.exists() {
        return Err(format!(
            "stage_1_refined_missing: {} not found",
            path.display()
        ));
    }
    let raw = fs::read_to_string(&path)
        .map_err(|e| format!("stage_1_refined_unreadable: {}", e))?;
    let v: Value = serde_json::from_str(&raw)
        .map_err(|e| format!("stage_1_refined_corrupt: {}", e))?;
    let extracted = v.get("extracted").cloned().unwrap_or(Value::Null);
    let finding_id = str_field(&extracted, "finding_id");
    let title = str_field(&extracted, "title");
    let description = str_field(&extracted, "description");
    let relevance_category = str_field(&extracted, "relevance_category");
    Ok(FindingSummary {
        finding_id,
        title,
        description,
        relevance_category,
    })
}

fn str_field(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string()
}

// ---------------------------------------------------------------------------
// Tokenization — whitespace split + lowercase + basic sanitization
// ---------------------------------------------------------------------------

fn tokenize_description(summary: &FindingSummary) -> Vec<String> {
    let combined = format!("{} {}", summary.title, summary.description);
    let mut seen: Vec<String> = Vec::new();
    for raw in combined.split_whitespace() {
        if seen.len() >= MAX_TOKENS {
            break;
        }
        let cleaned = clean_token(raw);
        if cleaned.len() < MIN_TOKEN_LEN {
            continue;
        }
        if !seen.iter().any(|t| t == &cleaned) {
            seen.push(cleaned);
        }
    }
    seen
}

fn clean_token(raw: &str) -> String {
    raw.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == ':')
        .collect::<String>()
        .to_ascii_lowercase()
}

// ---------------------------------------------------------------------------
// Per-token search + enrichment
// ---------------------------------------------------------------------------

struct MatchedSymbol {
    qualified_name: String,
    name: String,
    label: String,
    file_path: String,
    community_id: Option<String>,
    community_size: Option<u64>,
    processes: Vec<String>,
    calls: Vec<String>,
    called_by: Vec<String>,
    uses: Vec<String>,
}

fn search_and_enrich(store: &GraphStore, tokens: &[String]) -> Vec<MatchedSymbol> {
    let mut by_qn: Vec<MatchedSymbol> = Vec::new();
    for token in tokens {
        let opts = search::SearchOptions {
            limit: MATCHES_PER_TOKEN,
            label_filter: None,
            min_score: 0.0,
        };
        let hits = match search::search_graph(store, token, &opts) {
            Ok(h) => h,
            Err(_) => continue,
        };
        for hit in hits {
            if by_qn.iter().any(|m| m.qualified_name == hit.qualified_name) {
                continue;
            }
            by_qn.push(enrich(store, hit));
        }
    }
    by_qn
}

fn enrich(store: &GraphStore, hit: search::SearchResult) -> MatchedSymbol {
    let community_size = hit
        .community_id
        .as_ref()
        .and_then(|cid| lookup_community_size(store, cid));
    // 1-hop neighbors via get_context — reuses the SAME three-layer lookup
    // that get_context uses, so PRD consumers see exactly what the search
    // tools expose. Graceful degradation if the symbol is fuzzy-matched
    // only.
    let (calls, called_by, uses) = match search::get_context(store, &hit.qualified_name) {
        Ok(ctx) => (
            ctx.calls.iter().map(|r| r.name.clone()).collect(),
            ctx.called_by.iter().map(|r| r.name.clone()).collect(),
            ctx.uses.iter().map(|r| r.name.clone()).collect(),
        ),
        Err(_) => (Vec::new(), Vec::new(), Vec::new()),
    };
    MatchedSymbol {
        qualified_name: hit.qualified_name,
        name: hit.name,
        label: hit.label,
        file_path: hit.file_path,
        community_id: hit.community_id,
        community_size,
        processes: hit.process_names,
        calls,
        called_by,
        uses,
    }
}

fn lookup_community_size(store: &GraphStore, community_id: &str) -> Option<u64> {
    let escaped = community_id.replace('\'', "\\'");
    let cypher = format!(
        "MATCH (c:Community) WHERE c.id = '{escaped}' RETURN c.member_count LIMIT 1"
    );
    let qr = store.execute_query(&cypher).ok()?;
    let row = qr.rows.first()?;
    row.first().and_then(|s| s.parse::<u64>().ok())
}

// ---------------------------------------------------------------------------
// Impact aggregation helpers
// ---------------------------------------------------------------------------

fn dedup_keys<F>(matched: &[MatchedSymbol], key: F) -> Vec<String>
where
    F: Fn(&MatchedSymbol) -> Option<String>,
{
    let mut out: Vec<String> = Vec::new();
    for m in matched {
        if let Some(k) = key(m) {
            if !k.is_empty() && !out.contains(&k) {
                out.push(k);
            }
        }
    }
    out
}

fn impacted_processes_from_symbols(matched: &[MatchedSymbol]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for m in matched {
        for p in &m.processes {
            if !out.contains(p) {
                out.push(p.clone());
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Graph stats — cheap sanity counts for the PRD generator
// ---------------------------------------------------------------------------

struct GraphStats {
    nodes: u64,
    edges: u64,
    communities: u64,
    processes: u64,
}

fn collect_graph_stats(store: &GraphStore) -> GraphStats {
    let nodes = store.node_count().unwrap_or(0);
    let edges = store.edge_count().unwrap_or(0);
    let communities = count_label(store, "Community");
    let processes = count_label(store, "Process");
    GraphStats {
        nodes,
        edges,
        communities,
        processes,
    }
}

fn count_label(store: &GraphStore, label: &str) -> u64 {
    let cypher = format!("MATCH (n:{label}) RETURN count(n)");
    match store.execute_query(&cypher) {
        Ok(qr) => qr
            .rows
            .first()
            .and_then(|r| r.first())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0),
        Err(_) => 0,
    }
}

// ---------------------------------------------------------------------------
// Artifact builder — pure; no I/O
// ---------------------------------------------------------------------------

fn build_artifact(
    args: &PrdInputArgs,
    verified: &VerifiedReceipt,
    summary: &FindingSummary,
    prepared_at: &str,
    matched: &[MatchedSymbol],
    impacted_communities: &[String],
    impacted_processes: &[String],
    stats: &GraphStats,
) -> Value {
    let matched_symbols: Vec<Value> = matched.iter().map(matched_to_json).collect();

    let summary_text = if summary.description.is_empty() {
        summary.title.clone()
    } else {
        format!("{} — {}", summary.title, summary.description)
    };

    let stage2_rel = format!(
        "findings/{}/stage-2.verified.json",
        args.finding_id
    );

    json!({
        "run_id": args.run_id,
        "finding_id": args.finding_id,
        "stage2_verified_path": stage2_rel,
        "graph_path": args.graph_path.to_string_lossy(),
        "prepared_at": prepared_at,
        "prd_context": {
            "finding_summary": summary_text,
            "finding_title": summary.title,
            "relevance_category": summary.relevance_category,
            "finalized_at": verified.finalized_at,
            "stage1_refined_path": verified.stage1_refined_path,
            "verified": verified.verified,
            "finding_id": summary.finding_id,
            "matched_symbols": matched_symbols,
            "impacted_communities": impacted_communities,
            "impacted_processes": impacted_processes,
            "graph_stats": {
                "nodes": stats.nodes,
                "edges": stats.edges,
                "communities": stats.communities,
                "processes": stats.processes,
            }
        },
        "preparer_version": PREPARER_VERSION,
    })
}

fn matched_to_json(m: &MatchedSymbol) -> Value {
    json!({
        "qualified_name": m.qualified_name,
        "name": m.name,
        "label": m.label,
        "file_path": m.file_path,
        "community_id": m.community_id,
        "community_size": m.community_size,
        "processes": m.processes,
        "relationships": {
            "calls": m.calls,
            "called_by": m.called_by,
            "uses": m.uses,
        }
    })
}

// ---------------------------------------------------------------------------
// Filesystem writes
// ---------------------------------------------------------------------------

fn write_json(path: &Path, value: &Value) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("mkdir {:?}: {}", parent, e))?;
    }
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|e| format!("serialize: {}", e))?;
    fs::write(path, bytes).map_err(|e| format!("write {:?}: {}", path, e))?;
    Ok(())
}

/// Updates `<output_dir>/runs/<run_id>/index.json` with stage-4 markers.
/// Preserves all existing fields — stage-4 only appends two top-level keys.
fn update_index(args: &PrdInputArgs, prepared_at: &str) -> Result<(), String> {
    let index_path = args
        .output_dir
        .join("runs")
        .join(&args.run_id)
        .join("index.json");
    if !index_path.exists() {
        return Ok(());
    }
    let raw = fs::read_to_string(&index_path)
        .map_err(|e| format!("read index: {}", e))?;
    let mut v: Value = serde_json::from_str(&raw)
        .map_err(|e| format!("parse index: {}", e))?;
    if let Some(obj) = v.as_object_mut() {
        obj.insert(
            "stage4_prepared_at".into(),
            Value::String(prepared_at.to_string()),
        );
        let rel = format!(
            "findings/{}/{}",
            args.finding_id, PRD_INPUT_FILE_NAME
        );
        obj.insert("stage4_path".into(), Value::String(rel));
    }
    let bytes = serde_json::to_vec_pretty(&v)
        .map_err(|e| format!("serialize index: {}", e))?;
    fs::write(&index_path, bytes).map_err(|e| format!("write index: {}", e))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests — pure helpers that don't need a graph
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tokenize_description_basic() {
        let s = FindingSummary {
            finding_id: "f1".into(),
            title: "Handle tool call should reject unknown tools".into(),
            description: "The handle_tool_call function in main.rs silently returns".into(),
            relevance_category: "bug".into(),
        };
        let t = tokenize_description(&s);
        assert!(t.iter().any(|x| x == "handle"));
        assert!(t.iter().any(|x| x == "tool"));
        assert!(t.iter().any(|x| x == "handle_tool_call"));
        // Short tokens below MIN_TOKEN_LEN must be filtered.
        assert!(!t.iter().any(|x| x.len() < MIN_TOKEN_LEN));
    }

    #[test]
    fn test_tokenize_respects_max_tokens() {
        let description = (0..100)
            .map(|i| format!("word{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        let s = FindingSummary {
            finding_id: "f1".into(),
            title: "title".into(),
            description,
            relevance_category: "bug".into(),
        };
        let t = tokenize_description(&s);
        assert!(t.len() <= MAX_TOKENS);
    }

    #[test]
    fn test_clean_token_strips_punctuation() {
        assert_eq!(clean_token("Foo,"), "foo");
        assert_eq!(clean_token("bar.baz"), "barbaz");
        assert_eq!(clean_token("x::y"), "x::y");
        assert_eq!(clean_token("(parens)"), "parens");
    }

    #[test]
    fn test_load_verified_rejects_false_flag() {
        let tmp = std::env::temp_dir()
            .join(format!("prd_input_false_{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        let body = json!({
            "verified": false,
            "finalized_at": "2026-04-11T00:00:00Z",
            "stage1_refined_path": "findings/f/stage-1.refined.json",
        });
        fs::write(
            tmp.join("stage-2.verified.json"),
            serde_json::to_vec_pretty(&body).unwrap(),
        )
        .unwrap();
        let err = load_verified(&tmp).err().unwrap();
        assert!(err.contains("stage_2_not_verified"), "got: {err}");
        let _ = fs::remove_dir_all(&tmp);
    }
}
