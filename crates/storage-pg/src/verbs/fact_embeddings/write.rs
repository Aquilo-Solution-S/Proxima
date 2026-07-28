use proxima_core::llm::EMBEDDING_DIM;
use proxima_core::{
    EmbeddableEntityRef, EmbeddingWriteOutcome, EntityKind, MemoryId, Owner, StorageError,
};
use sqlx::{Postgres, Transaction};

use crate::error::map_err;

use super::owner_parts;

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
    let (owner_kind, owner_id) = owner_parts(owner);
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

    let entity_kind = entity.entity_kind();
    let entity_id = entity.entity_id();
    let lock_key = format!(
        "proxima-embedding:{}:{}:{}",
        entity_kind.as_str(),
        entity_id,
        model_id
    );
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
          WHERE entity_kind = $1
            AND entity_id = $2
            AND model_id = $3",
    )
    .bind(entity_kind)
    .bind(entity_id)
    .bind(model_id)
    .fetch_one(tx.as_mut())
    .await
    .map_err(map_err)?;

    for (chunk_index, vec) in chunks.iter().enumerate() {
        let chunk_index = i32::try_from(chunk_index).map_err(|_| {
            StorageError::ConstraintViolation("chunk count does not fit i32".into())
        })?;
        let vec_literal = crate::pgvector::literal(vec);
        sqlx::query(
            "INSERT INTO proxima_core.embeddings
                (entity_kind, entity_id, embedding_version, model_id, vec,
                 owner_kind, owner_id, chunk_index)
             VALUES ($1, $2, $3, $4, $5::vector, $6, $7, $8)",
        )
        .bind(entity_kind)
        .bind(entity_id)
        .bind(embedding_version)
        .bind(model_id)
        .bind(vec_literal)
        .bind(owner_kind)
        .bind(owner_id)
        .bind(chunk_index)
        .execute(tx.as_mut())
        .await
        .map_err(map_err)?;
    }

    sqlx::query(
        "INSERT INTO proxima_core.embedding_heads
            (entity_kind, entity_id, model_id, embedding_version,
             owner_kind, owner_id)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (entity_kind, entity_id, model_id)
         DO UPDATE SET
             embedding_version = EXCLUDED.embedding_version,
             owner_kind = EXCLUDED.owner_kind,
             owner_id = EXCLUDED.owner_id,
             updated_at = now()",
    )
    .bind(entity_kind)
    .bind(entity_id)
    .bind(model_id)
    .bind(embedding_version)
    .bind(owner_kind)
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
    let (owner_kind, owner_id) = owner_parts(owner);
    let exists = match entity {
        EmbeddableEntityRef::Memory { kind, memory_id } => sqlx::query_scalar(
            "SELECT EXISTS (
                SELECT 1
                  FROM proxima_core.memories m
                 WHERE m.memory_id = $1
                   AND m.owner_kind = $2
                   AND m.owner_id IS NOT DISTINCT FROM $3
                   AND NULLIF(btrim(m.text), '') IS NOT NULL
                   AND m.tombstoned_at IS NULL
                   AND (
                       ($4 = 'Fact'::proxima_core.entity_kind AND m.kind IS NULL)
                       OR m.kind = $4
                   )
            )",
        )
        .bind(memory_id.into_inner())
        .bind(owner_kind)
        .bind(owner_id)
        .bind(kind)
        .fetch_one(tx.as_mut())
        .await
        .map_err(map_err)?,
        EmbeddableEntityRef::Goal(goal_id) => sqlx::query_scalar(
            "SELECT EXISTS (
                SELECT 1
                  FROM proxima_core.goals g
                 WHERE g.goal_id = $1
                   AND g.owner_kind = $2
                   AND g.owner_id IS NOT DISTINCT FROM $3
                   AND NULLIF(btrim(g.text), '') IS NOT NULL
            )",
        )
        .bind(goal_id.into_inner())
        .bind(owner_kind)
        .bind(owner_id)
        .fetch_one(tx.as_mut())
        .await
        .map_err(map_err)?,
    };
    Ok(exists)
}
