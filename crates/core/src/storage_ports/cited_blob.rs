//! Host-wired cited-blob upload/read port for MCP tools and flavor
//! background workers.
//!
//! Core stays blob/storage-agnostic (docs/07): the S3-backed cited-blob
//! lane (`proxima-blob-s3`) is reachable as a Rust library, but MCP tools
//! live in core and cannot name the concrete store. This port is the
//! seam: the host inserts a [`CitedBlobService`] into the composed flavor
//! services when S3 is configured, and `core_upload` resolves it via
//! `ctx.service::<CitedBlobService>()`. When absent, the tool
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
use crate::authz::DelegationRuntimeBinding;
use crate::authz::{
    AuthzContext, DelegationRuntimeAuthority, EngineAuthority, context_for_engine_operation,
};
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

/// An artefact that is in the object store and has transfer bookkeeping, but
/// has not yet entered the corpus.
///
/// The bytes have been verified and moved to the canonical key derived
/// from their `upload_id`; the upload row records that locator and its
/// SHA-256 audit digest, while the staged payload carries the BLAKE3 content
/// address. No corpus row refers to either yet.
/// Everything after this point is one database transaction, which is why
/// staging stops here rather than persisting what it staged.
///
/// This struct carries `bucket`/`object_key` inside its `payload` — it is
/// the cited-object payload, and those coordinates are what makes an
/// artefact retrievable later. That does not weaken the rule the outcome
/// structs above keep: coordinates never reach a CLIENT. This one never
/// leaves the process.
#[derive(Debug, Clone, PartialEq)]
pub struct CitedBlobStaged {
    /// The typed description of the artefact, ready to persist as a
    /// `core/uploaded-blob-v1` cited object.
    pub payload: crate::citations::UploadedBlobPayload,
    /// Set when this upload was already completed on an earlier call: the
    /// artefact is in the corpus under this id, and staging only retried
    /// cleanup of the expendable pending key. `None` on the first completion.
    pub already_completed: Option<uuid::Uuid>,
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

/// An artefact this owner already holds, as [`CitedBlobPort::find_held_blobs`]
/// reports it.
///
/// NO STORAGE COORDINATES. `bucket`/`object_key` are deliberately absent, for
/// the same reason every other outcome struct in this file omits them: the
/// answer travels to whoever asked, and a caller that learns a locator can
/// forge a citation row pointing at it. What a caller needs in order to SKIP
/// an upload is the identity it would otherwise have received from
/// completion, and that is all this carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CitedBlobHeld {
    /// The digest that was asked about, echoed back so a caller can match
    /// this row to its request without relying on ordering.
    pub content_hash: [u8; 32],
    pub cited_object_id: uuid::Uuid,
    pub byte_len: u64,
    pub mime: String,
    pub filename: String,
}

