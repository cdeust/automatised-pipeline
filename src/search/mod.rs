// search — Stage 3d: hybrid BM25 + TF-IDF vector search with RRF fusion.
//
// Upgraded from substring-overlap heuristic to proper hybrid search:
//   1. BM25 full-text search via Tantivy (0.26, MIT, quickwit-oss)
//   2. Semantic vector search via TF-IDF (tokenized symbol names)
//   3. Reciprocal Rank Fusion (Cormack, Clarke, Büttcher 2009, k=60)
//
// The search index is built after clustering in the analyze_codebase flow:
//   parse → resolve → cluster → build_search_index
//
// Public API is unchanged: search_graph(store, query, options) → Vec<SearchResult>

pub mod bm25;
pub mod rrf;
pub mod vector;

use crate::graph_store::GraphStore;
use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

pub struct SearchResult {
    pub qualified_name: String,
    pub name: String,
    pub label: String,
    pub file_path: String,
    pub score: f64,
    pub community_id: Option<String>,
    pub process_names: Vec<String>,
    pub start_line: Option<u64>,
    pub end_line: Option<u64>,
}

pub struct SearchOptions {
    pub limit: usize,
    pub label_filter: Option<String>,
    pub min_score: f64,
}

impl Default for SearchOptions {
    fn default() -> Self {
        SearchOptions {
            limit: 20,
            label_filter: None,
            min_score: 0.0,
        }
    }
}

// ---------------------------------------------------------------------------
// Context types — 360° symbol view
// ---------------------------------------------------------------------------

pub struct SymbolContext {
    pub qualified_name: String,
    pub name: String,
    pub label: String,
    pub file_path: String,
    pub start_line: Option<u64>,
    pub end_line: Option<u64>,
    pub visibility: Option<String>,
    pub imports: Vec<RelatedSymbol>,
    pub imported_by: Vec<RelatedSymbol>,
    pub calls: Vec<RelatedSymbol>,
    pub called_by: Vec<RelatedSymbol>,
    pub implements: Vec<RelatedSymbol>,
    pub implemented_by: Vec<RelatedSymbol>,
    pub uses: Vec<RelatedSymbol>,
    pub used_by: Vec<RelatedSymbol>,
    pub community: Option<CommunityInfo>,
    pub processes: Vec<ProcessRef>,
}

pub struct RelatedSymbol {
    pub qualified_name: String,
    pub name: String,
    pub label: String,
}

pub struct CommunityInfo {
    pub id: String,
    pub name: String,
    pub member_count: u64,
}

pub struct ProcessRef {
    pub name: String,
    pub role: String,
}

// ---------------------------------------------------------------------------
// Build search index — called after clustering
// ---------------------------------------------------------------------------

/// Result of building search indexes.
pub struct SearchIndexResult {
    pub bm25_doc_count: usize,
    pub vector_doc_count: usize,
    pub elapsed_ms: u64,
}

/// Builds both BM25 (Tantivy) and vector (TF-IDF) indexes.
/// Call after the graph is fully built (post-clustering).
/// Index is stored at `<output_dir>/search_index/`.
pub fn build_search_index(
    store: &GraphStore,
    output_dir: &Path,
) -> Result<SearchIndexResult, String> {
    let start = Instant::now();
    let index_dir = output_dir.join("search_index");

    let bm25_dir = index_dir.join("bm25");
    let bm25_count = bm25::build_index(store, &bm25_dir)?;

    let vector_count = vector::build_index(store, &index_dir)?;

    Ok(SearchIndexResult {
        bm25_doc_count: bm25_count,
        vector_doc_count: vector_count,
        elapsed_ms: start.elapsed().as_millis() as u64,
    })
}

// ---------------------------------------------------------------------------
// search_graph — hybrid ranked search with RRF fusion
// ---------------------------------------------------------------------------

const SEARCHABLE_LABELS: &[&str] = &[
    "Function", "Method", "Struct", "Enum", "Trait",
    "Module", "Constant", "TypeAlias",
];

