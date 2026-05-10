use proxima_core::{FactPayload, proxima_schema_id};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, specta::Type)]
pub struct WorkspaceDiffFile {
    pub path: String,
    pub insertions: u64,
    pub deletions: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, specta::Type)]
pub struct WorkspaceDiffStat {
    pub files_changed: u64,
    pub insertions: u64,
    pub deletions: u64,
    pub files: Vec<WorkspaceDiffFile>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, specta::Type)]
pub struct WorkspaceRunV1 {
    pub wake_invocation_id: uuid::Uuid,
    pub repo_id: uuid::Uuid,
    pub target_branch: String,
    pub worktree_path: String,
    pub branch_name: String,
    pub parent_sha: String,
    pub head_sha: String,
    pub diff_stat_json: WorkspaceDiffStat,
    pub exit_code: Option<i32>,
    pub stdout_tail: Option<String>,
    pub stderr_tail: Option<String>,
    pub duration_ms: Option<u64>,
}

impl FactPayload for WorkspaceRunV1 {
    const SCHEMA_ID: &'static str = proxima_schema_id!("workspace-run-v1");
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "proxima_code.workspace_run_v1"
    }

    fn render(&self) -> String {
        let short_head = self.head_sha.get(..7).unwrap_or(&self.head_sha);
        format!("Workspace run {} at {short_head}", self.branch_name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceDecision {
    Rejected,
    Accepted,
    Merged,
}

impl WorkspaceDecision {
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Rejected => "rejected",
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

    fn render(&self) -> String {
        format!("Workspace decision: {}", self.decision.as_str())
    }
}
