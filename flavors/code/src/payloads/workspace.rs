use proxima_core::{
    FactPayload, SearchProjection, SearchProjectionColumnKind, SearchProjectionField,
    proxima_schema_id,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, specta::Type, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(
    type_name = "proxima_code.workspace_decision",
    rename_all = "snake_case"
)]
pub enum WorkspaceDecision {
    Rejected,
    RetryRequested,
    Accepted,
    Merged,
}

impl WorkspaceDecision {
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Rejected => "rejected",
            Self::RetryRequested => "retry_requested",
            Self::Accepted => "accepted",
            Self::Merged => "merged",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, specta::Type)]
pub struct WorkspaceDecisionV1 {
    pub workspace_run_memory_id: uuid::Uuid,
    pub decision: WorkspaceDecision,
    #[serde(with = "time::serde::rfc3339")]
    pub decided_at: time::OffsetDateTime,
    pub reason_text: Option<String>,
    pub decided_by_owner_id: uuid::Uuid,
}

impl FactPayload for WorkspaceDecisionV1 {
    const SCHEMA_ID: &'static str = proxima_schema_id!("workspace-decision-v1");
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "proxima_code.workspace_decision_v1"
    }

    fn search_projection() -> Option<SearchProjection> {
        Some(SearchProjection {
            fields: &[
                SearchProjectionField {
                    column: "decision",
                    kind: SearchProjectionColumnKind::Text,
                },
                SearchProjectionField {
                    column: "reason_text",
                    kind: SearchProjectionColumnKind::Text,
                },
            ],
        })
    }

    fn render(&self) -> String {
        format!("Workspace decision: {}", self.decision.as_str())
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, specta::Type, sqlx::Type,
)]
#[schemars(description = "Workspace review verdict for a run.")]
#[serde(rename_all = "snake_case")]
#[sqlx(
    type_name = "proxima_code.workspace_review_verdict",
    rename_all = "snake_case"
)]
pub enum WorkspaceReviewVerdict {
    Approved,
    Rejected,
    NeedsUser,
}

impl WorkspaceReviewVerdict {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Approved => "approved",
            Self::Rejected => "rejected",
            Self::NeedsUser => "needs_user",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, specta::Type)]
pub struct WorkspaceReviewFinding {
    #[schemars(description = "Finding severity such as `info`, `warning`, or `error`.")]
    pub severity: String,
    #[schemars(
        description = "Optional repo-relative file path for the finding. Omit or null when not file-specific."
    )]
    pub file_path: Option<String>,
    #[schemars(
        description = "Optional 1-based line number for the finding. Omit or null when not line-specific."
    )]
    pub line: Option<u32>,
    #[schemars(description = "Concrete finding message.")]
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, specta::Type)]
pub struct WorkspaceReviewV1 {
    pub workspace_run_memory_id: uuid::Uuid,
    pub execution_request_memory_id: uuid::Uuid,
    pub verdict: WorkspaceReviewVerdict,
    pub round_index: u32,
    pub summary: String,
    pub findings: Vec<WorkspaceReviewFinding>,
    pub correction_instructions: Option<String>,
    pub verification_summary: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub reviewed_at: time::OffsetDateTime,
}

impl FactPayload for WorkspaceReviewV1 {
    const SCHEMA_ID: &'static str = proxima_schema_id!("workspace-review-v1");
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "proxima_code.workspace_review_v1"
    }

    fn search_projection() -> Option<SearchProjection> {
        Some(SearchProjection {
            fields: &[
                SearchProjectionField {
                    column: "verdict",
                    kind: SearchProjectionColumnKind::Text,
                },
                SearchProjectionField {
                    column: "summary",
                    kind: SearchProjectionColumnKind::Text,
                },
                SearchProjectionField {
                    column: "correction_instructions",
                    kind: SearchProjectionColumnKind::Text,
                },
                SearchProjectionField {
                    column: "verification_summary",
                    kind: SearchProjectionColumnKind::Text,
                },
            ],
        })
    }

    fn render(&self) -> String {
        format!(
            "Workspace review: {} round {}",
            self.verdict.as_str(),
            self.round_index
        )
    }
}