/// Searches the graph using hybrid BM25 + vector search with RRF fusion.
///
/// If search indexes exist (built by `build_search_index`), uses the hybrid
/// approach. Falls back to graph-only substring search if no indexes found.
pub fn search_graph(
    store: &GraphStore,
    query: &str,
    options: &SearchOptions,
) -> Result<Vec<SearchResult>, String> {
    let _start = Instant::now();
    let query_lower = query.to_lowercase();
    let terms: Vec<&str> = query_lower.split_whitespace().collect();
    if terms.is_empty() {
        return Ok(Vec::new());
    }

    // Try to find search index directory relative to the graph path.
    // Convention: search_index/ is a sibling of graph/ under the output_dir.
    let index_dir = find_search_index_dir(store);

    let has_bm25 = index_dir.as_ref()
        .map(|d| d.join("bm25").exists())
        .unwrap_or(false);
    let has_vector = index_dir.as_ref()
        .map(|d| d.join("vector_index.bin").exists())
        .unwrap_or(false);

    if has_bm25 || has_vector {
        search_hybrid(store, query, options, index_dir.as_deref(), has_bm25, has_vector)
    } else {
        // Fallback: graph-only substring search (v1 behavior)
        search_substring(store, &terms, options)
    }
}

fn search_hybrid(
    store: &GraphStore,
    query: &str,
    options: &SearchOptions,
    index_dir: Option<&Path>,
    has_bm25: bool,
    has_vector: bool,
) -> Result<Vec<SearchResult>, String> {
    let fetch_limit = options.limit * 3; // overfetch for RRF fusion

    // Run BM25 search
    let bm25_ranked: Vec<rrf::RankedEntry> = if has_bm25 {
        let bm25_dir = index_dir.unwrap().join("bm25");
        let results = bm25::query_index(&bm25_dir, query, fetch_limit)?;
        results.iter().enumerate().map(|(i, r)| {
            rrf::RankedEntry { key: r.qualified_name.clone(), rank: i + 1 }
        }).collect()
    } else {
        Vec::new()
    };

    // Run vector search
    let vector_ranked: Vec<rrf::RankedEntry> = if has_vector {
        let results = vector::query_index(index_dir.unwrap(), query, fetch_limit)?;
        results.iter().enumerate().map(|(i, r)| {
            rrf::RankedEntry { key: r.qualified_name.clone(), rank: i + 1 }
        }).collect()
    } else {
        Vec::new()
    };

    // Fuse with RRF (Cormack et al. 2009, k=60)
    let mut ranking_lists: Vec<&[rrf::RankedEntry]> = Vec::new();
    if !bm25_ranked.is_empty() { ranking_lists.push(&bm25_ranked); }
    if !vector_ranked.is_empty() { ranking_lists.push(&vector_ranked); }

    if ranking_lists.is_empty() {
        return Ok(Vec::new());
    }

    let fused = rrf::fuse(&ranking_lists, options.limit * 2);

    // Enrich fused results with graph metadata
    let community_sizes = load_community_sizes(store);
    let process_counts = load_process_counts(store);

    let mut results: Vec<SearchResult> = Vec::new();
    for rrf_result in &fused {
        if let Some(enriched) = enrich_from_graph(
            store, &rrf_result.key, rrf_result.score,
            &community_sizes, &process_counts, options,
        ) {
            results.push(enriched);
        }
    }

    results.truncate(options.limit);
    Ok(results)
}

