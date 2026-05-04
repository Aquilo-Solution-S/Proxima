use proxima_core::{FactPayload, proxima_schema_id};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitV1 {
    pub repo_id: uuid::Uuid,
    pub sha: String,
    pub parents: Vec<String>,
    pub author_name: String,
    pub author_email: String,
    pub author_time: OffsetDateTime,
    pub committer_name: String,
    pub committer_email: String,
    pub committer_time: OffsetDateTime,
    pub message: String,
}

impl FactPayload for CommitV1 {
    const SCHEMA_ID: &'static str = proxima_schema_id!("commit-v1");
    const SCHEMA_VERSION: u32 = 1;
    fn sidecar_table() -> &'static str { "proxima_code.commit_v1" }
    fn render(&self) -> String {
        let short = self.sha.get(..7).unwrap_or(&self.sha);
        let first_line = self.message.lines().next().unwrap_or("");
        format!("{short} {first_line}")
    }
}
