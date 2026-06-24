use proxima_core::{PerspectivePayload, proxima_schema_id};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CodeDevelopmentPerspectiveV1 {
    #[schemars(
        description = "Optional repo UUID this Perspective is about. Omit or null for cross-repo observations."
    )]
    pub repo_id: Option<uuid::Uuid>,
    #[schemars(description = "Concise operator-authored summary of the development perspective.")]
    pub summary: String,
    #[schemars(description = "Observed pattern or recurring engineering signal.")]
    pub pattern: String,
    #[schemars(description = "Risk or failure mode implied by the pattern.")]
    pub risk: String,
    #[schemars(description = "Recommended engineering posture or next action.")]
    pub recommended_posture: String,
    #[schemars(description = "Confidence from 0.0 to 1.0 in this Perspective.")]
    pub confidence: f32,
}

impl PerspectivePayload for CodeDevelopmentPerspectiveV1 {
    const SCHEMA_ID: &'static str = proxima_schema_id!("development-perspective-v1");
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "proxima_code.development_perspective_v1"
    }

    fn json_schema() -> Option<serde_json::Value> {
        Some(
            serde_json::to_value(schemars::schema_for!(Self))
                .expect("CodeDevelopmentPerspectiveV1 schema serializes"),
        )
    }
}
