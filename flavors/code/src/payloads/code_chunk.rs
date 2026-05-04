use proxima_core::{FactPayload, proxima_schema_id};
use serde::{Deserialize, Serialize};

use crate::payloads::file_revision::FileState;

/// Code chunk Fact. The "which blob this chunk belongs to" relation is
/// carried by the substrate citation (shared `cited_object_id` with the
/// parent `file-revision-v1` Fact, keyed by blob content hash) — no
/// embedded `MemoryId` parent FK in the payload. See docs/11
/// §"Three-layer model".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeChunkV1 {
    pub repo_id: uuid::Uuid,
    pub file_path: String,
    pub chunk_index: u32,
    pub text: String,
    pub language: Option<String>,
    pub chunk_type: String,
    pub byte_range_start: u32,
    pub byte_range_end: u32,
    pub line_range_start: u32,
    pub line_range_end: u32,
    pub state: FileState,
}

impl FactPayload for CodeChunkV1 {
    const SCHEMA_ID: &'static str = proxima_schema_id!("code-chunk-v1");
    const SCHEMA_VERSION: u32 = 1;
    fn sidecar_table() -> &'static str {
        "proxima_code.code_chunk_v1"
    }
    fn natural_key_columns() -> &'static [&'static str] {
        &["repo_id", "file_path", "chunk_index"]
    }
    fn render(&self) -> String {
        format!(
            "{}:{}-{}",
            self.file_path, self.line_range_start, self.line_range_end
        )
    }
}
