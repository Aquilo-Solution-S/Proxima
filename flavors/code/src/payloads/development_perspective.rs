use proxima_core::{PerspectivePayload, proxima_schema_id};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CodeDevelopmentPerspectiveV1 {
    pub repo_id: Option<uuid::Uuid>,
    pub summary: String,
    pub pattern: String,
    pub risk: String,
    pub recommended_posture: String,
    pub confidence: f32,
}

impl PerspectivePayload for CodeDevelopmentPerspectiveV1 {
    const SCHEMA_ID: &'static str = proxima_schema_id!("development-perspective-v1");
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "proxima_code.development_perspective_v1"
    }
}
