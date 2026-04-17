// report.rs — JSON + human + markdown report builders.
//
// Responsibilities:
//   - aggregate per-corpus CorpusRun into per-language averages
//   - apply the §2.3 production-grade verdict (mean >=0.85 AND per-lang floor
//     >=0.75)
//   - emit stdout JSON (machine-consumable), stderr human summary (operator),
//     and benches/runs/<timestamp>.md (historical archive)

use crate::queries;
use crate::runner::CorpusRun;
use crate::scoring;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// Production-grade target per §1.1.
pub const TARGET_SCORE: f64 = 0.85;
/// Per-language floor per §2.3.
pub const LANGUAGE_FLOOR: f64 = 0.75;

/// Final aggregated summary handed to renderers.
#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    pub target: f64,
    pub language_floor: f64,
    pub corpora: Vec<Value>,
    pub per_language: HashMap<String, f64>,
    pub per_query: HashMap<String, f64>,
    pub end_result_score: f64,
    pub min_language_score: f64,
    pub min_language_name: String,
    pub verdict_production_grade: bool,
    pub verdict_reason: String,
}

/// Build the full summary from raw corpus runs.  Only corpora with at least
/// one scored label contribute to the aggregate — stubs are reported as
/// "pending" entries without affecting the mean.
pub fn build_summary(runs: &[CorpusRun]) -> Summary {
    let corpora: Vec<Value> = runs.iter().map(render_corpus_value).collect();

    let scoring_runs: Vec<&CorpusRun> = runs
        .iter()
        .filter(|r| r.labels_run > 0 && r.setup_error.is_none())
        .collect();

    let per_language = group_mean_by(&scoring_runs, |r| r.language.clone(), |r| r.end_result_score);
    let per_query = aggregate_per_query(&scoring_runs);

    let end_result_score = if scoring_runs.is_empty() {
        0.0
    } else {
        scoring_runs.iter().map(|r| r.end_result_score).sum::<f64>()
            / scoring_runs.len() as f64
    };

    let (min_language_name, min_language_score) = per_language
        .iter()
        .min_by(|a, b| a.1.partial_cmp(b.1).unwrap())
        .map(|(k, v)| (k.clone(), *v))
        .unwrap_or_else(|| ("<none>".to_string(), 0.0));

    let (verdict_production_grade, verdict_reason) =
        verdict(end_result_score, &per_language, scoring_runs.is_empty());

    Summary {
        target: TARGET_SCORE,
        language_floor: LANGUAGE_FLOOR,
        corpora,
        per_language,
        per_query,
        end_result_score,
        min_language_score,
        min_language_name,
        verdict_production_grade,
        verdict_reason,
    }
}

fn render_corpus_value(r: &CorpusRun) -> Value {
    json!({
        "name": r.name,
        "language": r.language,
        "end_result_score": r.end_result_score,
        "index_elapsed_ms": r.index_elapsed_ms,
        "labels_run": r.labels_run,
        "labels_skipped": r.labels_skipped,
        "per_query_scores": r.per_query_scores,
        "per_query_samples": r.per_query_samples,
        "per_query_elapsed_ms": r.per_query_elapsed_ms,
        "setup_error": r.setup_error,
    })
}

fn group_mean_by<F, G>(runs: &[&CorpusRun], key: F, value: G) -> HashMap<String, f64>
where
    F: Fn(&CorpusRun) -> String,
    G: Fn(&CorpusRun) -> f64,
{
    let mut sums: HashMap<String, (f64, usize)> = HashMap::new();
    for r in runs {
        let k = key(r);
        let v = value(r);
        let slot = sums.entry(k).or_insert((0.0, 0));
        slot.0 += v;
        slot.1 += 1;
    }
    sums.into_iter()
        .map(|(k, (total, n))| (k, total / n as f64))
        .collect()
}

fn aggregate_per_query(runs: &[&CorpusRun]) -> HashMap<String, f64> {
    let mut sums: HashMap<String, (f64, usize)> = HashMap::new();
    for r in runs {
        for (q, s) in &r.per_query_scores {
            let n = r.per_query_samples.get(q).copied().unwrap_or(1);
            let slot = sums.entry(q.clone()).or_insert((0.0, 0));
            slot.0 += s * n as f64;
            slot.1 += n;
        }
    }
    sums.into_iter()
        .map(|(q, (total, n))| (q, if n == 0 { 0.0 } else { total / n as f64 }))
        .collect()
}

fn verdict(
    end_result_score: f64,
    per_language: &HashMap<String, f64>,
    empty: bool,
) -> (bool, String) {
    if empty {
        return (false, "no scoring runs".to_string());
    }
    if end_result_score < TARGET_SCORE {
        return (
            false,
            format!(
                "end_result_score {:.3} < target {:.2}",
                end_result_score, TARGET_SCORE
            ),
        );
    }
    match scoring::check_language_floor(per_language, LANGUAGE_FLOOR) {
        Ok(()) => (true, "meets target + floor".to_string()),
        Err((lang, score)) => (
            false,
            format!(
                "per-language floor breached by {} ({:.3} < {:.2})",
                lang, score, LANGUAGE_FLOOR
            ),
        ),
    }
}

