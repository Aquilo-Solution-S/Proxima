use proxima_core::{FactPayload, proxima_schema_id};
use serde::{Deserialize, Serialize};

use super::AcceptanceCriterionV1;

/// Dispatch-boundary Fact: a planner requested verification work.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TestRequestedV1 {
    pub repo_id: uuid::Uuid,
    pub title: String,
    pub instructions: String,
    pub test_key: String,
    pub criteria: Vec<AcceptanceCriterionV1>,
}

impl FactPayload for TestRequestedV1 {
    const SCHEMA_ID: &'static str = proxima_schema_id!("test-requested-v1");
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> Option<&'static str> {
        Some("proxima_code.test_requested_v1")
    }

    fn render(&self) -> String {
        format!("{}: {}", self.test_key, self.title)
    }
}

/// Backwards-compatible Rust type alias while MCP/tool names migrate.
pub type TestRequestV1 = TestRequestedV1;
