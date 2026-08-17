use proxima_core::{
    AbstractionPayload, SearchProjection, SearchProjectionColumnKind, SearchProjectionField,
    proxima_schema_id,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Code F→A output — a per-commit synthesis covering the commit
/// Fact, its file-revision Facts, and derived code-slice/call
/// intelligence for the closed source-batch.
///
/// The Abstraction's `text` (operator-authored narrative) is the
/// search/embed body; this sidecar carries the typed structured fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CommitSummaryV1 {
    #[schemars(
        description = "Repo UUID for the summarized commit; provider-facing typed emit wrappers may replace *_id fields with handles."
    )]
    pub repo_id: uuid::Uuid,
    #[schemars(description = "Git commit SHA being summarized.")]
    pub commit_sha: String,
    /// 1–3 sentence summary of what this commit accomplished.
    #[schemars(description = "One to three sentence summary of what this commit accomplished.")]
    pub summary: String,
    /// Key files the operator highlighted as central to the change.
    #[schemars(
        description = "Key repo-relative files the operator highlighted as central to the change. Use `[]` when no key files stand out."
    )]
    pub key_files: Vec<String>,
    /// Operator-classified change type, free-form lowercase string.
    /// Common values: `"feature"`, `"fix"`, `"refactor"`, `"docs"`,
    /// `"test"`, `"chore"`. v1 does not constrain the vocabulary.
    #[schemars(
        description = "Operator-classified lowercase change type, for example `feature`, `fix`, `refactor`, `docs`, `test`, or `chore`."
    )]
    pub change_kind: String,
}

impl AbstractionPayload for CommitSummaryV1 {
    const SCHEMA_ID: &'static str = proxima_schema_id!("commit-summary-v1");
    const SCHEMA_VERSION: u32 = 1;
    fn sidecar_table() -> &'static str {
        "proxima_code.commit_summary_v1"
    }

    fn search_projection() -> Option<SearchProjection> {
        Some(SearchProjection {
            fields: &[
                SearchProjectionField {
                    column: "commit_sha",
                    kind: SearchProjectionColumnKind::Text,
                },
                SearchProjectionField {
                    column: "summary",
                    kind: SearchProjectionColumnKind::Text,
                },
                SearchProjectionField {
                    column: "key_files",
                    kind: SearchProjectionColumnKind::TextArray,
                },
            ],
            tag_column: None,
            tsv_column: Some("search_tsv"),
            language_column: None,
        })
    }

    fn json_schema() -> Option<serde_json::Value> {
        Some(
            serde_json::to_value(schemars::schema_for!(Self))
                .expect("CommitSummaryV1 schema serializes"),
        )
    }
}
