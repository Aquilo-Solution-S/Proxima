use proxima_core::{
    AbstractionPayload, SearchProjection, SearchProjectionColumnKind, SearchProjectionField,
    proxima_schema_id,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::payloads::file_revision::FileState;

/// Derived code-slice projection produced by the local-git F→A operator
/// over `file-revision-v1` Facts. It is code intelligence, not an
/// external observation: identity is scoped to the source file revision
/// plus slice index, and provenance is carried by `core/derived-from`
/// edges back to file/commit Facts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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

impl AbstractionPayload for CodeChunkV1 {
    const SCHEMA_ID: &'static str = proxima_schema_id!("code-chunk-v1");
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "proxima_code.code_chunk_v1"
    }

    /// `file_path` and `text`, in that order — the exact arguments the
    /// v0.0.7 flavor migration generates `search_tsv` from, so naming the
    /// stored column below is sound.
    ///
    /// `language` and `chunk_type` were projected here and are not any
    /// more. They are one lexeme each against a chunk body's few hundred,
    /// so they never lifted a result; they are already exposed as explicit
    /// filters on `proxima-code_search_chunks`; and every field listed here
    /// has to appear in the generated column's expression, so keeping them
    /// would widen the invariant for no retrieval gain.
    fn search_projection() -> Option<SearchProjection> {
        Some(SearchProjection {
            fields: &[
                SearchProjectionField {
                    column: "file_path",
                    kind: SearchProjectionColumnKind::Text,
                },
                SearchProjectionField {
                    column: "text",
                    kind: SearchProjectionColumnKind::Text,
                },
            ],
            tag_column: None,
            tsv_column: Some("search_tsv"),
        })
    }

    fn json_schema() -> Option<serde_json::Value> {
        Some(
            serde_json::to_value(schemars::schema_for!(Self))
                .expect("CodeChunkV1 schema serializes"),
        )
    }
}
