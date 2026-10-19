//! Core cited-object schemas.
//!
//! A Fact reaches a cited object through `memory.blob_id` (0..1): the
//! citation is that column plus the cited object's own schema.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::CitedObjectPayload;

pub const UPLOADED_BLOB_SCHEMA_ID: &str = "core/uploaded-blob-v1";

/// Canonical object key shared by staging and transactional publication.
#[must_use]
pub fn uploaded_blob_object_key(upload_id: uuid::Uuid) -> String {
    format!("objects/{upload_id}")
}

/// The key a holder's presigned PUT writes for one upload, before staging
/// publishes it under [`uploaded_blob_object_key`].
#[must_use]
pub fn upload_transfer_key(upload_id: uuid::Uuid) -> String {
    format!("pending/{upload_id}")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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

    fn idempotency_key(&self) -> [u8; 32] {
        self.content_hash
    }
}
