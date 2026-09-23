use proxima_core::storage_ports::{
    EmbeddingJobPort, EmbeddingJobStatusCounts, EmbeddingMaintenancePort, EmbeddingTextPort,
    EmbeddingWritePort, OperatorMaintenanceProof, OwnerWritePermit,
};
use proxima_core::{
    EmbeddableEntityRef, EmbeddingAnnObservability, EmbeddingJobClaim, EmbeddingOrphanSweepOutcome,
    EmbeddingSpace, EmbeddingWriteOutcome, MemoryId, Owner, StorageError,
};

use crate::{PgStorage, verbs};

#[async_trait::async_trait]
impl EmbeddingTextPort for PgStorage {
    async fn load_embedding_texts_for_host(
        &self,
        items: &[(Owner, proxima_core::EntityKind, MemoryId)],
        non_embeddable_schemas: &[String],
        _proof: OperatorMaintenanceProof,
    ) -> Result<Vec<Option<String>>, StorageError> {
        let mut tx = self.platform_transaction().await?;
        let result = verbs::fact_embeddings::load_embedding_texts_on_connection(
            tx.as_mut(),
            items,
            non_embeddable_schemas,
            &self.embed_units,
        )
        .await;
        crate::owner_scope::finish_transaction(tx, result).await
    }

    async fn load_embedding_text(
        &self,
        owner_scope: Option<&proxima_core::OwnerScope>,
        owner: &Owner,
        entity_kind: proxima_core::EntityKind,
        memory_id: MemoryId,
        non_embeddable_schemas: &[String],
    ) -> Result<Option<String>, StorageError> {
        let mut tx =
            crate::owner_scope::begin_compatible_owner_transaction(&self.pool, owner_scope).await?;
        let result = verbs::fact_embeddings::load_embedding_text_on_connection(
            tx.as_mut(),
            owner,
            entity_kind,
            memory_id,
            non_embeddable_schemas,
            &self.embed_units,
        )
        .await?;
        tx.commit().await.map_err(crate::error::map_err)?;
        Ok(result)
    }

    async fn load_embedding_texts(
        &self,
        owner_scope: Option<&proxima_core::OwnerScope>,
        items: &[(proxima_core::Owner, proxima_core::EntityKind, MemoryId)],
        non_embeddable_schemas: &[String],
    ) -> Result<Vec<Option<String>>, StorageError> {
        let mut tx =
            crate::owner_scope::begin_compatible_owner_transaction(&self.pool, owner_scope).await?;
        let result = verbs::fact_embeddings::load_embedding_texts_on_connection(
            tx.as_mut(),
            items,
            non_embeddable_schemas,
            &self.embed_units,
        )
        .await?;
        tx.commit().await.map_err(crate::error::map_err)?;
        Ok(result)
    }

    async fn list_facts_missing_embedding(
        &self,
        owner_scope: Option<&proxima_core::OwnerScope>,
        owner: &Owner,
        space: &EmbeddingSpace,
        limit: usize,
        non_embeddable_schemas: &[String],
    ) -> Result<Vec<MemoryId>, StorageError> {
        let mut tx = crate::begin_compatible_owner_transaction(&self.pool, owner_scope).await?;
        let result = verbs::fact_embeddings::list_facts_missing_embedding(
            tx.as_mut(),
            owner,
            space,
            limit,
            non_embeddable_schemas,
        )
        .await;
        crate::owner_scope::finish_transaction(tx, result).await
    }
}

#[async_trait::async_trait]
impl EmbeddingWritePort for PgStorage {
    async fn insert_embedding(
        &self,
        owner: &Owner,
        entity: EmbeddableEntityRef,
        space: &EmbeddingSpace,
        vec: &[f32],
        proof: proxima_core::storage_ports::EmbeddingWriteProof,
    ) -> Result<EmbeddingWriteOutcome, StorageError> {
        let mut tx = self.platform_transaction().await?;
        verbs::fact_embeddings::lock_embedding_job_claim(&mut tx, owner, entity, space, proof)
            .await?;
        let outcome =
            verbs::fact_embeddings::insert_embedding(&mut tx, owner, entity, space, vec).await?;
        tx.commit().await.map_err(crate::error::map_err)?;
        Ok(outcome)
    }

    async fn insert_embedding_chunks(
        &self,
        owner: &Owner,
        entity: EmbeddableEntityRef,
        space: &EmbeddingSpace,
        chunks: &[&[f32]],
        proof: proxima_core::storage_ports::EmbeddingWriteProof,
    ) -> Result<EmbeddingWriteOutcome, StorageError> {
        let mut tx = self.platform_transaction().await?;
        verbs::fact_embeddings::lock_embedding_job_claim(&mut tx, owner, entity, space, proof)
            .await?;
        let outcome =
            verbs::fact_embeddings::insert_embedding_chunks(&mut tx, owner, entity, space, chunks)
                .await?;
        tx.commit().await.map_err(crate::error::map_err)?;
        Ok(outcome)
    }
}

#[async_trait::async_trait]
impl EmbeddingJobPort for PgStorage {
    async fn claim_pending_embedding_jobs(
        &self,
        space: &EmbeddingSpace,
        limit: i64,
    ) -> Result<Vec<EmbeddingJobClaim>, StorageError> {
        let mut tx = self.platform_transaction().await?;
        let rows =
            verbs::fact_embeddings::claim_pending_embedding_jobs(tx.as_mut(), space, limit).await?;
        tx.commit().await.map_err(crate::error::map_err)?;
        Ok(rows)
    }

