use proxima_core::{FactPayload, proxima_schema_id};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, specta::Type, sqlx::Type,
)]
#[serde(rename_all = "snake_case")]
#[sqlx(
    type_name = "proxima_code.verification_evidence_status",
    rename_all = "snake_case"
)]
pub enum VerificationEvidenceStatus {
    Passed,
    Failed,
    Skipped,
}

impl VerificationEvidenceStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VerificationEvidenceV1 {
    pub workspace_run_memory_id: uuid::Uuid,
    pub execution_request_memory_id: uuid::Uuid,
    pub criterion_key: String,
    pub status: VerificationEvidenceStatus,
    pub summary: String,
    pub artifact_refs_json: serde_json::Value,
}

impl FactPayload for VerificationEvidenceV1 {
    const SCHEMA_ID: &'static str = proxima_schema_id!("verification-evidence-v1");
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "proxima_code.verification_evidence_v1"
    }

    fn render(&self) -> String {
        format!(
            "Verification evidence: {} {}",
            self.criterion_key,
            self.status.as_str()
        )
    }
}
