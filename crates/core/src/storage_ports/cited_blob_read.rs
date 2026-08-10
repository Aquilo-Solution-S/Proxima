//! Verified, in-process cited-blob byte reads.
//!
//! This capability is separate from [`super::CitedBlobPort`]. Presigned
//! downloads deliberately hand integrity verification to an external client;
//! workers that need trusted bytes instead use [`CitedBlobReadPort`], which
//! returns nothing until the complete bounded body matches the immutable row.

use std::num::NonZeroU64;
use std::sync::Arc;

use uuid::Uuid;

use crate::OwnerRef;
use crate::authz::AuthzContext;

/// Which immutable property of a completed cited blob did not match its bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CitedBlobIntegrityMismatch {
    ByteLength,
    ContentHash,
    Sha256,
}

/// Typed failure taxonomy for a verified cited-blob read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CitedBlobReadError {
    #[error("access denied")]
    AccessDenied,
    #[error("cited blob not found")]
    NotFound,
    #[error("cited blob is {byte_len} bytes; caller ceiling is {max_bytes}")]
    TooLarge { byte_len: u64, max_bytes: u64 },
    #[error("cited blob backend unavailable: {0}")]
    Unavailable(String),
    #[error("cited blob integrity mismatch: {0:?}")]
    IntegrityMismatch(CitedBlobIntegrityMismatch),
}

/// Completed cited-blob metadata plus bytes verified against it.
///
/// No storage coordinate or presigned locator crosses this boundary. The byte
/// vector is bounded by the non-zero ceiling supplied to
/// [`CitedBlobReadPort::collect_verified`].
#[derive(Debug, PartialEq, Eq)]
pub struct VerifiedCitedBlob {
    pub cited_object_id: Uuid,
    pub content_hash: [u8; 32],
    pub sha256: [u8; 32],
    pub byte_len: u64,
    pub mime: String,
    pub filename: String,
    pub bytes: Vec<u8>,
}

/// Owner-authorized, bounded, integrity-checked cited-blob byte reads.
#[async_trait::async_trait]
pub trait CitedBlobReadPort: Send + Sync {
    /// Collect one completed blob only after verifying its length, BLAKE3
    /// content hash, and SHA-256 digest.
    ///
    /// Implementations MUST authorize Fact-read access before reading a
    /// storage locator or making a network call. `max_bytes` is required and
    /// non-zero; an implementation MUST stop once the stream exceeds it and
    /// MUST NOT return partial bytes on any error.
    ///
    /// # Errors
    ///
    /// Returns [`CitedBlobReadError::AccessDenied`] before backend access,
    /// [`CitedBlobReadError::NotFound`] for an absent or non-canonical owner
    /// row, [`CitedBlobReadError::TooLarge`] before streaming when stored
    /// metadata exceeds `max_bytes`, [`CitedBlobReadError::Unavailable`] for
    /// backend faults, and [`CitedBlobReadError::IntegrityMismatch`] when the
    /// complete bytes disagree with immutable metadata.
    async fn collect_verified(
        &self,
        authz: &AuthzContext,
        owner: OwnerRef,
        cited_object_id: Uuid,
        max_bytes: NonZeroU64,
    ) -> Result<VerifiedCitedBlob, CitedBlobReadError>;
}

/// Shared verified-read handle published through the composed flavor services.
#[derive(Clone)]
pub struct CitedBlobReadService(pub Arc<dyn CitedBlobReadPort>);

impl std::fmt::Debug for CitedBlobReadService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("CitedBlobReadService")
            .field(&"<dyn CitedBlobReadPort>")
            .finish()
    }
}
