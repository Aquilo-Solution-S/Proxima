//! The [`CitedBlobPort`] adapter: the same five verbs, in the core
//! taxonomy the engine speaks.
//!
//! Nothing here decides anything. Each method translates the request,
//! delegates to the inherent verb, and maps the error — so a behaviour
//! change belongs in `upload.rs` or `read.rs`, never in this file.

use proxima_core::storage_ports::{
    CitedBlobHeld, CitedBlobPort, CitedBlobReadUrl, CitedBlobStaged, CitedBlobUploadAborted,
    CitedBlobUploadHeader, CitedBlobUploadPrepared,
};
use proxima_core::{AuthzContext, OwnerRef, StorageError};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

use super::CitedBlobStore;
use super::dto::{
    CitedBlobReadUrlTs, CitedBlobUploadAbortTs, CitedBlobUploadCompleteTs, CitedBlobUploadPrepareTs,
};
use crate::error::BlobError;

/// Map a [`BlobError`] onto the core storage-error taxonomy for the
/// [`CitedBlobPort`] boundary, preserving the message text.
///
/// Config/S3/database faults are infrastructure unavailability; state
/// violations (missing/expired upload, invalid input) are caller-fixable
/// `ConstraintViolation`s, which the MCP error mapper surfaces verbatim
/// instead of redacting to "internal server error". `Denied` is NOT
/// caller-fixable model input: the storage taxonomy has no forbidden
/// class, so it lands in `ConstraintViolation` only as a defense-in-depth
/// backstop — `core_upload` gates the same owner authority before the
/// port and surfaces denials as `forbidden`, matching `core_remember`.
fn blob_error_to_storage(err: BlobError) -> StorageError {
    match err {
        BlobError::Config(message) => {
            StorageError::Unavailable(format!("S3 config error: {message}"))
        }
        BlobError::S3(message) => StorageError::Unavailable(format!("S3 error: {message}")),
        BlobError::Db(err) => StorageError::Unavailable(format!("db error: {err}")),
        BlobError::Denied(message) => {
            StorageError::ConstraintViolation(format!("access denied: {message}"))
        }
        BlobError::State(message) => StorageError::ConstraintViolation(message),
    }
}

fn parse_port_time(value: &str) -> Result<OffsetDateTime, StorageError> {
    OffsetDateTime::parse(value, &Rfc3339)
        .map_err(|err| StorageError::Internal(format!("malformed expiry timestamp: {err}")))
}

#[async_trait::async_trait]
impl CitedBlobPort for CitedBlobStore {
    async fn prepare_upload(
        &self,
        authz: &AuthzContext,
        owner: OwnerRef,
        filename: &str,
        mime: &str,
        byte_len: u64,
    ) -> Result<CitedBlobUploadPrepared, StorageError> {
        let outcome = Self::prepare_upload(
            self,
            authz,
            CitedBlobUploadPrepareTs {
                owner,
                filename: filename.to_string(),
                mime: mime.to_string(),
                byte_len,
            },
        )
        .await
        .map_err(blob_error_to_storage)?;
        Ok(CitedBlobUploadPrepared {
            upload_id: outcome.upload_id,
            upload_url: outcome.upload_url,
            expires_at: parse_port_time(&outcome.expires_at)?,
            headers: outcome
                .headers
                .into_iter()
                .map(|header| CitedBlobUploadHeader {
                    name: header.name,
                    value: header.value,
                })
                .collect(),
        })
    }

    async fn stage_upload(
        &self,
        authz: &AuthzContext,
        owner: OwnerRef,
        upload_id: &str,
    ) -> Result<CitedBlobStaged, StorageError> {
        Self::stage_upload(
            self,
            authz,
            CitedBlobUploadCompleteTs {
                owner,
                upload_id: upload_id.to_string(),
            },
        )
        .await
        .map_err(blob_error_to_storage)
    }

    async fn finish_upload(
        &self,
        authz: &AuthzContext,
        owner: OwnerRef,
        upload_id: &str,
        cited_object_id: Uuid,
    ) -> Result<(), StorageError> {
        Self::finish_upload(self, authz, owner, upload_id, cited_object_id)
            .await
            .map_err(blob_error_to_storage)
    }

    async fn abort_upload(
        &self,
        authz: &AuthzContext,
        owner: OwnerRef,
        upload_id: &str,
    ) -> Result<CitedBlobUploadAborted, StorageError> {
        let outcome = Self::abort_upload(
            self,
            authz,
            CitedBlobUploadAbortTs {
                owner,
                upload_id: upload_id.to_string(),
            },
        )
        .await
        .map_err(blob_error_to_storage)?;
        Ok(CitedBlobUploadAborted {
            aborted: outcome.aborted,
        })
    }

    async fn read_url(
        &self,
        authz: &AuthzContext,
        owner: OwnerRef,
        cited_object_id: uuid::Uuid,
    ) -> Result<CitedBlobReadUrl, StorageError> {
        let outcome = Self::read_url(
            self,
            authz,
            CitedBlobReadUrlTs {
                owner,
                cited_object_id: cited_object_id.to_string(),
            },
        )
        .await
        .map_err(blob_error_to_storage)?;
        Ok(CitedBlobReadUrl {
            read_url: outcome.read_url,
            expires_at: parse_port_time(&outcome.expires_at)?,
        })
    }

    async fn find_held_blobs(
        &self,
        authz: &AuthzContext,
        owner: OwnerRef,
        content_hashes: &[[u8; 32]],
    ) -> Result<Vec<CitedBlobHeld>, StorageError> {
        // No shape to translate: the inherent verb already answers in the
        // core taxonomy, exactly as `stage_upload` does with `CitedBlobStaged`.
        // Only the error crosses a boundary here.
        Self::find_held_blobs(self, authz, owner, content_hashes)
            .await
            .map_err(blob_error_to_storage)
    }
}
