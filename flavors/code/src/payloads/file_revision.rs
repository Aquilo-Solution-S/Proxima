use proxima_core::{
    FactPayload, FactTombstone, PayloadKeyBuilder, SearchProjection, SearchProjectionColumnKind,
    SearchProjectionField, proxima_schema_id,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, sqlx::Type)]
#[sqlx(type_name = "proxima_code.file_state")]
pub enum FileState {
    Present,
    Tombstone,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRevisionV1 {
    pub repo_id: uuid::Uuid,
    pub file_path: String,
    pub language: Option<String>,
    // Hex-encoded under human-readable formats; raw bytes under binary formats.
    #[serde(with = "crate::payloads::content_hash_serde")]
    pub content_sha256: [u8; 32],
    pub size_bytes: u64,
    pub indexed_commit_sha: String,
    pub state: FileState,
}

impl FactPayload for FileRevisionV1 {
    const SCHEMA_ID: &'static str = proxima_schema_id!("file-revision-v1");
    const SCHEMA_VERSION: u32 = 1;
    fn event_key(&self) -> Vec<u8> {
        let mut key = PayloadKeyBuilder::new(Self::SCHEMA_ID, Self::SCHEMA_VERSION);
        key.field_uuid("repo_id", self.repo_id);
        key.field_str("file_path", &self.file_path);
        key.field_str("indexed_commit_sha", &self.indexed_commit_sha);
        key.field_bytes("content_sha256", &self.content_sha256);
        key.field_str("state", self.state.as_str());
        key.finish()
    }
    fn sidecar_table() -> Option<&'static str> {
        Some("proxima_code.file_revision_v1")
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
    fn search_projection() -> Option<SearchProjection> {
        Some(SearchProjection {
            fields: &[
                SearchProjectionField {
                    column: "file_path",
                    kind: SearchProjectionColumnKind::Text,
                },
                SearchProjectionField {
                    column: "language",
                    kind: SearchProjectionColumnKind::Text,
                },
                SearchProjectionField {
                    column: "indexed_commit_sha",
                    kind: SearchProjectionColumnKind::Text,
                },
            ],
            tag_column: None,
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

impl FileState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Present => "Present",
            Self::Tombstone => "Tombstone",
        }
    }
}
