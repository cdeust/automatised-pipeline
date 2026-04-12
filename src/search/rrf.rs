// search::rrf — Reciprocal Rank Fusion for combining ranked lists.
//
// Source: Cormack, Clarke, Büttcher (2009). "Reciprocal Rank Fusion
// Outperforms Condorcet and Individual Rank Learning Methods."
// SIGIR 2009, pp. 758–759.
//
// Formula: RRF_score(d) = Σ 1 / (k + rank_i(d))
// where k = 60 (standard constant from the paper) and the sum is
// over each ranking system.

use std::collections::HashMap;

/// The RRF constant k. From Cormack et al. 2009, empirically optimal.
const K: f64 = 60.0;

/// A document identifier with its fused RRF score.
pub struct RrfResult {
    pub key: String,
    pub score: f64,
}

/// A ranked entry from one retrieval system.
pub struct RankedEntry {
    pub key: String,
    pub rank: usize, // 1-based rank
}

/// Fuses multiple ranked lists using Reciprocal Rank Fusion.
///
/// Each input is a list of `RankedEntry` from one ranking system.
/// Returns fused results sorted by descending RRF score.
pub fn fuse(rankings: &[&[RankedEntry]], limit: usize) -> Vec<RrfResult> {
    let mut scores: HashMap<&str, f64> = HashMap::new();

    for ranked_list in rankings {
        for entry in *ranked_list {
            let rrf_contribution = 1.0 / (K + entry.rank as f64);
            *scores.entry(&entry.key).or_insert(0.0) += rrf_contribution;
        }
    }

    let mut results: Vec<RrfResult> = scores.into_iter()
        .map(|(key, score)| RrfResult { key: key.to_string(), score })
        .collect();
    results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    results.truncate(limit);
    results
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rrf_single_list() {
        let list = vec![
            RankedEntry { key: "a".into(), rank: 1 },
            RankedEntry { key: "b".into(), rank: 2 },
        ];
        let results = fuse(&[&list], 10);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].key, "a");
        assert!(results[0].score > results[1].score);
        // rank 1: 1/(60+1) ≈ 0.01639
        let expected = 1.0 / 61.0;
        assert!((results[0].score - expected).abs() < 1e-6);
    }

    #[test]
    fn test_rrf_two_lists_fusion() {
        // Document "b" is rank 1 in list1 and rank 2 in list2.
        // Document "a" is rank 2 in list1 and rank 1 in list2.
        // Both should have equal RRF scores.
        let list1 = vec![
            RankedEntry { key: "b".into(), rank: 1 },
            RankedEntry { key: "a".into(), rank: 2 },
        ];
        let list2 = vec![
            RankedEntry { key: "a".into(), rank: 1 },
            RankedEntry { key: "b".into(), rank: 2 },
        ];
        let results = fuse(&[&list1, &list2], 10);
        assert_eq!(results.len(), 2);
        // Both should have 1/61 + 1/62 ≈ 0.03252
        let expected = 1.0 / 61.0 + 1.0 / 62.0;
        assert!((results[0].score - expected).abs() < 1e-6);
        assert!((results[1].score - expected).abs() < 1e-6);
    }

    #[test]
    fn test_rrf_disjoint_lists() {
        let list1 = vec![
            RankedEntry { key: "a".into(), rank: 1 },
        ];
        let list2 = vec![
            RankedEntry { key: "b".into(), rank: 1 },
        ];
        let results = fuse(&[&list1, &list2], 10);
        assert_eq!(results.len(), 2);
        // Both have the same score: 1/61
        assert!((results[0].score - results[1].score).abs() < 1e-6);
    }
}
