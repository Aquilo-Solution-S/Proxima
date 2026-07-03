use proxima_core::storage_ports::{
    EmbeddingJobPort, EmbeddingMaintenancePort, EmbeddingTextPort, EmbeddingWritePort,
    OperatorMaintenanceProof, OwnerWritePermit,
};
use proxima_core::{
    EmbeddableEntityRef, EmbeddingAnnObservability, EmbeddingJobClaim, EmbeddingOrphanSweepOutcome,
    EmbeddingWriteOutcome, MemoryId, Owner, StorageError,
};

use crate::{PgStorage, verbs};

#[async_trait::async_trait]
impl EmbeddingTextPort for PgStorage {
    async fn load_embedding_text(
        &self,
        owner: &Owner,
        entity_kind: proxima_core::EntityKind,
        memory_id: MemoryId,
    ) -> Result<Option<String>, StorageError> {
        verbs::fact_embeddings::load_embedding_text(&self.pool, owner, entity_kind, memory_id).await
    }

    async fn list_facts_missing_embedding(
        &self,
        owner: &Owner,
        model_id: &str,
        limit: usize,
    ) -> Result<Vec<MemoryId>, StorageError> {
        verbs::fact_embeddings::list_facts_missing_embedding(&self.pool, owner, model_id, limit)
            .await
    }
}

#[async_trait::async_trait]
impl EmbeddingWritePort for PgStorage {
    async fn insert_embedding(
        &self,
        owner: &Owner,
        entity: EmbeddableEntityRef,
        model_id: &str,
        dim: usize,
        vec: &[f32],
        _proof: proxima_core::storage_ports::EmbeddingWriteProof,
    ) -> Result<EmbeddingWriteOutcome, StorageError> {
        let mut tx =
            self.pool.begin().await.map_err(|err| {
                StorageError::Internal(format!("begin embedding insert tx: {err}"))
            })?;
        let outcome =
            verbs::fact_embeddings::insert_embedding(&mut tx, owner, entity, model_id, dim, vec)
                .await?;
        tx.commit().await.map_err(crate::error::map_err)?;
        Ok(outcome)
    }

    async fn upsert_fact_embedding(
        &self,
        owner: &Owner,
        memory_id: MemoryId,
        model_id: &str,
        dim: usize,
        vec: &[f32],
        proof: proxima_core::storage_ports::EmbeddingWriteProof,
    ) -> Result<(), StorageError> {
        self.insert_fact_embedding(owner, memory_id, model_id, dim, vec, proof)
            .await?;
        Ok(())
    }

    async fn upsert_memory_embedding(
        &self,
        owner: &Owner,
        entity_kind: proxima_core::EntityKind,
        memory_id: MemoryId,
        model_id: &str,
        dim: usize,
        vec: &[f32],
        proof: proxima_core::storage_ports::EmbeddingWriteProof,
    ) -> Result<(), StorageError> {
        self.insert_memory_embedding(owner, entity_kind, memory_id, model_id, dim, vec, proof)
            .await?;
        Ok(())
    }
}

#[async_trait::async_trait]
impl EmbeddingJobPort for PgStorage {
    async fn claim_pending_embedding_jobs(
        &self,
        model_id: &str,
        limit: i64,
    ) -> Result<Vec<EmbeddingJobClaim>, StorageError> {
        verbs::fact_embeddings::claim_pending_embedding_jobs(&self.pool, model_id, limit).await
    }

    async fn complete_embedding_job(&self, claim: &EmbeddingJobClaim) -> Result<(), StorageError> {
        verbs::fact_embeddings::complete_embedding_job(&self.pool, claim).await
    }

    async fn fail_embedding_job(
        &self,
        claim: &EmbeddingJobClaim,
        error: &str,
    ) -> Result<(), StorageError> {
        verbs::fact_embeddings::fail_embedding_job(&self.pool, claim, error).await
    }

    async fn enqueue_missing_embedding_jobs(
        &self,
        permit: &OwnerWritePermit,
        model_id: &str,
        limit: i64,
    ) -> Result<u64, StorageError> {
        verbs::fact_embeddings::enqueue_missing_embedding_jobs(&self.pool, permit, model_id, limit)
            .await
    }

    async fn count_pending_embedding_jobs(&self, owner: &Owner) -> Result<u64, StorageError> {
        verbs::fact_embeddings::count_pending_embedding_jobs(&self.pool, owner).await
    }
}

#[async_trait::async_trait]
impl EmbeddingMaintenancePort for PgStorage {
    async fn embedding_ann_observability(
        &self,
        _proof: OperatorMaintenanceProof,
    ) -> Result<EmbeddingAnnObservability, StorageError> {
        verbs::fact_embeddings::embedding_ann_observability(&self.pool).await
    }

    async fn sweep_orphan_embedding_rows(
        &self,
        _proof: OperatorMaintenanceProof,
    ) -> Result<EmbeddingOrphanSweepOutcome, StorageError> {
        verbs::fact_embeddings::sweep_orphan_embedding_rows(&self.pool).await
    }
}
