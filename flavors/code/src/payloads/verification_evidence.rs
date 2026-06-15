use proxima_core::{
    FactPayload, SearchProjection, SearchProjectionColumnKind, SearchProjectionField,
    proxima_schema_id,
};
use schemars::JsonSchema;
use serde::de::{MapAccess, Visitor, value::MapAccessDeserializer};
use serde::{Deserialize, Deserializer, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, sqlx::Type)]
#[schemars(description = "Verification status for a criterion.")]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema, Default)]
#[schemars(description = "Structured artifact references captured during verification.")]
pub struct VerificationArtifactRefsV1 {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(
        description = "Optional single repo-relative artifact path. Omit or null when not needed."
    )]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(
        description = "Optional repo-relative artifact paths. Use `[]` when no path list is needed."
    )]
    pub paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(
        description = "Optional command argv used for verification. Use `[]` when no command was run."
    )]
    pub command: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(
        description = "Optional tool or script entrypoint used for verification. Omit or null when not applicable."
    )]
    pub entrypoint: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(
        description = "Optional allowed command prefixes for verifier policy context. Use `[]` when not applicable."
    )]
    pub allowed_prefixes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(
        description = "Optional changed file paths relevant to the verification. Use `[]` when not applicable."
    )]
    pub changed_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(
        description = "Optional process exit code from the verification command. Omit or null when no process ran."
    )]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(
        description = "Optional capped stdout tail from the verification command. Omit or null when unavailable."
    )]
    pub stdout_tail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(
        description = "Optional capped stderr tail from the verification command. Omit or null when unavailable."
    )]
    pub stderr_tail: Option<String>,
}

impl<'de> Deserialize<'de> for VerificationArtifactRefsV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(VerificationArtifactRefsVisitor)
    }
}

struct VerificationArtifactRefsVisitor;

impl<'de> Visitor<'de> for VerificationArtifactRefsVisitor {
    type Value = VerificationArtifactRefsV1;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an object-shaped verification artifact refs value")
    }

    fn visit_map<M>(self, map: M) -> Result<Self::Value, M::Error>
    where
        M: MapAccess<'de>,
    {
        let fields = VerificationArtifactRefsFields::deserialize(MapAccessDeserializer::new(map))?;
        Ok(VerificationArtifactRefsV1 {
            path: fields.path,
            paths: fields.paths,
            command: fields.command,
            entrypoint: fields.entrypoint,
            allowed_prefixes: fields.allowed_prefixes,
            changed_files: fields.changed_files,
            exit_code: fields.exit_code,
            stdout_tail: fields.stdout_tail,
            stderr_tail: fields.stderr_tail,
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VerificationArtifactRefsFields {
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    paths: Vec<String>,
    #[serde(default)]
    command: Vec<String>,
    #[serde(default)]
    entrypoint: Option<String>,
    #[serde(default)]
    allowed_prefixes: Vec<String>,
    #[serde(default)]
    changed_files: Vec<String>,
    #[serde(default)]
    exit_code: Option<i32>,
    #[serde(default)]
    stdout_tail: Option<String>,
    #[serde(default)]
    stderr_tail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VerificationEvidenceV1 {
    pub workspace_run_memory_id: uuid::Uuid,
    pub execution_request_memory_id: uuid::Uuid,
    pub criterion_key: String,
    pub status: VerificationEvidenceStatus,
    pub summary: String,
    pub artifact_refs: VerificationArtifactRefsV1,
}

impl FactPayload for VerificationEvidenceV1 {
    const SCHEMA_ID: &'static str = proxima_schema_id!("verification-evidence-v1");
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> Option<&'static str> {
        Some("proxima_code.verification_evidence_v1")
    }

    fn search_projection() -> Option<SearchProjection> {
        Some(SearchProjection {
            fields: &[
                SearchProjectionField {
                    column: "criterion_key",
                    kind: SearchProjectionColumnKind::Text,
                },
                SearchProjectionField {
                    column: "status",
                    kind: SearchProjectionColumnKind::Text,
                },
                SearchProjectionField {
                    column: "summary",
                    kind: SearchProjectionColumnKind::Text,
                },
            ],
            tag_column: None,
        })
    }

    fn render(&self) -> String {
        format!(
            "Verification evidence: {} {}",
            self.criterion_key,
            self.status.as_str()
        )
    }
}
