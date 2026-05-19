use proxima_core::{FactPayload, proxima_schema_id};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, specta::Type, sqlx::Type,
)]
#[serde(rename_all = "snake_case")]
#[sqlx(
    type_name = "proxima_code.acceptance_verifier_kind",
    rename_all = "snake_case"
)]
pub enum AcceptanceVerifierKind {
    FileExists,
    Command,
    BrowserSmoke,
    DiffScope,
    ReviewerOnly,
}

impl AcceptanceVerifierKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FileExists => "file_exists",
            Self::Command => "command",
            Self::BrowserSmoke => "browser_smoke",
            Self::DiffScope => "diff_scope",
            Self::ReviewerOnly => "reviewer_only",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AcceptanceVerifierSpecV1 {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AcceptanceCriterionV1 {
    pub key: String,
    pub description: String,
    pub required: bool,
    pub verifier_kind: AcceptanceVerifierKind,
    pub verifier_spec: AcceptanceVerifierSpecV1,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AcceptanceCriteriaV1 {
    pub execution_request_memory_id: uuid::Uuid,
    pub criteria: Vec<AcceptanceCriterionV1>,
}

impl FactPayload for AcceptanceCriteriaV1 {
    const SCHEMA_ID: &'static str = proxima_schema_id!("acceptance-criteria-v1");
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "proxima_code.acceptance_criteria_v1"
    }

    fn render(&self) -> String {
        format!("Acceptance criteria: {} criteria", self.criteria.len())
    }
}
