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
use crate::authz::DelegationRuntimeBinding;
use crate::authz::{
    AuthzContext, DelegationRuntimeAuthority, EngineAuthority, context_for_engine_operation,
};

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
pub struct CitedBlobReadService {
    port: Arc<dyn CitedBlobReadPort>,
    runtime_binding: Option<DelegationRuntimeBinding>,
}

impl std::fmt::Debug for CitedBlobReadService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("CitedBlobReadService")
            .field(&"<dyn CitedBlobReadPort>")
            .finish()
    }
}

impl CitedBlobReadService {
    #[cfg(any(test, feature = "test-fixtures"))]
    #[doc(hidden)]
    #[must_use]
    pub fn backend_identity_for_tests(&self) -> *const () {
        Arc::as_ptr(&self.port).cast::<()>()
    }

    /// Build an unbound service for ordinary authenticated calls and test
    /// fakes. Redeemed phases are rejected by an unbound service.
    #[must_use]
    pub fn new(port: Arc<dyn CitedBlobReadPort>) -> Self {
        Self {
            port,
            runtime_binding: None,
        }
    }

    /// Runtime composition seam binding this service to the same booted
    /// Engine as delegated phases.
    #[doc(hidden)]
    #[must_use]
    pub fn new_runtime(
        port: Arc<dyn CitedBlobReadPort>,
        authority: &DelegationRuntimeAuthority,
    ) -> Self {
        Self {
            port,
            runtime_binding: Some(authority.binding()),
        }
    }

    /// Collect verified bytes after checking an ordinary or redeemed phase
    /// authority at operation start.
    ///
    /// # Errors
    ///
    /// Returns `AccessDenied` for an invalid/expired authority before the
    /// backend observes owner, object id, or byte ceiling; otherwise forwards
    /// the verified-read taxonomy.
    pub async fn collect_verified<A>(
        &self,
        authority: &A,
        owner: OwnerRef,
        cited_object_id: Uuid,
        max_bytes: NonZeroU64,
    ) -> Result<VerifiedCitedBlob, CitedBlobReadError>
    where
        A: EngineAuthority + ?Sized,
    {
        let operation = context_for_engine_operation(authority)
            .map_err(|_| CitedBlobReadError::AccessDenied)?;
        operation
            .validate_runtime_binding(self.runtime_binding.as_ref())
            .map_err(|_| CitedBlobReadError::AccessDenied)?;
        self.port
            .collect_verified(operation.authz(), owner, cited_object_id, max_bytes)
            .await
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, SystemTime};

    use super::*;
    use crate::authz::{AuthPath, DelegatedPhase, DelegationRuntimeBinding};
    use crate::{GroupId, OwnerRef, Role, UserId};

    #[derive(Debug, Default)]
    struct RecordingRead {
        calls: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl CitedBlobReadPort for RecordingRead {
        async fn collect_verified(
            &self,
            _authz: &AuthzContext,
            _owner: OwnerRef,
            cited_object_id: Uuid,
            _max_bytes: NonZeroU64,
        ) -> Result<VerifiedCitedBlob, CitedBlobReadError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(VerifiedCitedBlob {
                cited_object_id,
                content_hash: [0; 32],
                sha256: [0; 32],
                byte_len: 1,
                mime: "application/octet-stream".into(),
                filename: "one.bin".into(),
                bytes: vec![0],
            })
        }
    }

    #[tokio::test]
    async fn foreign_phase_is_denied_before_verified_read_backend() {
        let backend = Arc::new(RecordingRead::default());
        let runtime_authority = DelegationRuntimeAuthority::new(DelegationRuntimeBinding::fresh());
        let service = CitedBlobReadService::new_runtime(backend.clone(), &runtime_authority);
        let subject = UserId::new(Uuid::now_v7());
        let owner = OwnerRef::Group(GroupId::new(Uuid::now_v7()));
        let expires_at = SystemTime::now() + Duration::from_secs(30);
        let raw = AuthzContext::for_subject_with_role(
            subject,
            [(owner, Role::editor())],
            AuthPath::Delegated,
        )
        .with_expires_at(Some(expires_at));
        let foreign_phase = DelegatedPhase::new(raw, expires_at, DelegationRuntimeBinding::fresh());

        let error = service
            .collect_verified(
                &foreign_phase,
                owner,
                Uuid::now_v7(),
                NonZeroU64::new(1).expect("non-zero byte ceiling"),
            )
            .await
            .expect_err("foreign phase must fail before backend access");

        assert_eq!(error, CitedBlobReadError::AccessDenied);
        assert_eq!(backend.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn matching_phase_reaches_verified_read_backend() {
        let backend = Arc::new(RecordingRead::default());
        let binding = DelegationRuntimeBinding::fresh();
        let runtime_authority = DelegationRuntimeAuthority::new(binding.clone());
        let service = CitedBlobReadService::new_runtime(backend.clone(), &runtime_authority);
        let subject = UserId::new(Uuid::now_v7());
        let owner = OwnerRef::Group(GroupId::new(Uuid::now_v7()));
        let expires_at = SystemTime::now() + Duration::from_secs(30);
        let raw = AuthzContext::for_subject_with_role(
            subject,
            [(owner, Role::viewer())],
            AuthPath::Delegated,
        )
        .with_expires_at(Some(expires_at));
        let phase = DelegatedPhase::new(raw, expires_at, binding);
        let cited_object_id = Uuid::now_v7();

        let blob = service
            .collect_verified(
                &phase,
                owner,
                cited_object_id,
                NonZeroU64::new(1).expect("non-zero byte ceiling"),
            )
            .await
            .expect("matching delegated runtime reaches backend");

        assert_eq!(blob.cited_object_id, cited_object_id);
        assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
    }
}
