pub use super::proof::EmbeddingWriteProof;

use crate::storage::{EmbeddingJobClaim, StorageError};
use crate::storage_ports::{OperatorMaintenanceProof, OwnerWritePermit};
use crate::{EmbeddableEntityRef, EmbeddingSpace, EntityKind, MemoryId, OwnerId, OwnerScope};

#[async_trait::async_trait]
pub trait EmbeddingTextPort: Send + Sync {
    /// Host embedding drain, admitted by the engine's sealed maintenance proof.
    async fn load_embedding_texts_for_host(
        &self,
        items: &[(OwnerId, EntityKind, crate::MemoryId)],
        non_embeddable_schemas: &[String],
        _proof: OperatorMaintenanceProof,
    ) -> Result<Vec<Option<String>>, StorageError> {
        self.load_embedding_texts(None, items, non_embeddable_schemas)
            .await
    }

    /// The text to embed for one entity, or `None` when there is nothing
    /// to embed.
    ///
    /// `non_embeddable_schemas` are excluded exactly as in
    /// [`Self::list_facts_missing_embedding`]: a row whose schema declared
    /// [`crate::flavor::EmbeddingRecipe::Never`] has no text to embed,
    /// however it is reached. The exclusion lives here, not only at the
    /// enqueue sites, because a caller holding a `MemoryId` can ask to
    /// embed one row directly and never passes through a job queue.
    /// Empty slice = exclude nothing.
    async fn load_embedding_text(
        &self,
        scope: Option<&OwnerScope>,
        owner: &OwnerId,
        entity_kind: EntityKind,
        memory_id: crate::MemoryId,
        non_embeddable_schemas: &[String],
    ) -> Result<Option<String>, StorageError>;

    /// Batch counterpart of [`Self::load_embedding_text`].
    ///
    /// Output is aligned with `items`. `None` at an index is the same
    /// “nothing to embed” as the single-row method (missing row, owner
    /// mismatch, excluded schema, or no `embed_text` column).
    async fn load_embedding_texts(
        &self,
        scope: Option<&OwnerScope>,
        items: &[(OwnerId, EntityKind, crate::MemoryId)],
        non_embeddable_schemas: &[String],
    ) -> Result<Vec<Option<String>>, StorageError>;

    /// Facts with text but no vector in `space`.
    ///
    /// `non_embeddable_schemas` are excluded — they are not missing a
    /// vector, they declined one ([`crate::flavor::EmbeddingRecipe::Never`]).
    /// Without the exclusion this reports a backlog that no drain can
    /// ever clear, which is the same shape of lie as a queue that never
    /// empties. Empty slice = exclude nothing.
    async fn list_facts_missing_embedding(
        &self,
        scope: Option<&OwnerScope>,
        owner: &OwnerId,
        space: &EmbeddingSpace,
        limit: usize,
        non_embeddable_schemas: &[String],
    ) -> Result<Vec<crate::MemoryId>, StorageError>;
}

/// One embedding version to write: the entity's vectors in text order and the
/// proof that authorizes the write.
#[derive(Debug, Clone, Copy)]
pub struct EmbeddingWrite<'a> {
    pub owner: &'a OwnerId,
    pub entity: EmbeddableEntityRef,
    pub vectors: &'a [crate::SpaceVector],
    pub proof: EmbeddingWriteProof,
}

