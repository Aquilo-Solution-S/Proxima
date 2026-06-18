use proxima_core::{FactPayload, proxima_schema_id};
use serde::{Deserialize, Serialize};

/// Dispatch-boundary Fact: a planner requested implementation work for
/// a repo. Durable intent/rationale belongs to `CodeExecutionPlanV1`
/// or the originating core Goal; this Fact is the observed wake request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkRequestedV1 {
    pub repo_id: uuid::Uuid,
    pub title: String,
    pub instructions: String,
    pub request_key: String,
}

impl FactPayload for WorkRequestedV1 {
    const SCHEMA_ID: &'static str = proxima_schema_id!("work-requested-v1");
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> Option<&'static str> {
        Some("proxima_code.work_requested_v1")
    }

    fn render(&self) -> String {
        format!("{}: {}", self.request_key, self.title)
    }
}

/// Backwards-compatible Rust type alias while MCP/tool names migrate.
pub type ExecutionRequestV1 = WorkRequestedV1;
