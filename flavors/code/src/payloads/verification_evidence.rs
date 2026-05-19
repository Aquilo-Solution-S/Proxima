use proxima_core::{FactPayload, proxima_schema_id};
use schemars::JsonSchema;
use serde::de::{MapAccess, Visitor, value::MapAccessDeserializer};
use serde::{Deserialize, Deserializer, Serialize};
use std::fmt;

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema, Default)]
pub struct VerificationArtifactRefsV1 {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entrypoint: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_prefixes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdout_tail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
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