/// Largest number of digests one [`CitedBlobPort::find_held_blobs`] call
/// may ask about.
///
/// The query binds ONE array parameter regardless of length, so Postgres'
/// parameter ceiling never binds — what this bounds is the RESPONSE, which
/// carries a filename and a mime per hit. A caller with more digests than
/// this asks more than once; the answer does not depend on how the batch was
/// cut, because each digest is resolved independently.
pub const MAX_HELD_BLOB_DIGESTS: usize = 1000;

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

    /// Verify the uploaded bytes and move them to the canonical key derived
    /// from their `upload_id`, recording only the transfer locator and
    /// digests — never corpus rows.
    ///
    /// The split exists so that persisting the artefact and recording its
    /// arrival can be one transaction. This half is the part that cannot
    /// be: it streams, hashes, and copies in the object store. It is
    /// idempotent — the canonical key is a function of the `upload_id`
    /// alone. A repeat verifies and reads that immutable canonical object —
    /// it never overwrites it — and a caller that crashes before persisting
    /// may stage again.
    ///
    /// The pending object is retired, best-effort, after the canonical locator
    /// is recorded — a provider failure there never fails a stage that already
    /// succeeded. A retry re-reads those canonical bytes if corpus persistence
    /// fails; `finish_upload` repeats pending-key cleanup after the artefact is
    /// in the corpus.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::ConstraintViolation`] for a missing,
    /// expired, aborted, or length-mismatched upload, and
    /// [`StorageError::Unavailable`] for S3/database faults.
    async fn stage_upload(
        &self,
        authz: &AuthzContext,
        owner: OwnerRef,
        upload_id: &str,
    ) -> Result<CitedBlobStaged, StorageError>;

    /// Close out a staged upload whose artefact is now in the corpus:
    /// mark the pending upload completed against `cited_object_id` and
    /// retry dropping the redundant pending object.
    ///
    /// Called after the artefact and its Fact are committed, never
    /// before — so a crash in between leaves an upload row that still
    /// says `pending` while its artefact is already recorded. That is
    /// bookkeeping for the transfer protocol, invisible in the corpus,
    /// and repaired by completing the same upload again.
    ///
    /// Idempotent: an upload already marked completed is not an error.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::ConstraintViolation`] when the upload is
    /// absent for `owner`, and [`StorageError::Unavailable`] for
    /// S3/database faults.
    async fn finish_upload(
        &self,
        authz: &AuthzContext,
        owner: OwnerRef,
        upload_id: &str,
        cited_object_id: uuid::Uuid,
    ) -> Result<(), StorageError>;

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

    /// Which of `content_hashes` this owner already holds as a
    /// `core/uploaded-blob-v1` artefact.
    ///
    /// WHAT IT IS FOR. Storage is content-addressed, so re-uploading bytes
    /// the corpus already has is already harmless — it converges on the one
    /// artefact and reports `idempotent_replay`. What it is not is free: the
    /// bytes cross the wire, get streamed and hashed, and are copied in the
    /// object store, and only then does the caller learn none of it was
    /// needed. This verb moves that discovery in front of the transfer, which
    /// is the difference between re-offering a large corpus costing its full
    /// size in bandwidth and costing one query.
    ///
    /// ONLY THE HITS COME BACK, in unspecified order. A digest that is
    /// absent from the result was not found; the caller matches rows to
    /// requests on `content_hash`, never on position.
    ///
    /// NOT FOUND AND NOT YOURS ARE THE SAME ANSWER, deliberately. Every read
    /// in this lane collapses the two so a probe cannot learn that an
    /// artefact exists under an owner it cannot read — and a batch verb is
    /// exactly where that would otherwise leak, since a caller could sweep
    /// digests and diff the hits. Gate on read authority for `owner` and let
    /// the owner-scoped predicate do the rest.
    ///
    /// REQUIRED, NOT DEFAULTED, and the distinction is load-bearing here. A
    /// defaulted method is safe when its body is DERIVED from other required
    /// methods on the trait, because an implementor that inherits it can
    /// only be slower, never wrong. Nothing on this port answers "do I hold
    /// these bytes", so any default body would have to assert one — and
    /// `Ok(vec![])` is precisely the assertion a caller acts on by uploading.
    /// A fake that models storage would inherit that silently and report
    /// every artefact as absent. Better to break every implementor at
    /// compile time and have each say what it holds.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::ConstraintViolation`] when more than
    /// [`MAX_HELD_BLOB_DIGESTS`] digests are asked about, and
    /// [`StorageError::Unavailable`] for database faults.
    async fn find_held_blobs(
        &self,
        authz: &AuthzContext,
        owner: OwnerRef,
        content_hashes: &[[u8; 32]],
    ) -> Result<Vec<CitedBlobHeld>, StorageError>;
}

/// Shared handle tools and workers resolve from the composed service set.
/// A newtype (not a bare `Arc<dyn ...>`) gives the concrete-type-keyed map a
/// unique, intention-revealing key.
#[derive(Clone)]
pub struct CitedBlobService {
    port: Arc<dyn CitedBlobPort>,
    runtime_binding: Option<DelegationRuntimeBinding>,
}

impl std::fmt::Debug for CitedBlobService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("CitedBlobService")
            .field(&"<dyn CitedBlobPort>")
            .finish()
    }
}

impl CitedBlobService {
    #[cfg(any(test, feature = "test-fixtures"))]
    #[doc(hidden)]
    #[must_use]
    pub fn backend_identity_for_tests(&self) -> *const () {
        Arc::as_ptr(&self.port).cast::<()>()
    }