    async fn complete_embedding_job(&self, claim: &EmbeddingJobClaim) -> Result<(), StorageError> {
        let mut tx = self.platform_transaction().await?;
        verbs::fact_embeddings::complete_embedding_job(tx.as_mut(), claim).await?;
        tx.commit().await.map_err(crate::error::map_err)
    }

    async fn renew_embedding_jobs(
        &self,
        claims: &[EmbeddingJobClaim],
    ) -> Result<u64, StorageError> {
        let mut tx = self.platform_transaction().await?;
        let count = verbs::fact_embeddings::renew_embedding_jobs(tx.as_mut(), claims).await?;
        tx.commit().await.map_err(crate::error::map_err)?;
        Ok(count)
    }

    async fn reclaim_stale_embedding_jobs(
        &self,
        older_than_seconds: i64,
    ) -> Result<u64, StorageError> {
        let mut tx = self.platform_transaction().await?;
        let count =
            verbs::fact_embeddings::reclaim_stale_embedding_jobs(tx.as_mut(), older_than_seconds)
                .await?;
        tx.commit().await.map_err(crate::error::map_err)?;
        Ok(count)
    }

    async fn fail_embedding_job(
        &self,
        claim: &EmbeddingJobClaim,
        error: &str,
    ) -> Result<(), StorageError> {
        let mut tx = self.platform_transaction().await?;
        verbs::fact_embeddings::fail_embedding_job(tx.as_mut(), claim, error).await?;
        tx.commit().await.map_err(crate::error::map_err)
    }

    async fn fail_embedding_job_permanently(
        &self,
        claim: &EmbeddingJobClaim,
        error: &str,
    ) -> Result<(), StorageError> {
        let mut tx = self.platform_transaction().await?;
        verbs::fact_embeddings::fail_embedding_job_permanently(tx.as_mut(), claim, error).await?;
        tx.commit().await.map_err(crate::error::map_err)
    }

    async fn release_embedding_jobs(
        &self,
        claims: &[EmbeddingJobClaim],
        error: &str,
    ) -> Result<(), StorageError> {
        let mut tx = self.platform_transaction().await?;
        verbs::fact_embeddings::release_embedding_jobs_on_connection(tx.as_mut(), claims, error)
            .await?;
        tx.commit().await.map_err(crate::error::map_err)
    }

    async fn enqueue_missing_embedding_jobs(
        &self,
        permit: &OwnerWritePermit,
        space: &EmbeddingSpace,
        limit: i64,
        non_embeddable_schemas: &[String],
    ) -> Result<u64, StorageError> {
        verbs::fact_embeddings::enqueue_missing_embedding_jobs(
            &self.pool,
            permit,
            space,
            limit,
            non_embeddable_schemas,
        )
        .await
    }

    async fn count_pending_embedding_jobs(
        &self,
        owner_scope: Option<&proxima_core::OwnerScope>,
        owner: &Owner,
    ) -> Result<u64, StorageError> {
        let mut tx = crate::begin_compatible_owner_transaction(&self.pool, owner_scope).await?;
        let result =
            verbs::fact_embeddings::count_pending_embedding_jobs(tx.as_mut(), owner).await?;
        tx.commit().await.map_err(crate::error::map_err)?;
        Ok(result)
    }

    async fn count_failed_embedding_jobs(
        &self,
        owner_scope: Option<&proxima_core::OwnerScope>,
        owner: &Owner,
    ) -> Result<u64, StorageError> {
        let mut tx = crate::begin_compatible_owner_transaction(&self.pool, owner_scope).await?;
        let result =
            verbs::fact_embeddings::count_failed_embedding_jobs(tx.as_mut(), owner).await?;
        tx.commit().await.map_err(crate::error::map_err)?;
        Ok(result)
    }

    async fn count_embedding_job_status(
        &self,
        owner_scope: Option<&proxima_core::OwnerScope>,
        owner: &Owner,
    ) -> Result<EmbeddingJobStatusCounts, StorageError> {
        let mut tx = crate::begin_compatible_owner_transaction(&self.pool, owner_scope).await?;
        let result = verbs::fact_embeddings::count_embedding_job_status(tx.as_mut(), owner).await;
        crate::owner_scope::finish_transaction(tx, result).await
    }
}

#[async_trait::async_trait]
impl EmbeddingMaintenancePort for PgStorage {
    async fn embedding_ann_observability(
        &self,
        policy: proxima_core::EmbeddingRuntimePolicy,
        _proof: OperatorMaintenanceProof,
    ) -> Result<EmbeddingAnnObservability, StorageError> {
        verbs::fact_embeddings::embedding_ann_observability(
            &self.pool,
            self.platform_scope.as_ref(),
            policy.stale_claim_timeout_seconds(),
        )
        .await
    }

    async fn sweep_orphan_embedding_rows(
        &self,
        _proof: OperatorMaintenanceProof,
    ) -> Result<EmbeddingOrphanSweepOutcome, StorageError> {
        verbs::fact_embeddings::sweep_orphan_embedding_rows(
            &self.pool,
            self.platform_scope.as_ref(),
        )
        .await
    }

    async fn reconcile_embeddings(
        &self,
        options: proxima_core::EmbeddingReconcileOptions<'_>,
        policy: proxima_core::EmbeddingRuntimePolicy,
        _proof: OperatorMaintenanceProof,
    ) -> Result<proxima_core::EmbeddingReconcileOutcome, StorageError> {
        verbs::fact_embeddings::reconcile_embeddings_with_platform(
            &self.pool,
            self.platform_scope.as_ref(),
            options,
            policy.stale_claim_timeout_seconds(),
        )
        .await
    }
}
