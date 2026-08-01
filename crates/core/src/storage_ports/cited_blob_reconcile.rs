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

use crate::storage::StorageError;
use crate::storage_ports::proof::OperatorMaintenanceProof;

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
    async fn reconcile_cited_blobs(
        &self,
        proof: OperatorMaintenanceProof,
    ) -> Result<CitedBlobReconcileOutcome, StorageError>;
}
