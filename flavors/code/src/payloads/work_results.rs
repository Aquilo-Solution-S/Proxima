use proxima_core::{AbstractionPayload, FactPayload, proxima_schema_id};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(
    type_name = "proxima_code.work_result_status",
    rename_all = "snake_case"
)]
pub enum WorkResultStatus {
    Succeeded,
    Failed,
    Blocked,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(
    type_name = "proxima_code.acceptance_verification_status",
    rename_all = "snake_case"
)]
pub enum AcceptanceVerificationStatus {
    Passed,
    Failed,
    Skipped,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ExecutionResultV1 {
    pub work_requested_memory_id: uuid::Uuid,
    pub repo_id: uuid::Uuid,
    pub status: WorkResultStatus,
    pub summary: String,
    pub artifact_refs: Vec<String>,
    pub log_excerpt: Option<String>,
}

impl FactPayload for ExecutionResultV1 {
    const SCHEMA_ID: &'static str = proxima_schema_id!("execution-result-v1");
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> Option<&'static str> {
        Some("proxima_code.execution_result_v1")
    }

    fn json_schema() -> Option<serde_json::Value> {
        Some(
            serde_json::to_value(schemars::schema_for!(Self))
                .expect("ExecutionResultV1 schema serializes"),
        )
    }

    fn render(&self) -> String {
        format!("Execution {:?}: {}", self.status, self.summary)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TestResultV1 {
    pub test_requested_memory_id: uuid::Uuid,
    pub repo_id: uuid::Uuid,
    pub status: WorkResultStatus,
    pub summary: String,
    pub artifact_refs: Vec<String>,
    pub log_excerpt: Option<String>,
}

impl FactPayload for TestResultV1 {
    const SCHEMA_ID: &'static str = proxima_schema_id!("test-result-v1");
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> Option<&'static str> {
        Some("proxima_code.test_result_v1")
    }

    fn json_schema() -> Option<serde_json::Value> {
        Some(
            serde_json::to_value(schemars::schema_for!(Self))
                .expect("TestResultV1 schema serializes"),
        )
    }

    fn render(&self) -> String {
        format!("Test {:?}: {}", self.status, self.summary)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AcceptanceVerificationV1 {
    pub work_item_memory_id: uuid::Uuid,
    pub criterion_key: String,
    pub status: AcceptanceVerificationStatus,
    pub summary: String,
    pub artifact_refs: Vec<String>,
    pub verifier_memory_id: Option<uuid::Uuid>,
}

impl FactPayload for AcceptanceVerificationV1 {
    const SCHEMA_ID: &'static str = proxima_schema_id!("acceptance-verification-v1");
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> Option<&'static str> {
        Some("proxima_code.acceptance_verification_v1")
    }

    fn json_schema() -> Option<serde_json::Value> {
        Some(
            serde_json::to_value(schemars::schema_for!(Self))
                .expect("AcceptanceVerificationV1 schema serializes"),
        )
    }

    fn render(&self) -> String {
        format!(
            "Acceptance {} {:?}: {}",
            self.criterion_key, self.status, self.summary
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AcceptanceSummaryV1 {
    pub work_item_memory_id: uuid::Uuid,
    pub repo_id: uuid::Uuid,
    pub passed_required: bool,
    pub summary: String,
    pub verification_memory_ids: Vec<uuid::Uuid>,
}

impl AbstractionPayload for AcceptanceSummaryV1 {
    const SCHEMA_ID: &'static str = proxima_schema_id!("acceptance-summary-v1");
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "proxima_code.acceptance_summary_v1"
    }

    fn json_schema() -> Option<serde_json::Value> {
        Some(
            serde_json::to_value(schemars::schema_for!(Self))
                .expect("AcceptanceSummaryV1 schema serializes"),
        )
    }
}
