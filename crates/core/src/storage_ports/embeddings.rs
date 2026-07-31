pub use super::proof::EmbeddingWriteProof;

use crate::storage::{EmbeddingJobClaim, StorageError};
use crate::storage_ports::{OperatorMaintenanceProof, OwnerWritePermit};
use crate::{EmbeddableEntityRef, EntityKind, Owner};

/// `last_error` prefix marking a job that failed for a permanent,
/// input-specific cause (embed input the provider will always reject).
/// Reconciliation keys off this marker to leave such jobs terminal instead
/// of requeueing them into an endless reject-retry loop.
pub const PERMANENT_EMBED_FAILURE_MARKER: &str = "permanent: ";

#[async_trait::async_trait]
pub trait EmbeddingTextPort: Send + Sync {
    async fn load_embedding_text(
        &self,
        owner: &Owner,
        entity_kind: EntityKind,
        memory_id: crate::MemoryId,
    ) -> Result<Option<String>, StorageError>;

    /// Facts with text but no vector under `model_id`.
    ///
    /// `non_embeddable_schemas` are excluded — they are not missing a
    /// vector, they declined one ([`crate::FactPayload::EMBEDDABLE`]).
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

    #[allow(clippy::too_many_arguments)] // entity-kind-generic variant of insert_embedding
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

    #[allow(clippy::too_many_arguments)] // entity-kind-generic variant of upsert_fact_embedding
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

    async fn fail_embedding_job(
        &self,
        claim: &EmbeddingJobClaim,
        error: &str,
    ) -> Result<(), StorageError>;

    /// Terminally fail a job whose input the embedding provider rejects for
    /// a cause retries cannot fix (e.g. text over the model's token limit).
    /// The job goes straight to `failed` with
    /// [`PERMANENT_EMBED_FAILURE_MARKER`]-prefixed `last_error`, and
    /// reconciliation must not resurrect it (the memory would just poison
    /// the queue again).
    async fn fail_embedding_job_permanently(
        &self,
        claim: &EmbeddingJobClaim,
        error: &str,
    ) -> Result<(), StorageError>;

    /// Return claimed-but-unattempted jobs to `pending` without burning a
    /// retry attempt. Used when a *batch* embed call fails for a transient
    /// provider-side cause (429/5xx/network): the failure says nothing about
    /// any individual job, so none of them should march toward the attempt
    /// cap. A short `next_attempt_at` delay keeps concurrent drainers from
    /// hot-looping on the same jobs.
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

    /// Count the owner's embedding jobs in the terminal `failed` state (retries
    /// exhausted). Surfaced on the readiness resource so an operator can see the
    /// retry dead-end that `reconcile` requeues.
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmbeddingReconcileOptions<'a> {
    pub model_id: &'a str,
    pub scope: EmbeddingReconcileScope,
    pub limit: Option<i64>,
    /// Fact schema ids that declined a vector
    /// ([`crate::FactPayload::EMBEDDABLE`]), excluded from the scan.
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
        proof: OperatorMaintenanceProof,
    ) -> Result<EmbeddingReconcileOutcome, StorageError>;
}
