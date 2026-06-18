//! Core cited-object schemas.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::{CitedObjectPayload, proxima_schema_id};

pub const UPLOADED_BLOB_SCHEMA_ID: &str = proxima_schema_id!("uploaded-blob-v1");

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct UploadedBlobPayload {
    pub content_hash: [u8; 32],
    pub bucket: String,
    pub object_key: String,
    pub sha256: [u8; 32],
    pub byte_len: u64,
    pub mime: String,
    pub filename: String,
    pub etag: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    #[schemars(with = "String")]
    pub uploaded_at: OffsetDateTime,
}

impl CitedObjectPayload for UploadedBlobPayload {
    const SCHEMA_ID: &'static str = UPLOADED_BLOB_SCHEMA_ID;
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "proxima_core.cited_uploaded_blob_v1"
    }

    fn idempotency_key(&self) -> [u8; 32] {
        self.content_hash
    }
}
