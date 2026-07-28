//! Host-wired cited-blob upload/read port for MCP tools and flavor
//! background workers.
//!
//! Core stays blob/storage-agnostic (docs/07): the S3-backed cited-blob
//! lane (`proxima-blob-s3`) is reachable as a Rust library, but MCP tools
//! live in core and cannot name the concrete store. This port is the
//! seam: the host inserts a [`CitedBlobService`] into the MCP tool
//! extensions when S3 is configured, and `core_upload` resolves it via
//! `ctx.extensions.get::<CitedBlobService>()`. When absent, the tool
//! fails typed with a configuration hint — exactly like the embedding
//! client's degraded mode. The serving runtime publishes the same
//! service to flavor workers, which need it for the half of artefact
//! work that outlives a tool call.
//!
//! Transfer is by presigned URL only (docs/10 §Large Artefact S3): the
//! MCP transport caps request bodies, and clients must never see
//! `bucket`/`object_key`. Every outcome struct here honours that.

use std::sync::Arc;

use time::OffsetDateTime;

use crate::OwnerRef;
use crate::authz::AuthzContext;
use crate::storage::StorageError;

/// Prepared presigned upload: `PUT` the raw bytes to `upload_url` with
/// exactly `headers` before `expires_at`, then complete with `upload_id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CitedBlobUploadPrepared {
    pub upload_id: String,
    pub upload_url: String,
    pub expires_at: OffsetDateTime,
    pub headers: Vec<CitedBlobUploadHeader>,
}

/// One header the presigned `PUT` requires verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CitedBlobUploadHeader {
    pub name: String,
    pub value: String,
}

/// Completed upload: the canonical cited object a Fact can now cite.
/// Hashes are lowercase hex; no storage coordinates are exposed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CitedBlobUploadCompleted {
    pub cited_object_id: String,
    pub schema: String,
    pub content_hash: String,
    pub sha256: String,
    pub byte_len: u64,
    pub mime: String,
    pub filename: String,
    pub idempotent_replay: bool,
}

/// Abort outcome; `aborted == false` means the upload had already
/// completed and its cited object remains.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CitedBlobUploadAborted {
    pub aborted: bool,
}

/// Presigned download for a completed cited blob.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CitedBlobReadUrl {
    pub read_url: String,
    pub expires_at: OffsetDateTime,
}

/// Cited-blob upload/read capability implemented by the blob backend
/// (`CitedBlobStore` in `proxima-blob-s3`). Each method re-authorizes
/// against `owner` with the caller's `authz`; the port never trusts a
/// client-supplied owner without that gate. That re-check is defense in
/// depth, not the caller-facing authz surface: the tool layer gates the
/// same owner authority first and surfaces denials as `forbidden`,
/// because `StorageError` has no forbidden class and a port-level denial
/// would misreport as caller-fixable input.
#[async_trait::async_trait]
pub trait CitedBlobPort: Send + Sync {
    /// Record a pending upload and mint its presigned `PUT`.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::ConstraintViolation`] for invalid input,
    /// and [`StorageError::Unavailable`] for S3 or database faults.
    async fn prepare_upload(
        &self,
        authz: &AuthzContext,
        owner: OwnerRef,
        filename: &str,
        mime: &str,
        byte_len: u64,
    ) -> Result<CitedBlobUploadPrepared, StorageError>;

    /// Verify the uploaded bytes and persist the canonical cited object.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::ConstraintViolation`] for a missing,
    /// expired, aborted, or length-mismatched upload, and
    /// [`StorageError::Unavailable`] for S3/database faults.
    async fn complete_upload(
        &self,
        authz: &AuthzContext,
        owner: OwnerRef,
        upload_id: &str,
    ) -> Result<CitedBlobUploadCompleted, StorageError>;

    /// Abort a pending upload; idempotent across replays and races.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::ConstraintViolation`] for a missing
    /// upload, and [`StorageError::Unavailable`] for S3/database faults.
    async fn abort_upload(
        &self,
        authz: &AuthzContext,
        owner: OwnerRef,
        upload_id: &str,
    ) -> Result<CitedBlobUploadAborted, StorageError>;

    /// Mint a presigned download URL for a completed cited blob.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::ConstraintViolation`] when the cited
    /// object is absent for `owner`, and [`StorageError::Unavailable`]
    /// for S3/database faults.
    async fn read_url(
        &self,
        authz: &AuthzContext,
        owner: OwnerRef,
        cited_object_id: uuid::Uuid,
    ) -> Result<CitedBlobReadUrl, StorageError>;
}

/// Shared handle MCP tools resolve from `McpToolCtx::extensions`, and
/// that the serving runtime also hands flavor workers directly.
/// A newtype (not a bare `Arc<dyn ...>`) so the `TypeId`-keyed extension
/// map has a unique, intention-revealing key.
#[derive(Clone)]
pub struct CitedBlobService(pub Arc<dyn CitedBlobPort>);

impl std::fmt::Debug for CitedBlobService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("CitedBlobService")
            .field(&"<dyn CitedBlobPort>")
            .finish()
    }
}
