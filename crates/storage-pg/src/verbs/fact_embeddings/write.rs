use proxima_core::llm::EMBEDDING_DIM;
use proxima_core::{
    EmbeddableEntityRef, EmbeddingWriteOutcome, EntityKind, MemoryId, Owner, StorageError,
};
use sqlx::{Postgres, Transaction};

use crate::error::map_err;

/// Append one memory embedding row inside an existing tx.
///
/// Crate-private: this is a raw-owner write below the proof gate. External
/// writers go through `EmbeddingWritePort`, which requires an
/// `EmbeddingWriteProof` only `proxima-core` can construct.
///
/// # Errors
///
/// Returns `ConstraintViolation` when `dim` or `vec.len()` do not match
/// the fixed embedding width, otherwise maps SQL failures through the
/// shared mapper.
pub(crate) async fn insert_memory_embedding(
    tx: &mut Transaction<'_, Postgres>,
    owner: &Owner,
    entity_kind: EntityKind,
    memory_id: MemoryId,
    model_id: &str,
    dim: usize,
    vec: &[f32],
) -> Result<EmbeddingWriteOutcome, StorageError> {
    insert_embedding(
        tx,
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

/// Append one embedding row and advance the independent latest head.
///
/// Crate-private: this is a raw-owner write below the proof gate. External
/// writers go through `EmbeddingWritePort`, which requires an
/// `EmbeddingWriteProof` only `proxima-core` can construct.
///
/// Returns version `0` when the entity is not currently eligible for
/// embedding, preserving the existing best-effort no-op behavior for deleted
/// or textless entities.
///
/// # Errors
///
/// Returns `ConstraintViolation` when the vector length differs from the fixed
/// embedding dimension, otherwise maps SQL failures through the shared mapper.
pub(crate) async fn insert_embedding(
    tx: &mut Transaction<'_, Postgres>,
    owner: &Owner,
    entity: EmbeddableEntityRef,
    model_id: &str,
    dim: usize,
    vec: &[f32],
) -> Result<EmbeddingWriteOutcome, StorageError> {
    insert_embedding_chunks(tx, owner, entity, model_id, dim, std::slice::from_ref(&vec)).await
}

/// Append one embedding *version* made of one or more chunk rows
/// (`chunk_index` 0..n) and advance the independent latest head. Chunked
/// versions represent one over-limit memory text split into provider-
/// acceptable pieces: search max-aggregates chunk similarity per memory,
/// so every part of the text stays semantically findable.
///
/// Crate-private: this is a raw-owner write below the proof gate. External
/// writers go through `EmbeddingWritePort`, which requires an
/// `EmbeddingWriteProof` only `proxima-core` can construct.
///
/// Returns version `0` when the entity is not currently eligible for
/// embedding, preserving the existing best-effort no-op behavior for deleted
/// or textless entities.
///
/// # Errors
///
/// Returns `ConstraintViolation` when `chunks` is empty or any vector's
/// length differs from the fixed embedding dimension, otherwise maps SQL
/// failures through the shared mapper.
pub(crate) async fn insert_embedding_chunks(
    tx: &mut Transaction<'_, Postgres>,
    owner: &Owner,
    entity: EmbeddableEntityRef,
    model_id: &str,
    dim: usize,
    chunks: &[&[f32]],
) -> Result<EmbeddingWriteOutcome, StorageError> {
    let owner_id = owner.stored_owner_id();
    if chunks.is_empty() {
        return Err(StorageError::ConstraintViolation(
            "embedding version needs at least one chunk".into(),
        ));
    }
    if dim != EMBEDDING_DIM || chunks.iter().any(|vec| vec.len() != EMBEDDING_DIM) {
        return Err(StorageError::ConstraintViolation(format!(
            "embedding length must be {EMBEDDING_DIM}"
        )));
    }

    let entity_id = entity.entity_id();
    let lock_key = format!("proxima-embedding:{entity_id}:{model_id}");
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
            AND model_id = $2",
    )
    .bind(entity_id)
    .bind(model_id)
    .fetch_one(tx.as_mut())
    .await
    .map_err(map_err)?;

    // 0008 embeddings have no chunk_index: one vec per version.
    let vec_literal = crate::pgvector::literal(chunks[0]);
    sqlx::query(
        "INSERT INTO proxima_core.embeddings
            (entity_id, model_id, embedding_version, vec, owner_id)
         VALUES ($1, $2, $3, $4::vector, $5)",
    )
    .bind(entity_id)
    .bind(model_id)
    .bind(embedding_version)
    .bind(vec_literal)
    .bind(owner_id)
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;

    sqlx::query(
        "INSERT INTO proxima_core.embedding_heads
            (entity_id, model_id, embedding_version, owner_id)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT (entity_id, model_id)
         DO UPDATE SET embedding_version = EXCLUDED.embedding_version",
    )
    .bind(entity_id)
    .bind(model_id)
    .bind(embedding_version)
    .bind(owner_id)
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;

    Ok(EmbeddingWriteOutcome { embedding_version })
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
