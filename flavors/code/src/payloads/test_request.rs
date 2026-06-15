use proxima_core::{
    FactPayload, SearchProjection, SearchProjectionColumnKind, SearchProjectionField,
    proxima_schema_id,
};
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

    fn sidecar_table() -> Option<&'static str> {
        Some("proxima_code.test_request_v1")
    }

    fn search_projection() -> Option<SearchProjection> {
        Some(SearchProjection {
            fields: &[
                SearchProjectionField {
                    column: "title",
                    kind: SearchProjectionColumnKind::Text,
                },
                SearchProjectionField {
                    column: "instructions",
                    kind: SearchProjectionColumnKind::Text,
                },
                SearchProjectionField {
                    column: "test_key",
                    kind: SearchProjectionColumnKind::Text,
                },
            ],
            tag_column: None,
        })
    }

    fn render(&self) -> String {
        format!("Test request: {}", self.title)
    }
}
