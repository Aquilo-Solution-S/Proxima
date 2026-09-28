use proxima_core::storage_ports::EmbeddingWriteProof;
use proxima_core::{
    EmbeddableEntityRef, EmbeddingSpace, EmbeddingWriteOutcome, EntityKind, MemoryId, Owner,
    SpaceVector, StorageError,
};
use sqlx::{Postgres, Transaction};

use crate::error::map_err;

/// Lock and validate the queue claim that authorized an embedding write.
///
/// The lock is held through the embedding transaction. A stale worker either
/// loses before writing, or commits before reclamation can mint a successor;
/// in the latter case the successor necessarily writes the later head.
pub(crate) async fn lock_embedding_job_claim(
    tx: &mut Transaction<'_, Postgres>,
    owner: &Owner,
    entity: EmbeddableEntityRef,
    space: &EmbeddingSpace,
    proof: EmbeddingWriteProof,
) -> Result<(), StorageError> {
    let Some((job_id, claim_token)) = proof.claim_fence() else {
        return Ok(());
    };
    lock_embedding_job_claim_fields(tx, owner, entity, space, job_id, claim_token).await
}

async fn lock_embedding_job_claim_fields(
    tx: &mut Transaction<'_, Postgres>,
    owner: &Owner,
    entity: EmbeddableEntityRef,
    space: &EmbeddingSpace,
    job_id: uuid::Uuid,
    claim_token: uuid::Uuid,
) -> Result<(), StorageError> {
    let locked = super::jobs::bind_claim_fence(
        sqlx::query(concat!(
            "SELECT job_id FROM proxima_core.embedding_jobs WHERE ",
            super::jobs::claim_fence!(),
            " FOR UPDATE"
        )),
        super::jobs::ClaimFence {
            job_id,
            claim_token,
            owner,
            entity_id: entity.entity_id(),
            space,
        },
    )
    .fetch_optional(tx.as_mut())
    .await
    .map_err(map_err)?;
    if locked.is_none() {
        return Err(StorageError::Conflict(
            "embedding job claim is stale or does not match the write".into(),
        ));
    }
    Ok(())
}

/// Append one complete memory embedding version inside an existing tx.
///
/// Crate-private: this is a raw-owner write below the proof gate. External
/// writers go through `EmbeddingWritePort`, which requires an
/// `EmbeddingWriteProof` only `proxima-core` can construct.
///
/// # Errors
///
/// Maps SQL failures through the shared mapper.
pub(crate) async fn insert_memory_embedding(
    tx: &mut Transaction<'_, Postgres>,
    owner: &Owner,
    entity_kind: EntityKind,
    memory_id: MemoryId,
    vectors: &[SpaceVector],
) -> Result<EmbeddingWriteOutcome, StorageError> {
    insert_embedding(
        tx,
        owner,
        EmbeddableEntityRef::Memory {
            kind: entity_kind,
            memory_id,
        },
        vectors,
    )
    .await
}

