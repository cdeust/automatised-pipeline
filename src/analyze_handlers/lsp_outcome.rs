//! Preserve optional LSP failure evidence while analysis continues.
use crate::lsp_client::LspResolutionResult;
use serde_json::{json, Value};

pub(super) enum LspOutcome {
    Disabled,
    Completed(LspResolutionResult),
    Failed(String),
}

impl LspOutcome {
    pub(super) fn status(&self) -> Value {
        match self {
            Self::Disabled => json!({"requested": false, "state": "disabled"}),
            Self::Completed(_) => json!({"requested": true, "state": "completed"}),
            Self::Failed(error) => json!({
                "requested": true, "state": "failed", "error": error,
                "fallback": "available_graph",
                "note": "Analysis continued; the graph may include partial LSP results."
            }),
        }
    }

    /// Keep the existing successful counts and disabled null wire shape.
    /// `completed` means the pass returned, not that every site resolved.
    pub(super) fn counts(&self) -> Value {
        match self {
            Self::Completed(r) => json!({
                "resolved_count": r.resolved_count,
                "failed_count": r.failed_count,
                "skipped_count": r.skipped_count,
                "elapsed_ms": r.elapsed_ms,
            }),
            Self::Disabled | Self::Failed(_) => Value::Null,
        }
    }
}
