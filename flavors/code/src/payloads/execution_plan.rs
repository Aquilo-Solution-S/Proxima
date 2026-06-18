use proxima_core::{AbstractionPayload, proxima_schema_id};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CodeExecutionPlanItemKind {
    Work,
    Test,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CodeExecutionPlanItemV1 {
    pub key: String,
    pub kind: CodeExecutionPlanItemKind,
    pub title: String,
    pub depends_on: Vec<String>,
    pub request_key: String,
}

/// Goal-native Code work plan. The durable desired future remains the
/// core Goal; this Abstraction records the code-flavor planning
/// synthesis derived from a goal activation Fact plus evidence Facts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CodeExecutionPlanV1 {
    pub repo_id: uuid::Uuid,
    pub plan_key: String,
    pub goal_activated_memory_id: uuid::Uuid,
    pub summary: String,
    pub items: Vec<CodeExecutionPlanItemV1>,
    pub evidence_memory_ids: Vec<uuid::Uuid>,
}

impl AbstractionPayload for CodeExecutionPlanV1 {
    const SCHEMA_ID: &'static str = proxima_schema_id!("execution-plan-v1");
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "proxima_code.execution_plan_v1"
    }

    fn json_schema() -> Option<serde_json::Value> {
        Some(
            serde_json::to_value(schemars::schema_for!(Self))
                .expect("CodeExecutionPlanV1 schema serializes"),
        )
    }
}
