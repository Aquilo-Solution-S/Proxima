//! Core cited-object schemas.
//!
//! A Fact reaches a cited object through `memory.blob_id` (0..1): the
//! citation is that column plus the cited object's own schema.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::{
    CitedObjectPayload, DurablePayloadBuilder, KeyField, PayloadDisplay, PayloadProjection,
};

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

impl PayloadProjection for UploadedBlobPayload {
    const KEY_FIELDS: &'static [KeyField<Self>] = &[];
    fn display(&self) -> PayloadDisplay {
        PayloadDisplay::default()
    }
    fn durable(&self) -> Result<crate::DurablePayload, String> {
        let mut fields = DurablePayloadBuilder::new();
        fields
            .field("content_hash", &self.content_hash)?
            .field("bucket", &self.bucket)?
            .field("object_key", &self.object_key)?
            .field("sha256", &self.sha256)?
            .field("byte_len", &self.byte_len)?
            .field("mime", &self.mime)?
            .field("filename", &self.filename)?
            .field("etag", &self.etag)?
            .field(
                "uploaded_at",
                &self
                    .uploaded_at
                    .format(&time::format_description::well_known::Rfc3339)
                    .map_err(|error| error.to_string())?,
            )?;
        Ok(fields.finish())
    }
}

#[cfg(test)]
mod projection_tests {
    use super::*;
    #[test]
    fn durable_matches_current_wire_encoding() {
        let value = UploadedBlobPayload {
            content_hash: [0; 32],
            bucket: "bucket".into(),
            object_key: "objects/0".into(),
            sha256: [1; 32],
            byte_len: 3,
            mime: "text/plain".into(),
            filename: "file.txt".into(),
            etag: None,
            uploaded_at: OffsetDateTime::UNIX_EPOCH,
        };
        assert_eq!(
            value.durable().expect("durable").canonical_bytes(),
            crate::canonical_json_bytes(&serde_json::to_value(&value).expect("wire"))
        );
    }
}
