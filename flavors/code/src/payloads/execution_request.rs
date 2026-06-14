use proxima_core::{
    FactPayload, SearchProjection, SearchProjectionColumnKind, SearchProjectionField,
    proxima_schema_id,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionRequestV1 {
    pub repo_id: uuid::Uuid,
    pub title: String,
    pub instructions: String,
    pub request_key: String,
}

impl FactPayload for ExecutionRequestV1 {
    const SCHEMA_ID: &'static str = proxima_schema_id!("execution-request-v1");
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "proxima_code.execution_request_v1"
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
                    column: "request_key",
                    kind: SearchProjectionColumnKind::Text,
                },
            ],
        })
    }

    fn render(&self) -> String {
        format!("Execution request: {}", self.title)
    }
}