fn enrich_from_graph(
    store: &GraphStore,
    qualified_name: &str,
    rrf_score: f64,
    community_sizes: &HashMap<String, u64>,
    process_counts: &HashMap<String, usize>,
    options: &SearchOptions,
) -> Option<SearchResult> {
    let escaped = qualified_name.replace('\'', "\\'");

    for &label in SEARCHABLE_LABELS {
        if let Some(ref filter) = options.label_filter {
            if !filter.eq_ignore_ascii_case(label) {
                continue;
            }
        }

        let has_lines = matches!(label, "Function" | "Method" | "Struct" | "Enum" | "Trait");
        let return_clause = if has_lines {
            "n.qualified_name, n.name, n.id, n.start_line, n.end_line"
        } else {
            "n.qualified_name, n.name, n.id"
        };
        let cypher = format!(
            "MATCH (n:{label}) WHERE n.qualified_name = '{escaped}' RETURN {return_clause}"
        );
        if let Ok(qr) = store.execute_query(&cypher) {
            if let Some(row) = qr.rows.first() {
                if row.len() < 3 { continue; }
                let qn = &row[0];
                let name = &row[1];
                let id = &row[2];
                let file_path = extract_file_path(qn);

                let community_id = lookup_community(store, label, id);
                let process_names = lookup_processes(store, label, id);

                let (sl, el) = if has_lines && row.len() >= 5 {
                    (parse_opt_u64(&row[3]), parse_opt_u64(&row[4]))
                } else {
                    (None, None)
                };

                // Add community/process boost to RRF score
                let community_boost = match &community_id {
                    Some(cid) => {
                        let size = community_sizes.get(cid).copied().unwrap_or(100);
                        if size < 20 { 0.002 } else { 0.0 }
                    }
                    None => 0.0,
                };
                let proc_count = process_counts.get(qn).copied().unwrap_or(0);
                let process_boost = 0.001 * (proc_count.min(3) as f64);
                let final_score = rrf_score + community_boost + process_boost;

                if final_score < options.min_score {
                    return None;
                }

                return Some(SearchResult {
                    qualified_name: qn.clone(),
                    name: name.clone(),
                    label: label.to_string(),
                    file_path,
                    score: final_score,
                    community_id,
                    process_names,
                    start_line: sl,
                    end_line: el,
                });
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Fallback: substring search (v1 behavior)
// ---------------------------------------------------------------------------

fn search_substring(
    store: &GraphStore,
    terms: &[&str],
    options: &SearchOptions,
) -> Result<Vec<SearchResult>, String> {
    let community_sizes = load_community_sizes(store);
    let process_counts = load_process_counts(store);

    let mut results: Vec<SearchResult> = Vec::new();

    for &label in SEARCHABLE_LABELS {
        if let Some(ref filter) = options.label_filter {
            if !filter.eq_ignore_ascii_case(label) {
                continue;
            }
        }
        let candidates = fetch_candidates(store, label)?;
        for c in candidates {
            let score = score_candidate(&c, terms, &community_sizes, &process_counts);
            if score < options.min_score {
                continue;
            }
            results.push(SearchResult {
                qualified_name: c.qualified_name,
                name: c.name,
                label: label.to_string(),
                file_path: c.file_path,
                score,
                community_id: c.community_id,
                process_names: c.process_names,
                start_line: c.start_line,
                end_line: c.end_line,
            });
        }
    }

    results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    results.truncate(options.limit);
    Ok(results)
}

// ---------------------------------------------------------------------------
// get_context — 360° symbol view
// ---------------------------------------------------------------------------

/// Error returned by `get_context` when a symbol cannot be resolved.
/// Carries "did you mean" suggestions so the MCP caller can surface them
/// verbatim instead of choking on a bare "symbol not found" string.
///
/// source: C-correctness bug 2 — callers naturally pass `src/main.rs::X`
/// while the graph stores `main.rs::X`; the old API returned a flat Err
/// that hid the near-misses.
#[derive(Debug, Clone)]
pub struct SymbolNotFound {
    pub input: String,
    pub did_you_mean: Vec<String>,
}

#[derive(Debug, Clone)]
pub enum GetContextError {
    NotFound(SymbolNotFound),
    Other(String),
}

impl From<String> for GetContextError {
    fn from(s: String) -> Self { GetContextError::Other(s) }
}

pub fn get_context(
    store: &GraphStore,
    qualified_name: &str,
) -> Result<SymbolContext, GetContextError> {
    // Layer 1: exact match (qualified_name OR id).
    // Layer 2: strip first path component and retry (src/foo::X → foo::X).
    // Layer 3: name-only fuzzy match — return top candidates.
    let resolved = resolve_qualified_name(store, qualified_name)
        .map_err(GetContextError::NotFound)?;

    let escaped = resolved.replace('\'', "\\'");
    let (label, name, file_path, start_line, end_line, visibility) =
        find_node_details(store, &escaped)?;

    let imports = find_related_out(store, &escaped, "Imports_");
    let imported_by = find_related_in(store, &escaped, "Imports_");
    let calls = find_related_out(store, &escaped, "Calls_");
    let called_by = find_related_in(store, &escaped, "Calls_");
    let implements = find_related_out(store, &escaped, "Implements_");
    let implemented_by = find_related_in(store, &escaped, "Implements_");
    let uses = find_related_out(store, &escaped, "Uses_");
    let used_by = find_related_in(store, &escaped, "Uses_");
    let community = find_community(store, &escaped);
    let processes = find_processes(store, &escaped);

    Ok(SymbolContext {
        qualified_name: resolved,
        name,
        label,
        file_path,
        start_line,
        end_line,
        visibility,
        imports,
        imported_by,
        calls,
        called_by,
        implements,
        implemented_by,
        uses,
        used_by,
        community,
        processes,
    })
}

/// Three-layer qualified-name lookup.
/// Returns the resolved (stored) qualified_name on success, or a
/// `SymbolNotFound` carrying suggestions when every layer misses.
///
/// Used by both `get_context` and `get_symbol` so both tools share the
/// same forgiving input surface.
pub fn resolve_qualified_name(
    store: &GraphStore,
    input: &str,
) -> Result<String, SymbolNotFound> {
    // Layer 1 — exact.
    if let Some(qn) = exact_match_qn(store, input) {
        return Ok(qn);
    }

    // Layer 2 — strip first path component if the input has one.
    // Parser strips `src/` when building qualified_names, so callers who
    // naturally pass `src/main.rs::foo` must find `main.rs::foo`.
    if let Some(stripped) = strip_leading_path_component(input) {
        if let Some(qn) = exact_match_qn(store, &stripped) {
            return Ok(qn);
        }
    }

    // Layer 3 — name-only fuzzy. Return top candidates as suggestions.
    let leaf = input.rsplit("::").next().unwrap_or(input);
    let suggestions = find_name_candidates(store, leaf, 5);
    Err(SymbolNotFound {
        input: input.to_string(),
        did_you_mean: suggestions,
    })
}

fn strip_leading_path_component(input: &str) -> Option<String> {
    // Only act if the path portion (before `::`) has a `/`.
    let (path_part, rest) = match input.find("::") {
        Some(i) => (&input[..i], &input[i..]),
        None => (input, ""),
    };
    let idx = path_part.find('/')?;
    Some(format!("{}{}", &path_part[idx + 1..], rest))
}

fn exact_match_qn(store: &GraphStore, input: &str) -> Option<String> {
    let escaped = input.replace('\'', "\\'");
    let labels = [
        "Function", "Method", "Struct", "Enum", "Trait",
        "Module", "Constant", "TypeAlias",
    ];
    for label in labels {
        let cypher = format!(
            "MATCH (n:{label}) WHERE n.qualified_name = '{escaped}' OR n.id = '{escaped}' \
             RETURN n.qualified_name LIMIT 1"
        );
        if let Ok(qr) = store.execute_query(&cypher) {
            if let Some(row) = qr.rows.first() {
                if !row.is_empty() {
                    return Some(row[0].clone());
                }
            }
        }
    }
    None
}

fn find_name_candidates(store: &GraphStore, name: &str, limit: usize) -> Vec<String> {
    let escaped = name.replace('\'', "\\'");
    let labels = [
        "Function", "Method", "Struct", "Enum", "Trait",
        "Module", "Constant", "TypeAlias",
    ];
    let mut out = Vec::new();
    for label in labels {
        if out.len() >= limit { break; }
        let cypher = format!(
            "MATCH (n:{label}) WHERE n.name = '{escaped}' \
             RETURN n.qualified_name LIMIT {limit}"
        );
        if let Ok(qr) = store.execute_query(&cypher) {
            for row in &qr.rows {
                if out.len() >= limit { break; }
                if !row.is_empty() && !out.contains(&row[0]) {
                    out.push(row[0].clone());
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Internal: find search index directory
// ---------------------------------------------------------------------------

fn find_search_index_dir(_store: &GraphStore) -> Option<std::path::PathBuf> {
    // The GraphStore doesn't expose its path, but we can probe known locations.
    // Convention: search_index/ is a sibling of graph/ under the output dir.
    // The caller passes graph_path when opening the store. We use a probe:
    // check if ../search_index/ exists relative to the DB path.
    //
    // Since GraphStore doesn't expose its path, we use an env-var hint
    // set by the search tool handler, or probe common locations.
    if let Ok(hint) = std::env::var("AA_SEARCH_INDEX_DIR") {
        let p = std::path::PathBuf::from(hint);
        if p.exists() { return Some(p); }
    }
    None
}

// ---------------------------------------------------------------------------
// Internal: candidate fetching (for substring fallback)
// ---------------------------------------------------------------------------

struct Candidate {
    qualified_name: String,
    name: String,
    file_path: String,
    community_id: Option<String>,
    process_names: Vec<String>,
    start_line: Option<u64>,
    end_line: Option<u64>,
}

fn fetch_candidates(store: &GraphStore, label: &str) -> Result<Vec<Candidate>, String> {
    let has_lines = matches!(label, "Function" | "Method" | "Struct" | "Enum" | "Trait");
    let return_clause = if has_lines {
        "n.qualified_name, n.name, n.id, n.start_line, n.end_line"
    } else {
        "n.qualified_name, n.name, n.id"
    };
    let cypher = format!("MATCH (n:{label}) RETURN {return_clause}");
    let qr = store.execute_query(&cypher)?;

    let mut candidates = Vec::new();
    for row in &qr.rows {
        if row.len() < 3 { continue; }
        let qn = &row[0];
        let name = &row[1];
        let id = &row[2];

        let file_path = extract_file_path(qn);
        let community_id = lookup_community(store, label, id);
        let process_names = lookup_processes(store, label, id);

        let (sl, el) = if has_lines && row.len() >= 5 {
            (parse_opt_u64(&row[3]), parse_opt_u64(&row[4]))
        } else {
            (None, None)
        };

        candidates.push(Candidate {
            qualified_name: qn.clone(),
            name: name.clone(),
            file_path,
            community_id,
            process_names,
            start_line: sl,
            end_line: el,
        });
    }
    Ok(candidates)
}

fn parse_opt_u64(s: &str) -> Option<u64> {
    s.parse::<u64>().ok()
}

fn extract_file_path(qualified_name: &str) -> String {
    if let Some(idx) = qualified_name.find("::") {
        qualified_name[..idx].to_string()
    } else {
        qualified_name.to_string()
    }
}

// ---------------------------------------------------------------------------
// Internal: scoring (substring fallback)
// ---------------------------------------------------------------------------

fn score_candidate(
    c: &Candidate,
    terms: &[&str],
    community_sizes: &HashMap<String, u64>,
    process_counts: &HashMap<String, usize>,
) -> f64 {
    let name_lower = c.name.to_lowercase();
    let qn_lower = c.qualified_name.to_lowercase();

    let mut best_term_score: f64 = 0.0;
    for &term in terms {
        let ts = term_score(term, &name_lower, &qn_lower);
        if ts > best_term_score { best_term_score = ts; }
    }

    if best_term_score == 0.0 {
        return 0.0;
    }

    let all_match = terms.iter().all(|t| qn_lower.contains(t) || name_lower.contains(t));
    let multi_bonus = if all_match && terms.len() > 1 { 0.1 } else { 0.0 };

    let community_boost = match &c.community_id {
        Some(cid) => {
            let size = community_sizes.get(cid).copied().unwrap_or(100);
            if size < 20 { 0.1 } else { 0.0 }
        }
        None => 0.0,
    };

    let proc_count = process_counts.get(&c.qualified_name).copied().unwrap_or(0);
    let process_boost = 0.05 * (proc_count.min(3) as f64);

    (best_term_score + multi_bonus + community_boost + process_boost).min(1.0)
}

fn term_score(term: &str, name_lower: &str, qn_lower: &str) -> f64 {
    if name_lower == term {
        return 1.0;
    }
    if !name_lower.is_empty() && name_lower.contains(term) {
        let ratio = term.len() as f64 / name_lower.len() as f64;
        return 0.7 + 0.3 * ratio;
    }
    if !qn_lower.is_empty() && qn_lower.contains(term) {
        let ratio = term.len() as f64 / qn_lower.len() as f64;
        return 0.5 * (1.0 + ratio);
    }
    0.0
}

// ---------------------------------------------------------------------------
// Internal: community + process lookups
// ---------------------------------------------------------------------------

fn load_community_sizes(store: &GraphStore) -> HashMap<String, u64> {
    let mut sizes = HashMap::new();
    let cypher = "MATCH (c:Community) RETURN c.id, c.member_count";
    if let Ok(qr) = store.execute_query(cypher) {
        for row in &qr.rows {
            if row.len() >= 2 {
                if let Ok(count) = row[1].parse::<u64>() {
                    sizes.insert(row[0].clone(), count);
                }
            }
        }
    }
    sizes
}

fn load_process_counts(store: &GraphStore) -> HashMap<String, usize> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    let labels = ["Function", "Method"];
    for label in labels {
        let cypher = format!(
            "MATCH (n:{label})-[:ParticipatesIn_{label}_Process]->(p:Process) \
             RETURN n.qualified_name, count(p)"
        );
        if let Ok(qr) = store.execute_query(&cypher) {
            for row in &qr.rows {
                if row.len() >= 2 {
                    if let Ok(c) = row[1].parse::<usize>() {
                        counts.insert(row[0].clone(), c);
                    }
                }
            }
        }
    }
    counts
}

fn lookup_community(store: &GraphStore, label: &str, node_id: &str) -> Option<String> {
    let rel = format!("MemberOf_{label}_Community");
    let cypher = format!(
        "MATCH (n:{label})-[:{rel}]->(c:Community) \
         WHERE n.id = '{node_id}' RETURN c.id LIMIT 1"
    );
    if let Ok(qr) = store.execute_query(&cypher) {
        if !qr.rows.is_empty() && !qr.rows[0].is_empty() {
            return Some(qr.rows[0][0].clone());
        }
    }
    None
}

fn lookup_processes(store: &GraphStore, label: &str, node_id: &str) -> Vec<String> {
    let mut names = Vec::new();
    if !matches!(label, "Function" | "Method") {
        return names;
    }
    let rel = format!("ParticipatesIn_{label}_Process");
    let cypher = format!(
        "MATCH (n:{label})-[:{rel}]->(p:Process) \
         WHERE n.id = '{node_id}' RETURN p.name"
    );
    if let Ok(qr) = store.execute_query(&cypher) {
        for row in &qr.rows {
            if !row.is_empty() {
                names.push(row[0].clone());
            }
        }
    }
    names
}

// ---------------------------------------------------------------------------
// Internal: context helpers (unchanged from v1)
// ---------------------------------------------------------------------------

fn find_node_details(
    store: &GraphStore,
    escaped: &str,
) -> Result<(String, String, String, Option<u64>, Option<u64>, Option<String>), String> {
    let labels_with_lines = [
        "Function", "Method", "Struct", "Enum", "Trait",
    ];
    for label in labels_with_lines {
        let cypher = format!(
            "MATCH (n:{label}) WHERE n.qualified_name = '{escaped}' OR n.id = '{escaped}' \
             RETURN n.name, n.qualified_name, n.start_line, n.end_line, n.visibility"
        );
        if let Ok(qr) = store.execute_query(&cypher) {
            if let Some(row) = qr.rows.first() {
                if row.len() >= 5 {
                    return Ok((
                        label.to_string(),
                        row[0].clone(),
                        extract_file_path(&row[1]),
                        parse_opt_u64(&row[2]),
                        parse_opt_u64(&row[3]),
                        Some(row[4].clone()),
                    ));
                }
            }
        }
    }
    let labels_no_lines = ["Module", "Constant", "TypeAlias"];
    for label in labels_no_lines {
        let cypher = format!(
            "MATCH (n:{label}) WHERE n.qualified_name = '{escaped}' OR n.id = '{escaped}' \
             RETURN n.name, n.qualified_name"
        );
        if let Ok(qr) = store.execute_query(&cypher) {
            if let Some(row) = qr.rows.first() {
                if row.len() >= 2 {
                    return Ok((
                        label.to_string(),
                        row[0].clone(),
                        extract_file_path(&row[1]),
                        None,
                        None,
                        None,
                    ));
                }
            }
        }
    }
    Err(format!("symbol not found: {escaped}"))
}

fn find_related_out(store: &GraphStore, escaped: &str, prefix: &str) -> Vec<RelatedSymbol> {
    let mut related = Vec::new();
    for &(rel, from_label, to_label) in crate::graph_store::REL_TABLES {
        if !rel.starts_with(prefix) { continue; }
        // source: stages/stage-3b-v2.md §5 — StdlibSymbol targets are
        // infrastructure-only (used for analysis + query_graph precision
        // metrics) and explicitly excluded from the flat get_context
        // calls/called_by result. Agents asking "what does X call?" expect
        // user-code callees, not every framework/std method implicitly
        // invoked.
        if to_label == crate::graph_store::NODE_STDLIB_SYMBOL { continue; }
        let cypher = format!(
            "MATCH (a:{from_label})-[:{rel}]->(b:{to_label}) \
             WHERE a.qualified_name = '{escaped}' OR a.id = '{escaped}' \
             RETURN b.name, b.qualified_name"
        );
        if let Ok(qr) = store.execute_query(&cypher) {
            for row in &qr.rows {
                if row.len() >= 2 {
                    related.push(RelatedSymbol {
                        name: row[0].clone(),
                        qualified_name: row[1].clone(),
                        label: to_label.to_string(),
                    });
                }
            }
        }
    }
    related
}

fn find_related_in(store: &GraphStore, escaped: &str, prefix: &str) -> Vec<RelatedSymbol> {
    let mut related = Vec::new();
    for &(rel, from_label, to_label) in crate::graph_store::REL_TABLES {
        if !rel.starts_with(prefix) { continue; }
        // source: see find_related_out — symmetric exclusion.
        if to_label == crate::graph_store::NODE_STDLIB_SYMBOL { continue; }
        let cypher = format!(
            "MATCH (a:{from_label})-[:{rel}]->(b:{to_label}) \
             WHERE b.qualified_name = '{escaped}' OR b.id = '{escaped}' \
             RETURN a.name, a.qualified_name"
        );
        if let Ok(qr) = store.execute_query(&cypher) {
            for row in &qr.rows {
                if row.len() >= 2 {
                    related.push(RelatedSymbol {
                        name: row[0].clone(),
                        qualified_name: row[1].clone(),
                        label: from_label.to_string(),
                    });
                }
            }
        }
    }
    related
}

fn find_community(store: &GraphStore, escaped: &str) -> Option<CommunityInfo> {
    let labels = ["Function", "Method", "Struct", "Enum", "Trait",
                  "Module", "Constant", "TypeAlias"];
    for label in labels {
        let rel = format!("MemberOf_{label}_Community");
        let cypher = format!(
            "MATCH (n:{label})-[:{rel}]->(c:Community) \
             WHERE n.qualified_name = '{escaped}' OR n.id = '{escaped}' \
             RETURN c.id, c.name, c.member_count LIMIT 1"
        );
        if let Ok(qr) = store.execute_query(&cypher) {
            if let Some(row) = qr.rows.first() {
                if row.len() >= 3 {
                    return Some(CommunityInfo {
                        id: row[0].clone(),
                        name: row[1].clone(),
                        member_count: row[2].parse::<u64>().unwrap_or(0),
                    });
                }
            }
        }
    }
    None
}

fn find_processes(store: &GraphStore, escaped: &str) -> Vec<ProcessRef> {
    let mut procs = Vec::new();
    for label in ["Function", "Method"] {
        let ep_rel = format!("EntryPointOf_{label}_Process");
        let cypher = format!(
            "MATCH (n:{label})-[:{ep_rel}]->(p:Process) \
             WHERE n.qualified_name = '{escaped}' OR n.id = '{escaped}' \
             RETURN p.name"
        );
        if let Ok(qr) = store.execute_query(&cypher) {
            for row in &qr.rows {
                if !row.is_empty() {
                    procs.push(ProcessRef {
                        name: row[0].clone(),
                        role: "entry_point".to_string(),
                    });
                }
            }
        }
        let part_rel = format!("ParticipatesIn_{label}_Process");
        let cypher = format!(
            "MATCH (n:{label})-[:{part_rel}]->(p:Process) \
             WHERE n.qualified_name = '{escaped}' OR n.id = '{escaped}' \
             RETURN p.name"
        );
        if let Ok(qr) = store.execute_query(&cypher) {
            for row in &qr.rows {
                if !row.is_empty() {
                    let pname = &row[0];
                    if !procs.iter().any(|pr| pr.name == *pname) {
                        procs.push(ProcessRef {
                            name: pname.clone(),
                            role: "participant".to_string(),
                        });
                    }
                }
            }
        }
    }
    procs
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_term_score_exact() {
        assert_eq!(term_score("main", "main", "src/main.rs::main"), 1.0);
    }

    #[test]
    fn test_term_score_contains_name() {
        let s = term_score("handle", "handle_tool_call", "src/main.rs::handle_tool_call");
        assert!(s > 0.7 && s < 1.0, "expected 0.7..1.0, got {s}");
    }

    #[test]
    fn test_term_score_contains_qn_only() {
        let s = term_score("main.rs", "handle_tool_call", "src/main.rs::handle_tool_call");
        assert!(s > 0.5 && s < 1.0, "expected 0.5..1.0, got {s}");
    }

    #[test]
    fn test_term_score_no_match() {
        assert_eq!(term_score("zzzzz", "main", "src/main.rs::main"), 0.0);
    }

    #[test]
    fn test_extract_file_path() {
        assert_eq!(extract_file_path("src/main.rs::handle_tool_call"), "src/main.rs");
        assert_eq!(extract_file_path("src/lib.rs"), "src/lib.rs");
    }

    #[test]
    fn test_strip_leading_path_component() {
        // source: C-correctness bug 2 — callers pass `src/main.rs::foo`,
        // the graph stores `main.rs::foo`. Layer 2 of the three-layer lookup
        // drops the first path component and retries.
        assert_eq!(
            strip_leading_path_component("src/main.rs::foo"),
            Some("main.rs::foo".to_string())
        );
        assert_eq!(
            strip_leading_path_component("src/foo/bar.rs::baz"),
            Some("foo/bar.rs::baz".to_string())
        );
        // No slash → nothing to strip.
        assert_eq!(strip_leading_path_component("main.rs::foo"), None);
        // Slash in the `::`-suffix is ignored — only the path part is split.
        assert_eq!(
            strip_leading_path_component("src/main.rs::Foo::bar"),
            Some("main.rs::Foo::bar".to_string())
        );
    }

    // Integration tests — build a real graph from src/ and exercise the
    // three-layer qualified-name lookup end to end.
    use crate::graph_store::GraphStore;
    use crate::indexer::index_codebase;
    use std::path::Path;

    fn fresh_store(tag: &str) -> (std::path::PathBuf, GraphStore) {
        let tmp = std::env::temp_dir()
            .join(format!("search_test_{}_{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let r = index_codebase(Path::new("src"), &tmp).unwrap();
        let store = GraphStore::open_or_create(&r.graph_path).unwrap();
        (tmp, store)
    }

    #[test]
    fn test_get_context_accepts_src_prefix() {
        // Callers passing `src/main.rs::handle_tool_call` must resolve to the
        // stored `main.rs::handle_tool_call`.
        let (tmp, store) = fresh_store("src_prefix");
        let resolved = resolve_qualified_name(&store, "src/main.rs::handle_tool_call")
            .expect("src/-prefixed qualified_name must resolve");
        assert_eq!(resolved, "main.rs::handle_tool_call");

        let ctx = get_context(&store, "src/main.rs::handle_tool_call")
            .map_err(|e| match e {
                GetContextError::NotFound(nf) => format!("not found: {:?}", nf.did_you_mean),
                GetContextError::Other(s) => s,
            })
            .expect("get_context must succeed with src/ prefix");
        assert_eq!(ctx.qualified_name, "main.rs::handle_tool_call");
        assert_eq!(ctx.name, "handle_tool_call");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_get_context_did_you_mean() {
        // An unknown qualified_name must fail with suggestions, not a
        // flat string error.
        let (tmp, store) = fresh_store("did_you_mean");
        let err = get_context(&store, "nonexistent.rs::handle_tool_call")
            .err()
            .expect("unknown qn must error");
        match err {
            GetContextError::NotFound(nf) => {
                assert_eq!(nf.input, "nonexistent.rs::handle_tool_call");
                assert!(
                    !nf.did_you_mean.is_empty(),
                    "did_you_mean must include candidates by name"
                );
                assert!(
                    nf.did_you_mean.iter().any(|s| s.ends_with("::handle_tool_call")),
                    "expected a `handle_tool_call` candidate, got {:?}",
                    nf.did_you_mean
                );
            }
            GetContextError::Other(m) => panic!("expected NotFound, got Other({m})"),
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