#[async_trait::async_trait]
pub trait EmbeddingWritePort: Send + Sync {
    /// Write complete embedding versions, each in text order, in ONE
    /// transaction: either every write lands or none does. Each `vectors` must
    /// be nonempty and share one space; storage commits every chunk and head
    /// atomically. The outcomes align with `writes`.
    ///
    /// A write authorized by a queue claim also completes that claim in the
    /// same transaction, fenced on its token: a stale claim fails the whole
    /// batch with `Conflict` before anything is written, and a vector and its
    /// completed job never exist apart.
    ///
    /// Public callers cannot forge `EmbeddingWriteProof`; route through
    /// engine embedding-write APIs instead.
    async fn insert_embeddings(
        &self,
        writes: &[EmbeddingWrite<'_>],
    ) -> Result<Vec<EmbeddingWriteOutcome>, StorageError>;

    /// One write of [`Self::insert_embeddings`].
    async fn insert_embedding(
        &self,
        owner: &OwnerId,
        entity: EmbeddableEntityRef,
        vectors: &[crate::SpaceVector],
        proof: EmbeddingWriteProof,
    ) -> Result<EmbeddingWriteOutcome, StorageError> {
        let outcomes = self
            .insert_embeddings(&[EmbeddingWrite {
                owner,
                entity,
                vectors,
                proof,
            }])
            .await?;
        outcomes.into_iter().next().ok_or_else(|| {
            StorageError::Internal("a batch write of one embedding returned no outcome".to_owned())
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmbeddingWriteOutcome {
    pub embedding_version: i32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EmbeddingJobBacklog {
    pub pending: u64,
    pub processing: u64,
    pub failed: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EmbeddingOrphanCounts {
    pub embeddings: u64,
    pub heads: u64,
    pub jobs: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EmbeddingOrphanSweepOutcome {
    pub embeddings_deleted: u64,
    pub heads_deleted: u64,
    pub jobs_deleted: u64,
}

/// Recall over chunk vector rows in the sampled space.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct EmbeddingRecallCanary {
    pub model_id: String,
    /// Width of the sampled space; the canary measures that width's index.
    pub dim: usize,
    pub k: u64,
    pub exact_count: u64,
    pub ann_count: u64,
    pub overlap_count: u64,
    pub recall_at_k: f64,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct EmbeddingAnnObservability {
    pub embedding_rows: u64,
    pub embedding_head_rows: u64,
    pub embedding_job_rows: u64,
    pub embedding_table_bytes: u64,
    pub embedding_total_relation_bytes: u64,
    pub hnsw_index_bytes: u64,
    pub backlog: EmbeddingJobBacklog,
    pub stale_processing_jobs: u64,
    pub orphan_rows: EmbeddingOrphanCounts,
    pub recall_canary: Option<EmbeddingRecallCanary>,
}

#[async_trait::async_trait]
pub trait EmbeddingJobPort: Send + Sync {
    /// Claim up to `limit` pending jobs in queue order, across every
    /// space. Skip entire Owners whose routes failed, and exact Owner/space
    /// pairs whose clients are paused or waiting. Each claim carries its space.
    async fn claim_pending_embedding_jobs(
        &self,
        limit: i64,
        skip_owners: &[crate::OwnerId],
        skip_owner_spaces: &[(crate::OwnerId, EmbeddingSpace)],
    ) -> Result<Vec<EmbeddingJobClaim>, StorageError>;

    async fn complete_embedding_job(&self, claim: &EmbeddingJobClaim) -> Result<(), StorageError>;

    /// Pay up to `limit` rows of the debt head changes left for the store's
    /// vector index ([`crate::storage_ports::VectorIndex`]).
    /// A store that keeps its vectors itself clears the rows. Returns how
    /// many were paid.
    async fn drain_vector_index_debt(&self, limit: i64) -> Result<u64, StorageError>;

    /// Refresh `claimed_at` for still-live, token-matching claims.
    ///
    /// Returns the number renewed. Missing rows are allowed: another step in
    /// the same drain may already have completed them. Every terminal write
    /// remains fenced by the claim token.
    async fn renew_embedding_jobs(&self, claims: &[EmbeddingJobClaim])
    -> Result<u64, StorageError>;

    /// Return `processing` jobs claimed more than `older_than_seconds` ago
    /// to `pending`, for every model. Returns the number reclaimed.
    ///
    /// The one recovery path for a drainer that died holding a claim: the
    /// job's unique key blocks any re-enqueue, so nothing else can ever
    /// free the row. Runs at the head of every drain, not only at boot —
    /// a process restarted inside the stale window would otherwise leave
    /// its predecessor's claims `processing` until some later boot
    /// happens to land after the timeout.
    async fn reclaim_stale_embedding_jobs(
        &self,
        older_than_seconds: i64,
    ) -> Result<u64, StorageError>;

    /// Fail an attempted job for a retryable cause. The job holds `error`
    /// and waits for a reconciliation pass to requeue it.
    async fn fail_embedding_job(
        &self,
        claim: &EmbeddingJobClaim,
        error: &str,
    ) -> Result<(), StorageError>;

    /// Terminally fail a job whose input the embedding provider rejects for
    /// a cause retries cannot fix (e.g. text over the model's token limit).
    /// The job takes a distinct terminal status carrying `error`, and
    /// reconciliation must not resurrect it (the memory would just poison
    /// the queue again).
    async fn fail_embedding_job_permanently(
        &self,
        claim: &EmbeddingJobClaim,
        error: &str,
    ) -> Result<(), StorageError>;

    /// Return claimed-but-unattempted jobs to `pending`.
    ///
    /// Used when a batch embed call fails for a transient provider-side
    /// cause. Nothing was tried, so the rows stay immediately claimable;
    /// there is no attempt counter and no `next_attempt_at`.
    async fn release_embedding_jobs(
        &self,
        claims: &[EmbeddingJobClaim],
        error: &str,
    ) -> Result<(), StorageError>;

    /// Owner-scoped backfill: enqueue jobs for embeddable memories that
    /// have none.
    ///
    /// `non_embeddable_schemas` are excluded. Gating only the inline
    /// write path would be a half-measure — this is the call that would
    /// find every row that path deliberately skipped and enqueue it
    /// anyway. Empty slice = exclude nothing.
    async fn enqueue_missing_embedding_jobs(
        &self,
        permit: &OwnerWritePermit,
        space: &EmbeddingSpace,
        limit: i64,
        non_embeddable_schemas: &[String],
    ) -> Result<u64, StorageError>;

    /// The owner's embedding jobs not yet embedded (`pending`, `processing`)
    /// and in a terminal state (`failed`, `failed_permanent`) — the backlog
    /// no drain clears on its own — in one read, surfaced on the readiness
    /// resource.
    async fn count_embedding_job_status(
        &self,
        scope: Option<&OwnerScope>,
        owner: &OwnerId,
    ) -> Result<EmbeddingJobStatusCounts, StorageError>;

    /// One row per embedding job of `entities` for `owner`.
    ///
    /// Not [`EmbeddingJobStatusCounts`]: each row is one job, and a
    /// processing job carries its claim age. `entities` is the memories
    /// the caller names; an empty list reads nothing. More than
    /// [`MAX_EMBEDDING_DIAGNOSTIC_UNITS`] is refused.
    ///
    /// # Errors
    ///
    /// [`StorageError::ConstraintViolation`] when `entities` is over the
    /// bound. Storage errors from the read.
    async fn embedding_job_diagnostics(
        &self,
        scope: Option<&OwnerScope>,
        owner: &OwnerId,
        entities: &[MemoryId],
    ) -> Result<Vec<EmbeddingJobDiagnostic>, StorageError>;
}

/// Owner-scoped pending and failed embedding job counts. See
/// [`EmbeddingJobPort::count_embedding_job_status`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EmbeddingJobStatusCounts {
    pub pending: u64,
    pub failed: u64,
}

/// Most memories one [`EmbeddingJobPort::embedding_job_diagnostics`] call names.
pub const MAX_EMBEDDING_DIAGNOSTIC_UNITS: usize = 256;

/// Refuse a diagnostics call that names more memories than
/// [`MAX_EMBEDDING_DIAGNOSTIC_UNITS`].
///
/// # Errors
///
/// [`StorageError::ConstraintViolation`] when `count` is over the bound.
pub fn check_diagnostic_unit_count(count: usize) -> Result<(), StorageError> {
    if count > MAX_EMBEDDING_DIAGNOSTIC_UNITS {
        return Err(StorageError::ConstraintViolation(format!(
            "at most {MAX_EMBEDDING_DIAGNOSTIC_UNITS} memories"
        )));
    }
    Ok(())
}

/// One embedding job of one memory.
///
/// Claim age exists only while the job is [`EmbeddingJobState::Processing`]:
/// the table allows `claimed_at` in that state alone. Coverage counters are
/// [`EmbeddingSpaceCoverage`], a different read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingJobDiagnostic {
    pub job_id: uuid::Uuid,
    pub entity_id: MemoryId,
    pub model_id: String,
    pub dim: u16,
    pub state: EmbeddingJobState,
}

/// Where one embedding job sits, and the claim age when it is claimed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmbeddingJobState {
    Pending,
    /// `claim_age` is `now() - claimed_at` from the database clock at the
    /// read, saturated at zero.
    Processing {
        claim_age: time::Duration,
    },
    Failed {
        last_error: Option<String>,
    },
    FailedPermanent {
        last_error: Option<String>,
    },
}

/// Which memories a global embedding reconciliation scans.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbeddingReconcileScope {
    MissingOnly,
    IncludeStale,
    Since(time::OffsetDateTime),
}

/// Boot / Engine catch-up cap when the caller does not name a limit.
///
/// Process startup uses this. `maintain-embeddings` without `--limit`
/// is the operator full pass (`i64::MAX` at that CLI boundary only).
/// Storage refuses `limit: None`.
pub const EMBEDDING_RECONCILE_DEFAULT_LIMIT: i64 = 50_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmbeddingReconcileOptions<'a> {
    pub space: &'a EmbeddingSpace,
    /// Reconcile only these Owners' memories; `None` is every Owner. The
    /// engine names the Owners whose route names `space`.
    pub owners: Option<&'a [crate::OwnerId]>,
    pub scope: EmbeddingReconcileScope,
    /// Required at the storage boundary. `None` is a constraint error.
    /// Engine `None` becomes [`EMBEDDING_RECONCILE_DEFAULT_LIMIT`]. It bounds
    /// the memories enqueued (`scanned` in the outcome), not the memories
    /// read to find them.
    pub limit: Option<i64>,
    /// Schema ids that declined a vector
    /// ([`crate::flavor::EmbeddingRecipe::Never`]), excluded from the scan.
    ///
    /// Reconcile is the global counterpart to the owner-scoped backfill,
    /// and it heals "no job exists" — precisely the state a
    /// non-embeddable schema is supposed to stay in. Empty slice =
    /// exclude nothing.
    pub non_embeddable_schemas: &'a [String],
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EmbeddingReconcileOutcome {
    pub scanned: u64,
    pub enqueued: u64,
    pub skipped: u64,
}

/// One Owner's embedding state in one space.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EmbeddingSpaceCounts {
    /// The Owner's current memories under an embeddable schema — the
    /// memories reconcile and backfill cover. The same number in every
    /// space.
    pub embeddable: u64,
    /// Of those, the memories with a vector in this space.
    pub embedded: u64,
    /// Every vector row the Owner has in this space, superseded versions
    /// included. Zero for a store whose vectors live in a host index.
    pub vectors: u64,
    pub pending: u64,
    pub processing: u64,
    pub failed: u64,
    pub failed_permanent: u64,
}

/// What the route makes of a space an Owner has embedding state in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbeddingSpaceRole {
    /// Embedded inline and searched.
    Current,
    /// Queued for every write while the Owner moves to it; not searched.
    Next,
    /// Named by neither: left over from an earlier route, until purged.
    Unrouted,
}

/// One row of [`crate::Engine::embedding_coverage`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingSpaceCoverage {
    pub space: EmbeddingSpace,
    pub role: EmbeddingSpaceRole,
    pub counts: EmbeddingSpaceCounts,
}

/// Rows [`crate::Engine::purge_embedding_spaces`] deleted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EmbeddingPurgeOutcome {
    pub vectors: u64,
    pub heads: u64,
    pub jobs: u64,
}

