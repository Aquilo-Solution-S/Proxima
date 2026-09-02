pub use super::proof::EmbeddingWriteProof;

use crate::storage::{EmbeddingJobClaim, StorageError};
use crate::storage_ports::{OperatorMaintenanceProof, OwnerWritePermit};
use crate::{EmbeddableEntityRef, EntityKind, Owner};

#[async_trait::async_trait]
pub trait EmbeddingTextPort: Send + Sync {
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
        owner: &Owner,
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
        items: &[(Owner, EntityKind, crate::MemoryId)],
        non_embeddable_schemas: &[String],
    ) -> Result<Vec<Option<String>>, StorageError>;

    /// Facts with text but no vector under `model_id`.
    ///
    /// `non_embeddable_schemas` are excluded — they are not missing a
    /// vector, they declined one ([`crate::flavor::EmbeddingRecipe::Never`]).
    /// Without the exclusion this reports a backlog that no drain can
    /// ever clear, which is the same shape of lie as a queue that never
    /// empties. Empty slice = exclude nothing.
    async fn list_facts_missing_embedding(
        &self,
        owner: &Owner,
        model_id: &str,
        limit: usize,
        non_embeddable_schemas: &[String],
    ) -> Result<Vec<crate::MemoryId>, StorageError>;
}

#[async_trait::async_trait]
pub trait EmbeddingWritePort: Send + Sync {
    /// Write one embedding row for an entity. Public callers cannot forge
    /// `EmbeddingWriteProof`; route through engine embedding-write APIs
    /// instead.
    async fn insert_embedding(
        &self,
        owner: &Owner,
        entity: EmbeddableEntityRef,
        model_id: &str,
        dim: usize,
        vec: &[f32],
        proof: EmbeddingWriteProof,
    ) -> Result<EmbeddingWriteOutcome, StorageError>;

    /// Write one embedding *version* made of ordered chunk rows
    /// (`chunk_index` 0..n) for an over-limit entity text, advancing the
    /// head once. Search max-aggregates chunk similarity per memory, so
    /// chunking keeps the whole text semantically findable. Public
    /// callers cannot forge `EmbeddingWriteProof`; route through engine
    /// embedding-write APIs instead.
    async fn insert_embedding_chunks(
        &self,
        owner: &Owner,
        entity: EmbeddableEntityRef,
        model_id: &str,
        dim: usize,
        chunks: &[&[f32]],
        proof: EmbeddingWriteProof,
    ) -> Result<EmbeddingWriteOutcome, StorageError>;

    async fn insert_fact_embedding(
        &self,
        owner: &Owner,
        memory_id: crate::MemoryId,
        model_id: &str,
        dim: usize,
        vec: &[f32],
        proof: EmbeddingWriteProof,
    ) -> Result<EmbeddingWriteOutcome, StorageError> {
        self.insert_embedding(
            owner,
            EmbeddableEntityRef::Memory {
                kind: EntityKind::Fact,
                memory_id,
            },
            model_id,
            dim,
            vec,
            proof,
        )
        .await
    }
    async fn insert_memory_embedding(
        &self,
        owner: &Owner,
        entity_kind: EntityKind,
        memory_id: crate::MemoryId,
        model_id: &str,
        dim: usize,
        vec: &[f32],
        proof: EmbeddingWriteProof,
    ) -> Result<EmbeddingWriteOutcome, StorageError> {
        self.insert_embedding(
            owner,
            EmbeddableEntityRef::Memory {
                kind: entity_kind,
                memory_id,
            },
            model_id,
            dim,
            vec,
            proof,
        )
        .await
    }

    async fn upsert_fact_embedding(
        &self,
        owner: &Owner,
        memory_id: crate::MemoryId,
        model_id: &str,
        dim: usize,
        vec: &[f32],
        proof: EmbeddingWriteProof,
    ) -> Result<(), StorageError> {
        self.insert_fact_embedding(owner, memory_id, model_id, dim, vec, proof)
            .await?;
        Ok(())
    }
    async fn upsert_memory_embedding(
        &self,
        owner: &Owner,
        entity_kind: EntityKind,
        memory_id: crate::MemoryId,
        model_id: &str,
        dim: usize,
        vec: &[f32],
        proof: EmbeddingWriteProof,
    ) -> Result<(), StorageError> {
        self.insert_memory_embedding(owner, entity_kind, memory_id, model_id, dim, vec, proof)
            .await?;
        Ok(())
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

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct EmbeddingRecallCanary {
    pub model_id: String,
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
    async fn claim_pending_embedding_jobs(
        &self,
        model_id: &str,
        limit: i64,
    ) -> Result<Vec<EmbeddingJobClaim>, StorageError>;

    async fn complete_embedding_job(&self, claim: &EmbeddingJobClaim) -> Result<(), StorageError>;

    /// Refresh `claimed_at` for still-live, token-matching claims.
    ///
    /// Returns the number renewed. Missing rows are allowed: another step in
    /// the same drain may already have completed them. Every terminal write
    /// remains fenced by the claim token.
    async fn renew_embedding_jobs(&self, claims: &[EmbeddingJobClaim])
    -> Result<u64, StorageError>;

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
        model_id: &str,
        limit: i64,
        non_embeddable_schemas: &[String],
    ) -> Result<u64, StorageError>;

    async fn count_pending_embedding_jobs(&self, owner: &Owner) -> Result<u64, StorageError>;

    /// Count the owner's embedding jobs in a terminal state — the retryable
    /// dead-end `reconcile` requeues plus the permanent rejections it never
    /// will. Surfaced on the readiness resource so an operator sees the
    /// backlog no drain is going to clear on its own.
    async fn count_failed_embedding_jobs(&self, owner: &Owner) -> Result<u64, StorageError>;

    /// Owner-scoped pending+failed embedding job counts in one call. Both
    /// counts read the same `embedding_jobs` table and differ only in the
    /// status predicate, so `get_graph_authorized` merges them instead of two
    /// serial round trips. The default falls back to the two independent
    /// calls above (run concurrently); the Postgres storage backend overrides
    /// this with a single `count(*) FILTER (WHERE …)` query.
    async fn count_embedding_job_status(
        &self,
        owner: &Owner,
    ) -> Result<EmbeddingJobStatusCounts, StorageError> {
        let (pending, failed) = tokio::try_join!(
            self.count_pending_embedding_jobs(owner),
            self.count_failed_embedding_jobs(owner)
        )?;
        Ok(EmbeddingJobStatusCounts { pending, failed })
    }
}

/// Owner-scoped pending+failed embedding job counts, merged into a single
/// read. See [`EmbeddingJobPort::count_embedding_job_status`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EmbeddingJobStatusCounts {
    pub pending: u64,
    pub failed: u64,
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
    pub model_id: &'a str,
    pub scope: EmbeddingReconcileScope,
    /// Required at the storage boundary. `None` is a constraint error.
    /// Engine `None` becomes [`EMBEDDING_RECONCILE_DEFAULT_LIMIT`].
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
    /// embeddable memory the scope selects that lacks coverage under
    /// `options.model_id`. Idempotent; requeues `failed` jobs, leaves
    /// `pending`/`processing` untouched.
    async fn reconcile_embeddings(
        &self,
        options: EmbeddingReconcileOptions<'_>,
        policy: crate::EmbeddingRuntimePolicy,
        proof: OperatorMaintenanceProof,
    ) -> Result<EmbeddingReconcileOutcome, StorageError>;
}
