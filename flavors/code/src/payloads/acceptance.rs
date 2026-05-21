use proxima_core::{FactPayload, proxima_schema_id};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, specta::Type, sqlx::Type,
)]
#[schemars(description = "Verifier category that determines how verifier_spec is interpreted.")]
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
#[schemars(description = "Structured verifier parameters for a selected verifier kind.")]
pub struct AcceptanceVerifierSpecV1 {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(
        description = "Optional repo-relative file path for file_exists, diff_scope, or reviewer context. Omit or null when not path-based."
    )]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(
        description = "Optional command argv for command verification. Omit or null for non-command criteria."
    )]
    pub command: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(
        description = "Optional expected pattern for command output or file content. Omit or null when no pattern check is needed."
    )]
    pub pattern: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(
        description = "Optional verifier note or reviewer instruction. Omit or null when not needed."
    )]
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AcceptanceCriterionV1 {
    #[schemars(
        description = "Stable criterion key used by verifier evidence, for example `build` or `tests`."
    )]
    pub key: String,
    #[schemars(
        description = "Human-readable acceptance condition the worker/verifier should satisfy."
    )]
    pub description: String,
    #[schemars(description = "Whether this criterion is required for acceptance.")]
    pub required: bool,
    #[schemars(
        description = "Verifier category that determines how `verifier_spec` should be interpreted."
    )]
    pub verifier_kind: AcceptanceVerifierKind,
    #[schemars(description = "Structured verifier parameters for the selected `verifier_kind`.")]
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
