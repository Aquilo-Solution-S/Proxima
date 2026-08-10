//! Does the corpus still hold what it says it holds?
//!
//! The blob lane is content-addressed across two systems that can drift.
//! `cited_uploaded_blob_v1` names a bucket and an object key; the object
//! store holds bytes. Nothing keeps them in step, and since the held-blob
//! check (`CitedBlobPort::find_held_blobs`) an upload is SKIPPED when the
//! row says the artefact is present — so a row that outlives its object
//! makes a citation permanently unresolvable, and no path repairs it.
//!
//! WHY THIS IS NOT A CHECK ON THE HOT PATH. `find_held_blobs` is one
//! indexed query by design: it exists to replace a per-page network
//! round-trip, and reinstating one to verify its own answer would give the
//! whole saving back. The divergence is rare, so it is swept for, not
//! guarded against — which is the same trade `sweep_orphan_embedding_rows`
//! makes for crash residue.
//!
//! IT REPORTS AND DELETES NOTHING, deliberately. The dangerous direction
//! cannot be repaired here in any case — the bytes are gone, and only a
//! source that still has them (a bucket version, a backup, the original
//! upload) can restore one. The other direction is deletable in principle
//! and still is not: an object with no row may be an upload that committed
//! its bytes microseconds ago and has not yet committed its row, so a
//! sweep that deleted on sight would race every concurrent upload it
//! passed. An operator acts on this report; the report does not act.

use std::sync::Arc;

use crate::OwnerRef;
use crate::authz::{AuthPath, AuthzContext, SystemAuthority};
use crate::storage::StorageError;

/// How many examples of each divergence a reconcile carries back.
///
/// A count answers "is anything wrong"; a sample answers "what". Bounded
/// because the caller is a report and not a work queue — a deployment with
/// fifty thousand orphans does not want fifty thousand strings, it wants
/// the number and enough keys to recognise the cause.
pub const MAX_RECONCILE_SAMPLE: usize = 100;

/// An artefact the corpus claims to hold whose object is not in the store.
///
/// THE DANGEROUS DIRECTION. Every field here exists so the row can be acted
/// on without a second query: `cited_object_id` finds the citation,
/// `object_key` is what to restore from a bucket version, and `filename`
/// plus `byte_len` are what a person recognises it by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CitedBlobMissingObject {
    pub cited_object_id: uuid::Uuid,
    pub object_key: String,
    pub byte_len: u64,
    pub filename: String,
}

/// What one pass over the bucket and the table found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CitedBlobReconcileOutcome {
    /// Rows examined: this store's bucket, under the canonical prefix.
    pub rows_scanned: u64,
    /// Objects examined under the canonical prefix.
    pub objects_scanned: u64,
    /// Rows with no object. **A citation that cannot be resolved.**
    pub missing_objects: u64,
    pub missing_sample: Vec<CitedBlobMissingObject>,
    /// Objects with no row: cost and retention, not correctness. An aborted
    /// upload, a dropped database, an owner erased from Postgres alone.
    pub orphan_objects: u64,
    pub orphan_sample: Vec<String>,
    /// Rows naming another bucket, or a key outside the canonical prefix.
    ///
    /// NOT COUNTED AS MISSING, because the cause is different and so is the
    /// repair. The locator columns are client-writable — `read_url` says so
    /// in its own comment and refuses to presign such a row — so a non-zero
    /// count here is either a legacy locator or someone writing rows
    /// directly, and folding it into `missing_objects` would report a data
    /// loss that never happened.
    pub foreign_locators: u64,
    pub foreign_sample: Vec<String>,
}

impl CitedBlobReconcileOutcome {
    /// True when every row this store is responsible for has its object.
    ///
    /// Orphans do not count against it: they cost money and retention, and
    /// they resolve nothing wrongly. A caller that wants "is the corpus
    /// intact" is asking about `missing_objects`.
    #[must_use]
    pub const fn is_intact(&self) -> bool {
        self.missing_objects == 0
    }
}

/// An owner-visible artefact whose object is missing.
///
/// Unlike [`CitedBlobMissingObject`], this shape deliberately carries no
/// bucket or object key. An authorized corpus reader needs the stable cited
/// object id and human metadata to identify the broken citation; storage
/// coordinates remain operator-only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CitedBlobOwnerMissingObject {
    pub cited_object_id: uuid::Uuid,
    pub byte_len: u64,
    pub filename: String,
}

/// Owner-scoped cited-blob reconciliation report.
///
/// Samples are limited to missing cited-object ids. Orphan and foreign
/// locator examples would disclose raw object-store coordinates, so the
/// owner lane reports only their counts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CitedBlobOwnerReconcileOutcome {
    pub rows_scanned: u64,
    pub objects_scanned: u64,
    pub missing_objects: u64,
    pub missing_sample: Vec<CitedBlobOwnerMissingObject>,
    pub orphan_objects: u64,
    pub foreign_locators: u64,
}

