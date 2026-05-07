use proxima_core::{FactPayload, FactTombstone, proxima_schema_id};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
pub enum FileState {
    Present,
    Tombstone,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
pub struct FileRevisionV1 {
    pub repo_id: uuid::Uuid,
    pub file_path: String,
    pub language: Option<String>,
    // Hex-encoded under JSON (round-trips through Postgres `bytea`
    // via `row_to_json`); raw bytes under binary formats.
    #[specta(type = String)]
    #[serde(with = "crate::payloads::content_hash_serde")]
    pub content_sha256: [u8; 32],
    pub size_bytes: u64,
    pub indexed_commit_sha: String,
    pub state: FileState,
}

impl FactPayload for FileRevisionV1 {
    const SCHEMA_ID: &'static str = proxima_schema_id!("file-revision-v1");
    const SCHEMA_VERSION: u32 = 1;
    fn sidecar_table() -> &'static str {
        "proxima_code.file_revision_v1"
    }
    fn natural_key_columns() -> &'static [&'static str] {
        &["repo_id", "file_path"]
    }
    fn tombstone() -> Option<FactTombstone> {
        Some(FactTombstone {
            column: "state",
            value: "Tombstone",
        })
    }
    fn render(&self) -> String {
        let short = self
            .indexed_commit_sha
            .get(..7)
            .unwrap_or(&self.indexed_commit_sha);
        match self.state {
            FileState::Present => format!("{} @ {short}", self.file_path),
            FileState::Tombstone => format!("(deleted) {} @ {short}", self.file_path),
        }
    }
}
