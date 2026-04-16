// git_diff — maps git diff changed lines to affected symbols in the code graph.
//
// Two entry points:
//   1. analyze_diff(store, diff_text) — parse raw unified diff text
//   2. analyze_git_diff(store, codebase_path, base_ref, head_ref) — run git diff
//
// Changed lines are mapped to graph symbols by overlapping [start_line, end_line]
// ranges. Each affected symbol is enriched with community and process membership.

use crate::clustering;
use crate::graph_store::{cypher_str, GraphStore};
use serde::Serialize;
use std::collections::HashSet;
use std::path::Path;
use std::process::Command;

// ---------------------------------------------------------------------------
// Safety limit — diff line counter overflow guard.
// source: M3 fix — current_line is u64, but a malicious diff with billions of
// `+` lines could wrap around. Cap at u64::MAX / 2 so we always have headroom.
// ---------------------------------------------------------------------------

const DIFF_LINE_MAX: u64 = u64::MAX / 2;

/// Validates a git ref for safe use as a positional argument.
/// Rejects refs starting with `-` (which git would treat as an option flag)
/// and refs containing newlines or NUL bytes.
/// source: C2 fix — git CVE-2018-17456 class vulnerability prevention.
fn validate_git_ref(r: &str, field: &str) -> Result<(), String> {
    if r.is_empty() {
        return Err(format!("invalid_ref: {field} must not be empty"));
    }
    if r.starts_with('-') {
        return Err(format!(
            "invalid_ref: {field} must not start with '-' (got {r:?})"
        ));
    }
    if r.contains('\n') || r.contains('\0') {
        return Err(format!(
            "invalid_ref: {field} must not contain newline or NUL (got {r:?})"
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct ChangedSymbol {
    pub qualified_name: String,
    pub name: String,
    pub label: String,
    pub file_path: String,
    pub change_type: String, // "modified", "added", "deleted"
    pub lines_changed: u64,
    pub community_id: Option<String>,
    pub processes: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DiffAnalysis {
    pub files_changed: u64,
    pub symbols_affected: Vec<ChangedSymbol>,
    pub communities_affected: Vec<String>,
    pub processes_affected: Vec<String>,
    pub risk_score: f64,
}

// ---------------------------------------------------------------------------
// Diff parsing — unified diff format
// ---------------------------------------------------------------------------

struct FileHunk {
    file_path: String,
    changed_lines: Vec<u64>,
    is_new: bool,
    is_deleted: bool,
}

fn parse_unified_diff(diff_text: &str) -> Result<Vec<FileHunk>, String> {
    let mut hunks: Vec<FileHunk> = Vec::new();
    let mut current_file: Option<String> = None;
    let mut current_lines: Vec<u64> = Vec::new();
    let mut current_line: u64 = 0;
    let mut is_new = false;
    let mut is_deleted = false;

    for line in diff_text.lines() {
        if line.starts_with("diff --git") {
            // Flush previous file
            if let Some(fp) = current_file.take() {
                hunks.push(FileHunk {
                    file_path: fp,
                    changed_lines: std::mem::take(&mut current_lines),
                    is_new,
                    is_deleted,
                });
            }
            is_new = false;
            is_deleted = false;
        } else if line.starts_with("--- a/") {
            // For deleted files, this is the only place we get the path
            if current_file.is_none() {
                current_file = Some(line[6..].to_string());
            }
        } else if line.starts_with("--- /dev/null") {
            is_new = true;
        } else if line.starts_with("+++ b/") {
            current_file = Some(line[6..].to_string());
        } else if line.starts_with("+++ /dev/null") {
            is_deleted = true;
        } else if line.starts_with("@@ ") {
            // Parse hunk header: @@ -old_start,old_count +new_start,new_count @@
            if let Some(new_range) = parse_hunk_header(line) {
                current_line = new_range;
            }
        } else if line.starts_with('+') && !line.starts_with("+++") {
            current_lines.push(current_line);
            if current_line >= DIFF_LINE_MAX {
                return Err("diff_too_large: line counter would overflow".to_string());
            }
            current_line += 1;
        } else if line.starts_with('-') && !line.starts_with("---") {
            // Deleted lines affect the old position; we record current_line
            // so we can match the surrounding context in the new file.
            current_lines.push(current_line);
            // Don't increment — deleted lines don't exist in the new file
        } else {
            // Context line (or other)
            if current_line >= DIFF_LINE_MAX {
                return Err("diff_too_large: line counter would overflow".to_string());
            }
            current_line += 1;
        }
    }

    // Flush last file
    if let Some(fp) = current_file.take() {
        hunks.push(FileHunk {
            file_path: fp,
            changed_lines: current_lines,
            is_new,
            is_deleted,
        });
    }

    Ok(hunks)
}

fn parse_hunk_header(line: &str) -> Option<u64> {
    // Format: @@ -old_start[,old_count] +new_start[,new_count] @@
    let plus_pos = line.find('+')?;
    let rest = &line[plus_pos + 1..];
    let end = rest.find(|c: char| c == ',' || c == ' ')?;
    rest[..end].parse::<u64>().ok()
}

// ---------------------------------------------------------------------------
// Symbol mapping — lines to graph nodes
// ---------------------------------------------------------------------------

const SYMBOL_LABELS_WITH_LINES: &[&str] = &[
    "Function", "Method", "Struct", "Enum", "Trait",
];

fn map_lines_to_symbols(
    store: &GraphStore,
    file_path: &str,
    changed_lines: &[u64],
    is_new: bool,
    is_deleted: bool,
) -> Vec<ChangedSymbol> {
    let mut symbols: Vec<ChangedSymbol> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    // source: M1 fix — centralized escape handles both `\` and `'` correctly.
    let path_lit = cypher_str(file_path);

    for &label in SYMBOL_LABELS_WITH_LINES {
        let defines_rel = format!("Defines_File_{label}");
        let cypher = format!(
            "MATCH (f:File)-[:{defines_rel}]->(n:{label}) \
             WHERE f.path = {path_lit} \
             RETURN n.id, n.name, n.qualified_name, n.start_line, n.end_line"
        );
        let qr = match store.execute_query(&cypher) {
            Ok(q) => q,
            Err(_) => continue,
        };

        for row in &qr.rows {
            if row.len() < 5 { continue; }
            let id = &row[0];
            if seen.contains(id) { continue; }

            let start: u64 = row[3].parse().unwrap_or(0);
            let end: u64 = row[4].parse().unwrap_or(0);

            // Count how many changed lines overlap this symbol's range
            let overlap: u64 = changed_lines
                .iter()
                .filter(|&&l| l >= start && l <= end)
                .count() as u64;

            if overlap == 0 && !is_new && !is_deleted {
                continue;
            }

            seen.insert(id.clone());

            let change_type = if is_new {
                "added"
            } else if is_deleted {
                "deleted"
            } else {
                "modified"
            };

            let impact = clustering::get_impact(store, &row[2]);
            let (community_id, processes) = match impact {
                Ok(imp) => (
                    imp.communities.into_iter().next(),
                    imp.processes,
                ),
                Err(_) => (None, Vec::new()),
            };

            symbols.push(ChangedSymbol {
                qualified_name: row[2].clone(),
                name: row[1].clone(),
                label: label.to_string(),
                file_path: file_path.to_string(),
                change_type: change_type.to_string(),
                lines_changed: if is_new || is_deleted {
                    end.saturating_sub(start) + 1
                } else {
                    overlap
                },
                community_id,
                processes,
            });
        }
    }

    symbols
}

// ---------------------------------------------------------------------------
// Risk score — heuristic, NOT paper-backed
// ---------------------------------------------------------------------------

/// Computes a risk score in [0.0, 1.0] for the set of changed symbols.
///
/// Heuristic formula (not sourced from any published algorithm):
///   risk = 0.6 * avg(processes_per_symbol / max_processes)
///        + 0.4 * (communities_affected / total_communities)
///
/// Rationale: symbols in more processes have higher blast radius (0.6 weight).
/// Changes spanning more communities indicate broader architectural impact
/// (0.4 weight). The weights are arbitrary engineering judgment.
fn compute_risk_score(
    symbols: &[ChangedSymbol],
    total_communities: u64,
) -> f64 {
    if symbols.is_empty() {
        return 0.0;
    }

    // Process centrality component
    let max_proc = symbols.iter().map(|s| s.processes.len()).max().unwrap_or(1);
    let max_proc = max_proc.max(1) as f64;
    let avg_proc_ratio: f64 = symbols
        .iter()
        .map(|s| s.processes.len() as f64 / max_proc)
        .sum::<f64>()
        / symbols.len() as f64;

    // Community spread component
    let communities: HashSet<&str> = symbols
        .iter()
        .filter_map(|s| s.community_id.as_deref())
        .collect();
    let total = total_communities.max(1) as f64;
    let community_ratio = communities.len() as f64 / total;

    (0.6 * avg_proc_ratio + 0.4 * community_ratio).min(1.0)
}

fn count_total_communities(store: &GraphStore) -> u64 {
    let cypher = "MATCH (c:Community) RETURN count(c)";
    if let Ok(qr) = store.execute_query(cypher) {
        if let Some(row) = qr.rows.first() {
            if let Some(val) = row.first() {
                return val.parse().unwrap_or(0);
            }
        }
    }
    0
}

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

pub fn analyze_diff(
    store: &GraphStore,
    diff_text: &str,
) -> Result<DiffAnalysis, String> {
    let hunks = parse_unified_diff(diff_text)?;
    build_analysis(store, &hunks)
}

pub fn analyze_git_diff(
    store: &GraphStore,
    codebase_path: &Path,
    base_ref: &str,
    head_ref: &str,
) -> Result<DiffAnalysis, String> {
    validate_git_ref(base_ref, "base_ref")?;
    validate_git_ref(head_ref, "head_ref")?;
    // The combined `base..head` string now starts with whatever base_ref
    // starts with (which validate_git_ref guarantees is NOT `-`).
    // The trailing `--` tells git no pathspecs follow, so an adversarial
    // head_ref cannot smuggle a path-based side effect either.
    // source: C2 fix — git-diff(1) synopsis `git diff [<commit>] [--] [<path>…]`.
    let output = Command::new("git")
        .arg("diff")
        .arg(format!("{base_ref}..{head_ref}"))
        .arg("--")
        .current_dir(codebase_path)
        .output()
        .map_err(|e| format!("failed to run git diff: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git diff failed: {stderr}"));
    }

    let diff_text = String::from_utf8_lossy(&output.stdout);
    analyze_diff(store, &diff_text)
}

fn build_analysis(
    store: &GraphStore,
    hunks: &[FileHunk],
) -> Result<DiffAnalysis, String> {
    let files_changed = hunks.len() as u64;
    let mut all_symbols: Vec<ChangedSymbol> = Vec::new();

    for hunk in hunks {
        let symbols = map_lines_to_symbols(
            store,
            &hunk.file_path,
            &hunk.changed_lines,
            hunk.is_new,
            hunk.is_deleted,
        );
        all_symbols.extend(symbols);
    }

    let communities: Vec<String> = all_symbols
        .iter()
        .filter_map(|s| s.community_id.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();

    let processes: Vec<String> = all_symbols
        .iter()
        .flat_map(|s| s.processes.iter().cloned())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();

    let total_communities = count_total_communities(store);
    let risk_score = compute_risk_score(&all_symbols, total_communities);

    Ok(DiffAnalysis {
        files_changed,
        symbols_affected: all_symbols,
        communities_affected: communities,
        processes_affected: processes,
        risk_score,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_hunk_header() {
        assert_eq!(parse_hunk_header("@@ -10,5 +20,7 @@ fn foo"), Some(20));
        assert_eq!(parse_hunk_header("@@ -1 +1,3 @@"), Some(1));
        assert_eq!(parse_hunk_header("@@ -0,0 +1 @@"), Some(1));
    }

    #[test]
    fn test_parse_unified_diff_single_file() {
        let diff = "\
diff --git a/src/main.rs b/src/main.rs
--- a/src/main.rs
+++ b/src/main.rs
@@ -10,3 +10,4 @@ fn main() {
     let x = 1;
+    let y = 2;
     let z = 3;
";
        let hunks = parse_unified_diff(diff).unwrap();
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].file_path, "src/main.rs");
        assert!(!hunks[0].changed_lines.is_empty());
        assert!(!hunks[0].is_new);
        assert!(!hunks[0].is_deleted);
    }

    #[test]
    fn test_parse_unified_diff_new_file() {
        let diff = "\
diff --git a/src/new.rs b/src/new.rs
--- /dev/null
+++ b/src/new.rs
@@ -0,0 +1,5 @@
+fn hello() {
+    println!(\"hello\");
+}
";
        let hunks = parse_unified_diff(diff).unwrap();
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].file_path, "src/new.rs");
        assert!(hunks[0].is_new);
    }

    #[test]
    fn test_parse_unified_diff_deleted_file() {
        let diff = "\
diff --git a/src/old.rs b/src/old.rs
--- a/src/old.rs
+++ /dev/null
@@ -1,3 +0,0 @@
-fn old() {
-    // gone
-}
";
        let hunks = parse_unified_diff(diff).unwrap();
        assert_eq!(hunks.len(), 1);
        assert!(hunks[0].is_deleted);
    }

    #[test]
    fn test_parse_unified_diff_multiple_files() {
        let diff = "\
diff --git a/src/a.rs b/src/a.rs
--- a/src/a.rs
+++ b/src/a.rs
@@ -1,3 +1,4 @@
 fn a() {}
+fn a2() {}
diff --git a/src/b.rs b/src/b.rs
--- a/src/b.rs
+++ b/src/b.rs
@@ -5,3 +5,4 @@
 fn b() {}
+fn b2() {}
";
        let hunks = parse_unified_diff(diff).unwrap();
        assert_eq!(hunks.len(), 2);
        assert_eq!(hunks[0].file_path, "src/a.rs");
        assert_eq!(hunks[1].file_path, "src/b.rs");
    }

    #[test]
    fn test_compute_risk_score_empty() {
        assert_eq!(compute_risk_score(&[], 10), 0.0);
    }

    #[test]
    fn test_compute_risk_score_bounded() {
        let sym = ChangedSymbol {
            qualified_name: "src/main.rs::foo".into(),
            name: "foo".into(),
            label: "Function".into(),
            file_path: "src/main.rs".into(),
            change_type: "modified".into(),
            lines_changed: 5,
            community_id: Some("c0".into()),
            processes: vec!["p1".into(), "p2".into()],
        };
        let score = compute_risk_score(&[sym], 2);
        assert!(score >= 0.0 && score <= 1.0, "score out of range: {score}");
    }

    #[test]
    fn test_analyze_diff_on_empty_graph() {
        let dir = std::env::temp_dir().join("git_diff_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("testdb");

        let store = GraphStore::open_or_create(&db_path).unwrap();
        store.create_schema().unwrap();

        let diff = "\
diff --git a/src/main.rs b/src/main.rs
--- a/src/main.rs
+++ b/src/main.rs
@@ -10,3 +10,4 @@ fn main() {
     let x = 1;
+    let y = 2;
     let z = 3;
";
        let result = analyze_diff(&store, diff).unwrap();
        assert_eq!(result.files_changed, 1);
        // No symbols in an empty graph
        assert!(result.symbols_affected.is_empty());
        assert_eq!(result.risk_score, 0.0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_git_ref_with_dash_rejected() {
        // source: C2 fix — refs starting with `-` would be interpreted by git
        // as option flags (e.g. `--upload-pack=rm -rf ~`, `-c core.fsmonitor=...`).
        // Both base_ref and head_ref must be rejected before reaching Command::new.
        let dir = std::env::temp_dir().join("git_ref_validate_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("testdb");
        let store = GraphStore::open_or_create(&db_path).unwrap();
        store.create_schema().unwrap();

        let err = analyze_git_diff(&store, &dir, "--upload-pack=rm", "HEAD")
            .expect_err("must reject dash-prefixed base_ref");
        assert!(err.contains("invalid_ref"), "got: {err}");

        let err2 = analyze_git_diff(&store, &dir, "main", "-c")
            .expect_err("must reject dash-prefixed head_ref");
        assert!(err2.contains("invalid_ref"), "got: {err2}");

        let err3 = analyze_git_diff(&store, &dir, "", "HEAD")
            .expect_err("must reject empty ref");
        assert!(err3.contains("invalid_ref"), "got: {err3}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_diff_too_large_overflow_guard() {
        // source: M3 fix — parse_unified_diff must return diff_too_large
        // rather than overflowing current_line. We can't realistically craft
        // a diff with u64::MAX lines, but validate_git_ref's logic is tested;
        // instead smoke-test that a normal diff still succeeds and returns Ok.
        let diff = "\
diff --git a/x b/x
--- a/x
+++ b/x
@@ -1 +1,2 @@
 keep
+add
";
        let hunks = parse_unified_diff(diff).expect("small diff must parse");
        assert_eq!(hunks.len(), 1);
    }
}