    /// Build an unbound service for ordinary authenticated calls and test
    /// fakes. Redeemed phases are rejected by an unbound service.
    #[must_use]
    pub fn new(port: Arc<dyn CitedBlobPort>) -> Self {
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
        port: Arc<dyn CitedBlobPort>,
        authority: &DelegationRuntimeAuthority,
    ) -> Self {
        Self {
            port,
            runtime_binding: Some(authority.binding()),
        }
    }

    fn operation_context<'a, A>(&self, authority: &'a A) -> Result<&'a AuthzContext, StorageError>
    where
        A: EngineAuthority + ?Sized,
    {
        let operation = context_for_engine_operation(authority)
            .map_err(|error| StorageError::ConstraintViolation(error.message))?;
        operation
            .validate_runtime_binding(self.runtime_binding.as_ref())
            .map_err(|error| StorageError::ConstraintViolation(error.message))?;
        Ok(operation.authz())
    }

    /// Delegated-capable service entrypoint; checks phase expiry before the
    /// backend can create an upload row or presign a request.
    ///
    /// # Errors
    ///
    /// Rejects invalid/expired/foreign authority before forwarding backend
    /// prepare errors.
    pub async fn prepare_upload<A>(
        &self,
        authority: &A,
        owner: OwnerRef,
        filename: &str,
        mime: &str,
        byte_len: u64,
    ) -> Result<CitedBlobUploadPrepared, StorageError>
    where
        A: EngineAuthority + ?Sized,
    {
        let authz = self.operation_context(authority)?;
        self.port
            .prepare_upload(authz, owner, filename, mime, byte_len)
            .await
    }

    /// Delegated-capable verified staging entrypoint.
    ///
    /// # Errors
    ///
    /// Rejects invalid/expired/foreign authority before forwarding backend
    /// staging errors.
    pub async fn stage_upload<A>(
        &self,
        authority: &A,
        owner: OwnerRef,
        upload_id: &str,
    ) -> Result<CitedBlobStaged, StorageError>
    where
        A: EngineAuthority + ?Sized,
    {
        let authz = self.operation_context(authority)?;
        self.port.stage_upload(authz, owner, upload_id).await
    }

    /// Delegated-capable upload-finish entrypoint.
    ///
    /// # Errors
    ///
    /// Rejects invalid/expired/foreign authority before forwarding backend
    /// finish errors.
    pub async fn finish_upload<A>(
        &self,
        authority: &A,
        owner: OwnerRef,
        upload_id: &str,
        cited_object_id: uuid::Uuid,
    ) -> Result<(), StorageError>
    where
        A: EngineAuthority + ?Sized,
    {
        let authz = self.operation_context(authority)?;
        self.port
            .finish_upload(authz, owner, upload_id, cited_object_id)
            .await
    }

    /// Delegated-capable abort entrypoint.
    ///
    /// # Errors
    ///
    /// Rejects invalid/expired/foreign authority before forwarding backend
    /// abort errors.
    pub async fn abort_upload<A>(
        &self,
        authority: &A,
        owner: OwnerRef,
        upload_id: &str,
    ) -> Result<CitedBlobUploadAborted, StorageError>
    where
        A: EngineAuthority + ?Sized,
    {
        let authz = self.operation_context(authority)?;
        self.port.abort_upload(authz, owner, upload_id).await
    }

    /// Delegated-capable presigned-read entrypoint.
    ///
    /// # Errors
    ///
    /// Rejects invalid/expired/foreign authority before forwarding backend
    /// read errors.
    pub async fn read_url<A>(
        &self,
        authority: &A,
        owner: OwnerRef,
        cited_object_id: uuid::Uuid,
    ) -> Result<CitedBlobReadUrl, StorageError>
    where
        A: EngineAuthority + ?Sized,
    {
        let authz = self.operation_context(authority)?;
        self.port.read_url(authz, owner, cited_object_id).await
    }

    /// Delegated-capable held-blob query entrypoint.
    ///
    /// # Errors
    ///
    /// Rejects invalid/expired/foreign authority before forwarding backend
    /// query errors.
    pub async fn find_held_blobs<A>(
        &self,
        authority: &A,
        owner: OwnerRef,
        content_hashes: &[[u8; 32]],
    ) -> Result<Vec<CitedBlobHeld>, StorageError>
    where
        A: EngineAuthority + ?Sized,
    {
        let authz = self.operation_context(authority)?;
        self.port
            .find_held_blobs(authz, owner, content_hashes)
            .await
    }
}
