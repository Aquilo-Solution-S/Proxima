use proxima_core::{PerspectivePayload, proxima_schema_id};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CodeCommitSummarizerSelfV1 {
    pub display_name: String,
    pub purpose: String,
}

impl PerspectivePayload for CodeCommitSummarizerSelfV1 {
    const SCHEMA_ID: &'static str = proxima_schema_id!("commit-summarizer-self-v1");
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "proxima_code.commit_summarizer_self_v1"
    }

    fn json_schema() -> Option<serde_json::Value> {
        Some(
            serde_json::to_value(schemars::schema_for!(Self))
                .expect("CodeCommitSummarizerSelfV1 schema serializes"),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CodeEngineerSelfV1 {
    pub display_name: String,
    pub purpose: String,
}

impl PerspectivePayload for CodeEngineerSelfV1 {
    const SCHEMA_ID: &'static str = proxima_schema_id!("engineer-self-v1");
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "proxima_code.engineer_self_v1"
    }

    fn json_schema() -> Option<serde_json::Value> {
        Some(
            serde_json::to_value(schemars::schema_for!(Self))
                .expect("CodeEngineerSelfV1 schema serializes"),
        )
    }
}
