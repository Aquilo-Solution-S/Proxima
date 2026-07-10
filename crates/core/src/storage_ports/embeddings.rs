pub use super::proof::EmbeddingWriteProof;

use crate::storage::{EmbeddingJobClaim, StorageError};
use crate::storage_ports::{OperatorMaintenanceProof, OwnerWritePermit};
use crate::{EmbeddableEntityRef, EntityKind, Owner};

#[async_trait::async_trait]
pub trait EmbeddingTextPort: Send + Sync {
    async fn load_embedding_text(
        &self,
        owner: &Owner,
        entity_kind: EntityKind,
        memory_id: crate::MemoryId,
    ) -> Result<Option<String>, StorageError>;

    async fn list_facts_missing_embedding(
        &self,
        owner: &Owner,
        model_id: &str,
        limit: usize,
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

    async fn enqueue_missing_embedding_jobs(
        &self,
        permit: &OwnerWritePermit,
        model_id: &str,
        limit: i64,
    ) -> Result<u64, StorageError>;

    async fn count_pending_embedding_jobs(&self, owner: &Owner) -> Result<u64, StorageError>;

    /// Count the owner's embedding jobs in the terminal `failed` state (retries
    /// exhausted). Surfaced on the readiness resource so an operator can see the
    /// retry dead-end that `reconcile` requeues.
    async fn count_failed_embedding_jobs(&self, owner: &Owner) -> Result<u64, StorageError>;
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
}
