// scoring.rs — four score types + weighted aggregator + per-language floor.
//
// Spec: stages/stage-3b-v2.md §2.3.
//   - Q1-Q3, Q10: exact-match on fully-qualified name (1.0 | 0.0).
//   - Q4-Q9, Q11, Q14: F1 against hand-labeled set.
//   - Q12: adjusted Rand index against labeled partition.
//   - Q13: unweighted mean of precision on "flagged present" + recall on
//          "truly present".
//   - Q7: recall at depth=3 on golden transitive closure (implemented as
//          set-F1 for now; recall-only lives behind a flag).
//
// Weighted mean yields end_result_score per corpus.  Per-language floor of
// 0.75 gates the aggregate so average cannot hide a broken adapter.

use std::collections::{HashMap, HashSet};

/// One of the four score types.  Each query declares its type; the scorer
/// routes on this enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScoreType {
    ExactMatch,
    F1Set,
    AdjustedRand,
    PrecisionRecallMean,
}

/// Exact-match: both strings or both booleans equal.  Normalizes whitespace
/// and ASCII case.  Returns 1.0 iff equal, else 0.0.
pub fn score_exact_match(expected: &str, actual: &str) -> f64 {
    if expected.trim().eq_ignore_ascii_case(actual.trim()) {
        1.0
    } else {
        0.0
    }
}

/// F1 on two string sets.  Returns 1.0 iff both sets are empty (trivially
/// correct).  Returns 0.0 iff one side is empty and the other is not.
pub fn score_f1(expected: &[String], actual: &[String]) -> f64 {
    let exp: HashSet<&str> = expected.iter().map(String::as_str).collect();
    let act: HashSet<&str> = actual.iter().map(String::as_str).collect();
    if exp.is_empty() && act.is_empty() {
        return 1.0;
    }
    if exp.is_empty() || act.is_empty() {
        return 0.0;
    }
    let tp = exp.intersection(&act).count() as f64;
    let precision = tp / act.len() as f64;
    let recall = tp / exp.len() as f64;
    if precision + recall == 0.0 {
        0.0
    } else {
        2.0 * precision * recall / (precision + recall)
    }
}

/// Adjusted Rand Index between two partitions.  Hubert & Arabie (1985)
/// formulation:  ARI = (RI - E[RI]) / (max(RI) - E[RI]).
///
/// Input: two equal-length vectors of cluster labels (one per item, same
/// item order in both). Values >=0 for well-clustered items; the function
/// itself does not care about label identity — it compares the induced
/// partition structures.
pub fn score_adjusted_rand(labels_a: &[i64], labels_b: &[i64]) -> f64 {
    if labels_a.len() != labels_b.len() || labels_a.is_empty() {
        return 0.0;
    }
    let n = labels_a.len() as f64;
    let contingency = contingency_table(labels_a, labels_b);
    let row_sums = row_sums(&contingency);
    let col_sums = col_sums(&contingency);
    let sum_comb_cells: f64 = contingency
        .values()
        .map(|&c| n_choose_2(c as f64))
        .sum();
    let sum_comb_rows: f64 = row_sums.values().map(|&c| n_choose_2(c as f64)).sum();
    let sum_comb_cols: f64 = col_sums.values().map(|&c| n_choose_2(c as f64)).sum();
    let total_pairs = n_choose_2(n);
    if total_pairs == 0.0 {
        return 0.0;
    }
    let expected = (sum_comb_rows * sum_comb_cols) / total_pairs;
    let max_index = 0.5 * (sum_comb_rows + sum_comb_cols);
    let denom = max_index - expected;
    if denom.abs() < 1e-12 {
        return 1.0;
    }
    (sum_comb_cells - expected) / denom
}

fn n_choose_2(x: f64) -> f64 {
    if x < 2.0 {
        0.0
    } else {
        x * (x - 1.0) / 2.0
    }
}

fn contingency_table(a: &[i64], b: &[i64]) -> HashMap<(i64, i64), u64> {
    let mut m: HashMap<(i64, i64), u64> = HashMap::new();
    for (x, y) in a.iter().zip(b.iter()) {
        *m.entry((*x, *y)).or_insert(0) += 1;
    }
    m
}

fn row_sums(t: &HashMap<(i64, i64), u64>) -> HashMap<i64, u64> {
    let mut r: HashMap<i64, u64> = HashMap::new();
    for ((a, _), v) in t {
        *r.entry(*a).or_insert(0) += v;
    }
    r
}

fn col_sums(t: &HashMap<(i64, i64), u64>) -> HashMap<i64, u64> {
    let mut c: HashMap<i64, u64> = HashMap::new();
    for ((_, b), v) in t {
        *c.entry(*b).or_insert(0) += v;
    }
    c
}

