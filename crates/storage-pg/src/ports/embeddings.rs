use proxima_core::storage_ports::{
    EmbeddingJobPort, EmbeddingJobStatusCounts, EmbeddingMaintenancePort, EmbeddingTextPort,
    EmbeddingWritePort, OperatorMaintenanceProof, OwnerWritePermit,
};
use proxima_core::{
    EmbeddableEntityRef, EmbeddingAnnObservability, EmbeddingJobClaim, EmbeddingOrphanSweepOutcome,
    EmbeddingSpace, EmbeddingWriteOutcome, MemoryId, OwnerId, StorageError,
};

use crate::{PgStorage, verbs};

#[async_trait::async_trait]
impl EmbeddingTextPort for PgStorage {
    async fn load_embedding_texts_for_host(
        &self,
        items: &[(OwnerId, proxima_core::EntityKind, MemoryId)],
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
        owner: &OwnerId,
        entity_kind: proxima_core::EntityKind,
        memory_id: MemoryId,
        non_embeddable_schemas: &[String],
    ) -> Result<Option<String>, StorageError> {
        let mut tx = crate::owner_scope::begin_owner_transaction(
            &self.pool,
            crate::owner_scope::require_owner_scope(owner_scope)?,
        )
        .await?;
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
        items: &[(proxima_core::OwnerId, proxima_core::EntityKind, MemoryId)],
        non_embeddable_schemas: &[String],
    ) -> Result<Vec<Option<String>>, StorageError> {
        let mut tx = crate::owner_scope::begin_owner_transaction(
            &self.pool,
            crate::owner_scope::require_owner_scope(owner_scope)?,
        )
        .await?;
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
        owner: &OwnerId,
        space: &EmbeddingSpace,
        limit: usize,
        non_embeddable_schemas: &[String],
    ) -> Result<Vec<MemoryId>, StorageError> {
        let mut tx = crate::begin_owner_transaction(
            &self.pool,
            crate::owner_scope::require_owner_scope(owner_scope)?,
        )
        .await?;
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
        owner: &OwnerId,
        entity: EmbeddableEntityRef,
        vectors: &[proxima_core::SpaceVector],
        proof: proxima_core::storage_ports::EmbeddingWriteProof,
    ) -> Result<EmbeddingWriteOutcome, StorageError> {
        let space = verbs::fact_embeddings::validate_embedding_vectors(vectors)?;
        let mut tx = self.platform_transaction().await?;
        verbs::fact_embeddings::lock_embedding_job_claim(&mut tx, owner, entity, space, proof)
            .await?;
        let outcome =
            verbs::fact_embeddings::insert_embedding(&mut tx, owner, entity, vectors).await?;
        tx.commit().await.map_err(crate::error::map_err)?;
        Ok(outcome)
    }
}

#[async_trait::async_trait]
impl EmbeddingJobPort for PgStorage {
    async fn claim_pending_embedding_jobs(
        &self,
        limit: i64,
        skip_owners: &[OwnerId],
    ) -> Result<Vec<EmbeddingJobClaim>, StorageError> {
        let mut tx = self.platform_transaction().await?;
        let rows =
            verbs::fact_embeddings::claim_pending_embedding_jobs(tx.as_mut(), limit, skip_owners)
                .await?;
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

    async fn count_embedding_job_status(
        &self,
        owner_scope: Option<&proxima_core::OwnerScope>,
        owner: &OwnerId,
    ) -> Result<EmbeddingJobStatusCounts, StorageError> {
        let mut tx = crate::begin_owner_transaction(
            &self.pool,
            crate::owner_scope::require_owner_scope(owner_scope)?,
        )
        .await?;
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
            &self.platform_scope,
            policy.stale_claim_timeout_seconds(),
        )
        .await
    }

    async fn sweep_orphan_embedding_rows(
        &self,
        _proof: OperatorMaintenanceProof,
    ) -> Result<EmbeddingOrphanSweepOutcome, StorageError> {
        verbs::fact_embeddings::sweep_orphan_embedding_rows(&self.pool, &self.platform_scope).await
    }

    async fn reconcile_embeddings(
        &self,
        options: proxima_core::EmbeddingReconcileOptions<'_>,
        policy: proxima_core::EmbeddingRuntimePolicy,
        _proof: OperatorMaintenanceProof,
    ) -> Result<proxima_core::EmbeddingReconcileOutcome, StorageError> {
        verbs::fact_embeddings::reconcile_embeddings_with_platform(
            &self.pool,
            &self.platform_scope,
            options,
            policy.stale_claim_timeout_seconds(),
        )
        .await
    }

    async fn embedding_owner_page(
        &self,
        after: Option<OwnerId>,
        limit: i64,
        _proof: OperatorMaintenanceProof,
    ) -> Result<Vec<OwnerId>, StorageError> {
        verbs::fact_embeddings::embedding_owner_page(&self.pool, &self.platform_scope, after, limit)
            .await
    }

    async fn embedding_coverage(
        &self,
        owner: &OwnerId,
        spaces: &[proxima_core::EmbeddingSpace],
        non_embeddable_schemas: &[String],
        _proof: OperatorMaintenanceProof,
    ) -> Result<
        Vec<(
            proxima_core::EmbeddingSpace,
            proxima_core::EmbeddingSpaceCounts,
        )>,
        StorageError,
    > {
        verbs::fact_embeddings::embedding_coverage(
            &self.pool,
            &self.platform_scope,
            owner,
            spaces,
            non_embeddable_schemas,
        )
        .await
    }

    async fn purge_embedding_spaces(
        &self,
        owner: &OwnerId,
        keep: &[proxima_core::EmbeddingSpace],
        limit: i64,
        _proof: OperatorMaintenanceProof,
    ) -> Result<proxima_core::EmbeddingPurgeOutcome, StorageError> {
        verbs::fact_embeddings::purge_embedding_spaces(
            &self.pool,
            &self.platform_scope,
            owner,
            keep,
            limit,
        )
        .await
    }
}
