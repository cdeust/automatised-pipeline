// queries.rs — the 14 canonical queries (spec §2.1) as typed structs.
//
// Each query knows:
//   - its id (q1..q14)
//   - which MCP tool to call, with which argument shape
//   - how to score the response (ScoreType)
//   - its weight in the aggregate (§2.3)
//
// We don't hold the score function inline — the runner looks at ScoreType and
// dispatches.  This keeps queries declarative (easy to add) and scoring in
// one place.

use crate::scoring::ScoreType;
use std::collections::HashMap;

/// Static metadata for each canonical query.
#[derive(Debug, Clone)]
pub struct QuerySpec {
    pub id: &'static str,
    pub description: &'static str,
    pub tool: &'static str,
    pub score_type: ScoreType,
    pub weight: f64,
}

/// The 14 canonical queries.  Weights come from §2.3, normalized to sum to 1.
pub fn all_queries() -> Vec<QuerySpec> {
    vec![
        QuerySpec {
            id: "q1",
            description: "Find class/struct/type named X",
            tool: "search_codebase",
            score_type: ScoreType::ExactMatch,
            weight: 0.0833,
        },
        QuerySpec {
            id: "q2",
            description: "Find interface/protocol/trait named X",
            tool: "search_codebase",
            score_type: ScoreType::ExactMatch,
            weight: 0.0833,
        },
        QuerySpec {
            id: "q3",
            description: "Find function/method named X",
            tool: "search_codebase",
            score_type: ScoreType::ExactMatch,
            weight: 0.0833,
        },
        QuerySpec {
            id: "q4",
            description: "What are all callers of function X?",
            tool: "get_context",
            score_type: ScoreType::F1Set,
            weight: 0.10,
        },
        QuerySpec {
            id: "q5",
            description: "What does X call?",
            tool: "get_context",
            score_type: ScoreType::F1Set,
            weight: 0.10,
        },
        QuerySpec {
            id: "q6",
            description: "What classes implement interface X?",
            tool: "get_context",
            score_type: ScoreType::F1Set,
            weight: 0.10,
        },
        QuerySpec {
            id: "q7",
            description: "Blast radius at depth=3 for X",
            tool: "get_impact",
            score_type: ScoreType::F1Set,
            weight: 0.15,
        },
        QuerySpec {
            id: "q8",
            description: "What symbols live in file F?",
            tool: "query_graph",
            score_type: ScoreType::F1Set,
            weight: 0.025,
        },
        QuerySpec {
            id: "q9",
            description: "What imports does F have?",
            tool: "query_graph",
            score_type: ScoreType::F1Set,
            weight: 0.10,
        },
        QuerySpec {
            id: "q10",
            description: "What module is symbol X in?",
            tool: "get_symbol",
            score_type: ScoreType::ExactMatch,
            weight: 0.025,
        },
        QuerySpec {
            id: "q11",
            description: "What field X exists on type Y?",
            tool: "query_graph",
            score_type: ScoreType::F1Set,
            weight: 0.025,
        },
        QuerySpec {
            id: "q12",
            description: "Community structure quality",
            tool: "cluster_graph",
            score_type: ScoreType::AdjustedRand,
            weight: 0.05,
        },
        QuerySpec {
            id: "q13",
            description: "PRD validation against graph",
            tool: "validate_prd_against_graph",
            score_type: ScoreType::PrecisionRecallMean,
            weight: 0.10,
        },
        QuerySpec {
            id: "q14",
            description: "Unresolved external refs in file F",
            tool: "query_graph",
            score_type: ScoreType::F1Set,
            weight: 0.025,
        },
    ]
}

/// Returns the id->weight map used by the weighted aggregator.
pub fn weights() -> HashMap<String, f64> {
    all_queries()
        .iter()
        .map(|q| (q.id.to_string(), q.weight))
        .collect()
}

/// Lookup a query spec by id.  Used by the runner when dispatching labels.
pub fn lookup(id: &str) -> Option<QuerySpec> {
    all_queries().into_iter().find(|q| q.id == id)
}