/// Precision-recall mean (Q13).  `flagged_present` = items the tool said
/// exist; `truly_present` = ground truth.  Precision over flagged, recall
/// over truth, unweighted mean.
pub fn score_precision_recall_mean(flagged_present: &[String], truly_present: &[String]) -> f64 {
    let flagged: HashSet<&str> = flagged_present.iter().map(String::as_str).collect();
    let truth: HashSet<&str> = truly_present.iter().map(String::as_str).collect();
    if flagged.is_empty() && truth.is_empty() {
        return 1.0;
    }
    let tp = flagged.intersection(&truth).count() as f64;
    let precision = if flagged.is_empty() {
        0.0
    } else {
        tp / flagged.len() as f64
    };
    let recall = if truth.is_empty() {
        0.0
    } else {
        tp / truth.len() as f64
    };
    (precision + recall) / 2.0
}

/// Weighted mean of per-query scores.  Missing queries contribute 0 with
/// their declared weight (so omission counts as failure, not as "waived").
pub fn weighted_mean(scores: &HashMap<String, f64>, weights: &HashMap<String, f64>) -> f64 {
    let total_weight: f64 = weights.values().sum();
    if total_weight <= 0.0 {
        return 0.0;
    }
    let mut acc = 0.0;
    for (q, w) in weights {
        let s = scores.get(q).copied().unwrap_or(0.0);
        acc += w * s;
    }
    acc / total_weight
}

/// Apply the per-language floor gate (§2.3): no single language below 0.75.
/// Returns Ok iff every language clears the floor, else Err with the
/// offending language and score.
pub fn check_language_floor(
    per_language: &HashMap<String, f64>,
    floor: f64,
) -> Result<(), (String, f64)> {
    for (lang, score) in per_language {
        if *score < floor {
            return Err((lang.clone(), *score));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_match_hit() {
        assert_eq!(score_exact_match("main.rs::foo", "main.rs::foo"), 1.0);
    }

    #[test]
    fn exact_match_miss() {
        assert_eq!(score_exact_match("main.rs::foo", "main.rs::bar"), 0.0);
    }

    #[test]
    fn f1_all_hit() {
        let exp = vec!["a".to_string(), "b".to_string()];
        let act = vec!["a".to_string(), "b".to_string()];
        assert!((score_f1(&exp, &act) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn f1_half() {
        // expected={a,b}, actual={a,c}: tp=1, P=0.5, R=0.5, F1=0.5
        let exp = vec!["a".to_string(), "b".to_string()];
        let act = vec!["a".to_string(), "c".to_string()];
        assert!((score_f1(&exp, &act) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn f1_both_empty_is_perfect() {
        assert_eq!(score_f1(&[], &[]), 1.0);
    }

    #[test]
    fn f1_one_empty_is_zero() {
        let act = vec!["x".to_string()];
        assert_eq!(score_f1(&[], &act), 0.0);
    }

    #[test]
    fn ari_identical_partitions() {
        let a = vec![0, 0, 1, 1, 2, 2];
        let b = vec![5, 5, 3, 3, 9, 9];
        assert!((score_adjusted_rand(&a, &b) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn ari_random_partition() {
        // Totally disjoint labeling vs a uniform one should score near 0.
        let a = vec![0, 0, 0, 0];
        let b = vec![0, 1, 0, 1];
        let ari = score_adjusted_rand(&a, &b);
        assert!(ari.abs() < 0.5, "got {ari}");
    }

    #[test]
    fn pr_mean_perfect() {
        let f = vec!["a".to_string()];
        let t = vec!["a".to_string()];
        assert!((score_precision_recall_mean(&f, &t) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn pr_mean_half_precision_full_recall() {
        let f = vec!["a".to_string(), "b".to_string()]; // a is wrong flag
        let t = vec!["a".to_string()];
        // P=0.5, R=1.0 -> mean 0.75
        assert!((score_precision_recall_mean(&f, &t) - 0.75).abs() < 1e-9);
    }

    #[test]
    fn weighted_mean_missing_counts_as_zero() {
        let mut scores = HashMap::new();
        scores.insert("q1".to_string(), 1.0);
        let mut weights = HashMap::new();
        weights.insert("q1".to_string(), 0.5);
        weights.insert("q2".to_string(), 0.5);
        // q2 missing -> 0.  Mean = (0.5*1 + 0.5*0)/1 = 0.5
        assert!((weighted_mean(&scores, &weights) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn floor_pass() {
        let mut m = HashMap::new();
        m.insert("rust".to_string(), 0.85);
        m.insert("ts".to_string(), 0.80);
        assert!(check_language_floor(&m, 0.75).is_ok());
    }

    #[test]
    fn floor_fail() {
        let mut m = HashMap::new();
        m.insert("rust".to_string(), 0.85);
        m.insert("ts".to_string(), 0.60);
        let e = check_language_floor(&m, 0.75).unwrap_err();
        assert_eq!(e.0, "ts");
    }
}