/// Append one embedding version and advance the independent latest head.
/// Chunks keep text order, and the head advances only after every chunk is stored.
///
/// Crate-private: this is a raw-owner write below the proof gate. External
/// writers go through `EmbeddingWritePort`, which requires an
/// `EmbeddingWriteProof` only `proxima-core` can construct.
///
/// Returns version `0` when the entity is not eligible for embedding: a
/// deleted or textless entity is a best-effort no-op, not an error.
///
/// # Errors
///
/// Maps SQL failures through the shared mapper.
pub(crate) async fn insert_embedding(
    tx: &mut Transaction<'_, Postgres>,
    owner: &Owner,
    entity: EmbeddableEntityRef,
    vectors: &[SpaceVector],
) -> Result<EmbeddingWriteOutcome, StorageError> {
    let owner_id = owner.stored_owner_id();
    let space = validate_embedding_vectors(vectors)?;
    let dim = crate::pgvector::Lane::of(space.dim()).width;
    let model_id = space.model_id();

    let entity_id = entity.entity_id();
    let lock_key = format!("proxima-embedding:{entity_id}:{model_id}:{dim}");
    let _ = sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(lock_key)
        .fetch_one(tx.as_mut())
        .await
        .map_err(map_err)?;

    if !embedding_entity_is_eligible(tx, owner, entity).await? {
        return Ok(EmbeddingWriteOutcome {
            embedding_version: 0,
        });
    }

    let embedding_version: i32 = sqlx::query_scalar(
        "SELECT COALESCE(max(embedding_version), 0) + 1
           FROM proxima_core.embeddings
          WHERE entity_id = $1
            AND model_id = $2
            AND dim = $3",
    )
    .bind(entity_id)
    .bind(model_id)
    .bind(dim)
    .fetch_one(tx.as_mut())
    .await
    .map_err(map_err)?;

    for (ordinal, vector) in vectors.iter().enumerate() {
        let chunk_ordinal = i32::try_from(ordinal).map_err(|_| {
            StorageError::ConstraintViolation("embedding has too many chunks".into())
        })?;
        sqlx::query(
            "INSERT INTO proxima_core.embeddings
                (entity_id, model_id, dim, embedding_version, chunk_ordinal, vec, owner_id)
             VALUES ($1, $2, $3, $4, $5, $6::vector, $7)",
        )
        .bind(entity_id)
        .bind(model_id)
        .bind(dim)
        .bind(embedding_version)
        .bind(chunk_ordinal)
        .bind(crate::pgvector::literal(vector.values()))
        .bind(owner_id)
        .execute(tx.as_mut())
        .await
        .map_err(map_err)?;
    }

    sqlx::query(
        "INSERT INTO proxima_core.embedding_heads
            (entity_id, model_id, dim, embedding_version, owner_id)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (entity_id, model_id, dim)
         DO UPDATE SET embedding_version = EXCLUDED.embedding_version",
    )
    .bind(entity_id)
    .bind(model_id)
    .bind(dim)
    .bind(embedding_version)
    .bind(owner_id)
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;

    Ok(EmbeddingWriteOutcome { embedding_version })
}

/// Validate the whole batch before acquiring a claim or writing any chunk.
pub(crate) fn validate_embedding_vectors(
    vectors: &[SpaceVector],
) -> Result<&EmbeddingSpace, StorageError> {
    let first = vectors.first().ok_or_else(|| {
        StorageError::ConstraintViolation("embedding must contain at least one chunk".into())
    })?;
    if i32::try_from(vectors.len()).is_err() {
        return Err(StorageError::ConstraintViolation(
            "embedding has too many chunks".into(),
        ));
    }
    if vectors.iter().any(|vector| vector.space() != first.space()) {
        return Err(StorageError::ConstraintViolation(
            "embedding chunks must use one space".into(),
        ));
    }
    if vectors
        .iter()
        .any(|vector| vector.values().iter().any(|value| !value.is_finite()))
    {
        return Err(StorageError::ConstraintViolation(
            "embedding chunks must contain finite values".into(),
        ));
    }
    Ok(first.space())
}

async fn embedding_entity_is_eligible(
    tx: &mut Transaction<'_, Postgres>,
    owner: &Owner,
    entity: EmbeddableEntityRef,
) -> Result<bool, StorageError> {
    let owner_id = owner.stored_owner_id();
    let entity_id = match entity {
        EmbeddableEntityRef::Memory { memory_id, .. } => memory_id.into_inner(),
        EmbeddableEntityRef::Goal(goal_id) => goal_id.into_inner(),
    };
    sqlx::query_scalar(
        "SELECT EXISTS (
            SELECT 1 FROM proxima_core.memory WHERE t = $1 AND owner_id = $2
            UNION ALL
            SELECT 1 FROM proxima_core.goal WHERE t = $1 AND owner_id = $2
        )",
    )
    .bind(entity_id)
    .bind(owner_id)
    .fetch_one(tx.as_mut())
    .await
    .map_err(map_err)
}
