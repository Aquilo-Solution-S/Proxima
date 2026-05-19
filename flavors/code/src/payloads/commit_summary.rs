use proxima_core::{AbstractionPayload, proxima_schema_id};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// First Code F→A output — a per-commit synthesis covering the
/// commit Fact, its file-revision Facts, and chunk Facts in a
/// single closed source-batch.
///
/// The Abstraction's `text` (operator-authored narrative) lives on
/// the substrate `memories.text` column; this sidecar carries the
/// typed structured fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CommitSummaryV1 {
    pub repo_id: uuid::Uuid,
    pub commit_sha: String,
    /// 1–3 sentence summary of what this commit accomplished.
    pub summary: String,
    /// Key files the operator highlighted as central to the change.
    pub key_files: Vec<String>,
    /// Operator-classified change type, free-form lowercase string.
    /// Common values: `"feature"`, `"fix"`, `"refactor"`, `"docs"`,
    /// `"test"`, `"chore"`. v1 does not constrain the vocabulary.
    pub change_kind: String,
}

impl AbstractionPayload for CommitSummaryV1 {
    const SCHEMA_ID: &'static str = proxima_schema_id!("commit-summary-v1");
    const SCHEMA_VERSION: u32 = 1;
    fn sidecar_table() -> &'static str {
        "proxima_code.commit_summary_v1"
    }

    fn json_schema() -> Option<serde_json::Value> {
        Some(
            serde_json::to_value(schemars::schema_for!(Self))
                .expect("CommitSummaryV1 schema serializes"),
        )
    }
}