impl std::ops::AddAssign for EmbeddingPurgeOutcome {
    fn add_assign(&mut self, other: Self) {
        self.vectors += other.vectors;
        self.heads += other.heads;
        self.jobs += other.jobs;
    }
}

#[async_trait::async_trait]
pub trait EmbeddingMaintenancePort: Send + Sync {
    async fn embedding_ann_observability(
        &self,
        policy: crate::EmbeddingRuntimePolicy,
        proof: OperatorMaintenanceProof,
    ) -> Result<EmbeddingAnnObservability, StorageError>;

    async fn sweep_orphan_embedding_rows(
        &self,
        proof: OperatorMaintenanceProof,
    ) -> Result<EmbeddingOrphanSweepOutcome, StorageError>;

    /// Global enqueue-only reconciliation: durable embedding jobs for every
    /// embeddable memory the scope selects that lacks coverage in
    /// `options.space`. Idempotent; requeues `failed` jobs, leaves
    /// `pending`/`processing` untouched.
    async fn reconcile_embeddings(
        &self,
        options: EmbeddingReconcileOptions<'_>,
        policy: crate::EmbeddingRuntimePolicy,
        proof: OperatorMaintenanceProof,
    ) -> Result<EmbeddingReconcileOutcome, StorageError>;

    /// Up to `limit` Owners after `after`, in a stable order: the engine
    /// pages every Owner to route it before reconciling.
    async fn embedding_owner_page(
        &self,
        after: Option<crate::OwnerId>,
        limit: i64,
        proof: OperatorMaintenanceProof,
    ) -> Result<Vec<crate::OwnerId>, StorageError>;

    /// `owner`'s state in every space it has vectors or jobs in, and in
    /// each of `spaces`, ordered by space.
    async fn embedding_coverage(
        &self,
        owner: &crate::OwnerId,
        spaces: &[EmbeddingSpace],
        non_embeddable_schemas: &[String],
        proof: OperatorMaintenanceProof,
    ) -> Result<Vec<(EmbeddingSpace, EmbeddingSpaceCounts)>, StorageError>;

    /// Delete up to `limit` rows per table of `owner`'s vectors, heads and
    /// jobs in spaces outside `keep`. A `processing` job stays: its drain
    /// completes it as stale, and deleting it would fail that drain.
    async fn purge_embedding_spaces(
        &self,
        owner: &crate::OwnerId,
        keep: &[EmbeddingSpace],
        limit: i64,
        proof: OperatorMaintenanceProof,
    ) -> Result<EmbeddingPurgeOutcome, StorageError>;
}