/// JSON form for stdout (machine consumers, CI dashboards).
pub fn to_json(summary: &Summary) -> String {
    serde_json::to_string_pretty(summary).unwrap_or_else(|_| "{}".to_string())
}

/// Human-readable form for stderr.
pub fn to_human(summary: &Summary) -> String {
    let mut out = String::new();
    out.push_str("== end_result_score by corpus ==\n");
    for c in &summary.corpora {
        let name = c.get("name").and_then(|v| v.as_str()).unwrap_or("?");
        let lang = c.get("language").and_then(|v| v.as_str()).unwrap_or("?");
        let err = c.get("setup_error").and_then(|v| v.as_str());
        let labels = c.get("labels_run").and_then(|v| v.as_u64()).unwrap_or(0);
        let score = c
            .get("end_result_score")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        if let Some(e) = err {
            out.push_str(&format!("  {name:24} ({lang}): SETUP_ERROR — {e}\n"));
        } else if labels == 0 {
            out.push_str(&format!(
                "  {name:24} ({lang}): TODO — no labels yet\n"
            ));
        } else {
            let flag = if score >= LANGUAGE_FLOOR { "OK" } else { "BELOW FLOOR" };
            out.push_str(&format!(
                "  {name:24} ({lang}): {score:.3}  [{labels} labels, floor {:.2}] {flag}\n",
                LANGUAGE_FLOOR
            ));
        }
    }
    out.push_str("\n== by query ==\n");
    for qs in queries::all_queries() {
        let s = summary.per_query.get(qs.id);
        let line = match s {
            Some(v) => format!(
                "  {:<3} {:<30} {:.3}  weight={:.3}\n",
                qs.id, qs.description, v, qs.weight
            ),
            None => format!(
                "  {:<3} {:<30} (no labels)  weight={:.3}\n",
                qs.id, qs.description, qs.weight
            ),
        };
        out.push_str(&line);
    }
    out.push_str("\n== aggregate ==\n");
    out.push_str(&format!(
        "  end_result_score: {:.3}  (target: >={:.2})\n",
        summary.end_result_score, summary.target
    ));
    out.push_str(&format!(
        "  min_per_language: {} = {:.3}  (floor: >={:.2})\n",
        summary.min_language_name, summary.min_language_score, summary.language_floor
    ));
    let verdict = if summary.verdict_production_grade {
        "PRODUCTION-GRADE"
    } else {
        "NOT PRODUCTION-GRADE"
    };
    out.push_str(&format!(
        "  verdict: {} — {}\n",
        verdict, summary.verdict_reason
    ));
    out
}

/// Write the markdown archive into `dir/<unix_ts>.md`.
pub fn write_markdown(dir: &Path, summary: &Summary) -> Result<(), String> {
    fs::create_dir_all(dir).map_err(|e| format!("create {:?}: {e}", dir))?;
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let path = dir.join(format!("{ts}.md"));
    let body = markdown_body(summary);
    fs::write(&path, body).map_err(|e| format!("write {:?}: {e}", path))?;
    eprintln!("[bench] markdown report: {:?}", path);
    Ok(())
}

fn markdown_body(summary: &Summary) -> String {
    let mut out = String::new();
    out.push_str("# End-result evaluation — run summary\n\n");
    out.push_str(&format!(
        "- Target: {:.2}\n- Per-language floor: {:.2}\n- Verdict: {} ({})\n",
        summary.target,
        summary.language_floor,
        if summary.verdict_production_grade {
            "PRODUCTION-GRADE"
        } else {
            "NOT PRODUCTION-GRADE"
        },
        summary.verdict_reason
    ));
    out.push_str("\n## Corpora\n\n");
    out.push_str("| corpus | lang | score | labels | index_ms | notes |\n");
    out.push_str("|---|---|---|---|---|---|\n");
    for c in &summary.corpora {
        let name = c.get("name").and_then(|v| v.as_str()).unwrap_or("?");
        let lang = c.get("language").and_then(|v| v.as_str()).unwrap_or("?");
        let score = c
            .get("end_result_score")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let labels = c.get("labels_run").and_then(|v| v.as_u64()).unwrap_or(0);
        let index_ms = c
            .get("index_elapsed_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let notes = c
            .get("setup_error")
            .and_then(|v| v.as_str())
            .map(|e| format!("ERROR: {e}"))
            .unwrap_or_else(|| {
                if labels == 0 {
                    "TODO — no labels".to_string()
                } else {
                    String::new()
                }
            });
        out.push_str(&format!(
            "| {name} | {lang} | {score:.3} | {labels} | {index_ms} | {notes} |\n"
        ));
    }
    out.push_str("\n## Per-query aggregate\n\n");
    out.push_str("| query | description | score |\n|---|---|---|\n");
    for qs in queries::all_queries() {
        let s = summary
            .per_query
            .get(qs.id)
            .map(|v| format!("{v:.3}"))
            .unwrap_or_else(|| "—".to_string());
        out.push_str(&format!("| {} | {} | {} |\n", qs.id, qs.description, s));
    }
    out.push_str(&format!(
        "\n## Aggregate\n\n- end_result_score: **{:.3}**\n- min per-language: {} = {:.3}\n",
        summary.end_result_score, summary.min_language_name, summary.min_language_score
    ));
    out
}
