use proxima_core::{FactPayload, proxima_schema_id};
use serde::{Deserialize, Serialize};

use super::AcceptanceCriterionV1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TestRequestV1 {
    pub repo_id: uuid::Uuid,
    pub title: String,
    pub instructions: String,
    pub test_key: String,
    pub criteria: Vec<AcceptanceCriterionV1>,
}

impl FactPayload for TestRequestV1 {
    const SCHEMA_ID: &'static str = proxima_schema_id!("test-request-v1");
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "proxima_code.test_request_v1"
    }

    fn render(&self) -> String {
        format!("Test request: {}", self.title)
    }
}