impl CitedBlobOwnerReconcileOutcome {
    /// True when every row this owner can inspect has its object.
    #[must_use]
    pub const fn is_intact(&self) -> bool {
        self.missing_objects == 0
    }
}

/// Reconcile the object store against the rows that name it.
///
/// Owner-agnostic, like every other maintenance verb: it sweeps the whole
/// configured bucket rather than one owner's prefix, so a divergence is
/// found whether or not anyone thought to ask about that owner.
#[async_trait::async_trait]
pub trait CitedBlobReconcilePort: Send + Sync {
    /// One pass: list the canonical prefix, stream the rows, diff.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Unavailable`] when the object store cannot
    /// be listed, and whatever the database refuses with when the rows
    /// cannot be read. A partial answer is never returned: a reconcile that
    /// could not see one side of the comparison would report every row on
    /// the other side as diverged, which is worse than reporting nothing.
    async fn reconcile_all(
        &self,
        authority: &SystemAuthority,
    ) -> Result<CitedBlobReconcileOutcome, StorageError>;
}

/// Reconcile only one authorized owner's cited-blob namespace.
///
/// Implementations must authorize Fact-read access before touching either
/// Postgres or object storage, constrain both sides of the diff to `owner`,
/// and return only the redacted owner DTO above.
#[async_trait::async_trait]
pub trait CitedBlobOwnerReconcilePort: Send + Sync {
    /// One owner-scoped report-only pass.
    ///
    /// # Errors
    ///
    /// Returns a constraint violation for denied owner access and
    /// [`StorageError::Unavailable`] when either backing store cannot be read.
    async fn reconcile_owner(
        &self,
        authz: &AuthzContext,
        owner: OwnerRef,
    ) -> Result<CitedBlobOwnerReconcileOutcome, StorageError>;
}

/// Typed extension-map handle for owner-scoped blob reconciliation.
#[derive(Clone)]
pub struct CitedBlobOwnerReconcileService {
    port: Arc<dyn CitedBlobOwnerReconcilePort>,
}

impl std::fmt::Debug for CitedBlobOwnerReconcileService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("CitedBlobOwnerReconcileService")
            .field(&"<dyn CitedBlobOwnerReconcilePort>")
            .finish()
    }
}

impl CitedBlobOwnerReconcileService {
    #[must_use]
    pub fn new(port: Arc<dyn CitedBlobOwnerReconcilePort>) -> Self {
        Self { port }
    }

    #[cfg(any(test, feature = "test-fixtures"))]
    #[doc(hidden)]
    #[must_use]
    pub fn backend_identity_for_tests(&self) -> *const () {
        Arc::as_ptr(&self.port).cast::<()>()
    }

    /// Run one ordinary owner-authorized reconciliation pass.
    ///
    /// Owner reconciliation is not a delegated-capable operation. Workers
    /// must not reconstruct a raw delegated context to reach it.
    ///
    /// # Errors
    ///
    /// Rejects raw delegated contexts before forwarding to the backend;
    /// otherwise forwards the port's owner-access and storage failures.
    pub async fn reconcile_owner(
        &self,
        authz: &AuthzContext,
        owner: OwnerRef,
    ) -> Result<CitedBlobOwnerReconcileOutcome, StorageError> {
        if authz.auth_path() == AuthPath::Delegated {
            return Err(StorageError::ConstraintViolation(
                "raw delegated authorization contexts are not blob authority".into(),
            ));
        }
        self.port.reconcile_owner(authz, owner).await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::{GroupId, Role, UserId};

    #[derive(Debug, Default)]
    struct RecordingReconcile {
        calls: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl CitedBlobOwnerReconcilePort for RecordingReconcile {
        async fn reconcile_owner(
            &self,
            _authz: &AuthzContext,
            _owner: OwnerRef,
        ) -> Result<CitedBlobOwnerReconcileOutcome, StorageError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(CitedBlobOwnerReconcileOutcome::default())
        }
    }

    #[tokio::test]
    async fn raw_delegated_context_is_denied_before_owner_reconcile_port() {
        let port = Arc::new(RecordingReconcile::default());
        let service = CitedBlobOwnerReconcileService::new(port.clone());
        let subject = UserId::new(uuid::Uuid::now_v7());
        let owner = OwnerRef::Group(GroupId::new(uuid::Uuid::now_v7()));
        let raw = AuthzContext::for_subject_with_role(
            subject,
            [(owner, Role::viewer())],
            AuthPath::Delegated,
        );

        let error = service
            .reconcile_owner(&raw, owner)
            .await
            .expect_err("raw delegated context must not reach reconcile backend");

        assert!(matches!(error, StorageError::ConstraintViolation(_)));
        assert_eq!(port.calls.load(Ordering::SeqCst), 0);
    }
}
