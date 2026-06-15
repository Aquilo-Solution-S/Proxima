use proxima_core::{
    FactPayload, SearchProjection, SearchProjectionColumnKind, SearchProjectionField,
    proxima_schema_id,
};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitV1 {
    pub repo_id: uuid::Uuid,
    pub sha: String,
    pub parents: Vec<String>,
    pub author_name: String,
    pub author_email: String,
    // RFC 3339 explicit for round-trip through Postgres `row_to_json`
    // which renders timestamptz as `2026-05-04T21:55:05+00:00` —
    // distinct from `time`'s default human-readable format.
    #[serde(with = "time::serde::rfc3339")]
    pub author_time: OffsetDateTime,
    pub committer_name: String,
    pub committer_email: String,
    #[serde(with = "time::serde::rfc3339")]
    pub committer_time: OffsetDateTime,
    pub message: String,
}

impl FactPayload for CommitV1 {
    const SCHEMA_ID: &'static str = proxima_schema_id!("commit-v1");
    const SCHEMA_VERSION: u32 = 1;
    fn sidecar_table() -> Option<&'static str> {
        Some("proxima_code.commit_v1")
    }
    fn search_projection() -> Option<SearchProjection> {
        Some(SearchProjection {
            fields: &[
                SearchProjectionField {
                    column: "sha",
                    kind: SearchProjectionColumnKind::Text,
                },
                SearchProjectionField {
                    column: "message",
                    kind: SearchProjectionColumnKind::Text,
                },
                SearchProjectionField {
                    column: "author_name",
                    kind: SearchProjectionColumnKind::Text,
                },
                SearchProjectionField {
                    column: "author_email",
                    kind: SearchProjectionColumnKind::Text,
                },
            ],
            tag_column: None,
        })
    }
    fn render(&self) -> String {
        let short = self.sha.get(..7).unwrap_or(&self.sha);
        let first_line = self.message.lines().next().unwrap_or("");
        format!("{short} {first_line}")
    }
}
