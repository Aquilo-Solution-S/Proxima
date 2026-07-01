use crate::storage::{EmbeddingJobClaim, StorageError};
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
    async fn insert_embedding(
        &self,
        owner: &Owner,
        entity: EmbeddableEntityRef,
        model_id: &str,
        dim: usize,
        vec: &[f32],
    ) -> Result<EmbeddingWriteOutcome, StorageError>;

    async fn insert_fact_embedding(
        &self,
        owner: &Owner,
        memory_id: crate::MemoryId,
        model_id: &str,
        dim: usize,
        vec: &[f32],
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
    ) -> Result<(), StorageError> {
        self.insert_fact_embedding(owner, memory_id, model_id, dim, vec)
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
    ) -> Result<(), StorageError> {
        self.insert_memory_embedding(owner, entity_kind, memory_id, model_id, dim, vec)
            .await?;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmbeddingWriteOutcome {
    pub embedding_version: i32,
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
        owner: &Owner,
        model_id: &str,
        limit: i64,
    ) -> Result<u64, StorageError>;

    async fn count_pending_embedding_jobs(&self, owner: &Owner) -> Result<u64, StorageError>;
}
