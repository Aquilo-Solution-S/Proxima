use proxima_core::llm::EMBEDDING_DIM;
use proxima_core::storage_ports::MemoryEmbeddingWrite;
use proxima_core::{
    EmbeddableEntityRef, EmbeddingWriteOutcome, EntityKind, MemoryId, Owner, StorageError,
};
use sqlx::{Postgres, Transaction};

use crate::error::map_err;

use super::owner_parts;

/// Take every entity's advisory lock for the whole batch in one round trip.
/// The keys arrive sorted and deduplicated, and `unnest` yields them in
/// array order, so two batches that overlap acquire the shared locks in the
/// same order and cannot deadlock against each other.
const LOCK_EMBEDDING_ENTITIES_SQL: &str = "SELECT pg_advisory_xact_lock(hashtextextended(key, 0))
       FROM unnest($1::text[]) AS t(key)";

/// One claimed batch's vectors in one statement.
///
/// The row insert, the head advance and the returned versions all read one
/// `versioned` CTE, so a unit repeated inside the batch is versioned by its
/// position and the head lands on the last of them — the same result serial
/// application gives, which is what makes the batch a substitution rather
/// than a second semantics. Units whose entity is no longer eligible drop
/// out of `eligible` and answer version `0`, exactly as the single-row path
/// no-ops for a deleted or textless entity.
const INSERT_EMBEDDING_BATCH_SQL: &str = "WITH input AS (
             SELECT *
               FROM unnest($2::proxima_core.entity_kind[], $3::uuid[], $4::text[],
                           $5::proxima_core.owner_ref_kind[], $6::uuid[])
                    WITH ORDINALITY
                      AS t(entity_kind, entity_id, vec, owner_kind, owner_id, unit)
         ),
         eligible AS (
             SELECT i.*
               FROM input i
              WHERE EXISTS (
                    SELECT 1
                      FROM proxima_core.memories m
                     WHERE m.memory_id = i.entity_id
                       AND m.owner_kind = i.owner_kind
                       AND m.owner_id IS NOT DISTINCT FROM i.owner_id
                       AND NULLIF(btrim(m.text), '') IS NOT NULL
                       AND m.tombstoned_at IS NULL
                       AND (
                           (i.entity_kind = 'Fact'::proxima_core.entity_kind
                               AND m.kind IS NULL)
                           OR m.kind = i.entity_kind
                       )
              )
         ),
         versioned AS MATERIALIZED (
             SELECT e.entity_kind, e.entity_id, e.vec, e.owner_kind, e.owner_id, e.unit,
                    ((SELECT COALESCE(max(prior.embedding_version), 0)
                        FROM proxima_core.embeddings prior
                       WHERE prior.entity_kind = e.entity_kind
                         AND prior.entity_id = e.entity_id
                         AND prior.model_id = $1)
                     + row_number() OVER (PARTITION BY e.entity_kind, e.entity_id
                                              ORDER BY e.unit))::int
                        AS embedding_version
               FROM eligible e
         ),
         written AS (
             INSERT INTO proxima_core.embeddings
                 (entity_kind, entity_id, embedding_version, model_id, vec,
                  owner_kind, owner_id, chunk_index)
             SELECT v.entity_kind, v.entity_id, v.embedding_version, $1, v.vec::vector,
                    v.owner_kind, v.owner_id, 0
               FROM versioned v
         ),
         heads AS (
             INSERT INTO proxima_core.embedding_heads
                 (entity_kind, entity_id, model_id, embedding_version,
                  owner_kind, owner_id)
             SELECT DISTINCT ON (v.entity_kind, v.entity_id)
                    v.entity_kind, v.entity_id, $1, v.embedding_version,
                    v.owner_kind, v.owner_id
               FROM versioned v
              ORDER BY v.entity_kind, v.entity_id, v.unit DESC
             ON CONFLICT (entity_kind, entity_id, model_id)
             DO UPDATE SET
                 embedding_version = EXCLUDED.embedding_version,
                 owner_kind = EXCLUDED.owner_kind,
                 owner_id = EXCLUDED.owner_id,
                 updated_at = now()
         )
         SELECT v.unit, v.embedding_version
           FROM versioned v
          ORDER BY v.unit";

#[derive(sqlx::FromRow)]
struct BatchEmbeddingVersionRow {
    unit: i64,
    embedding_version: i32,
}

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
    let _ = sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(embedding_lock_key(entity_kind, entity_id, model_id))
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

/// Append one embedding row per unit and advance every entity's head, in
/// one statement over `unnest` arrays.
///
/// Crate-private for the same reason as the single-row path above: this is
/// a raw-owner write below the proof gate, reached through
/// `EmbeddingWritePort`.
///
/// Returns one outcome per unit in input order. A unit whose entity is not
/// currently eligible answers version `0`, preserving the single-row path's
/// best-effort no-op for deleted or textless entities, and a unit repeated
/// within the batch is versioned by its position — the head ends on the
/// last occurrence, as it would after serial application.
///
/// # Errors
///
/// Returns `ConstraintViolation` when any vector's length differs from the
/// fixed embedding dimension, otherwise maps SQL failures through the
/// shared mapper.
pub(crate) async fn insert_memory_embeddings(
    tx: &mut Transaction<'_, Postgres>,
    model_id: &str,
    dim: usize,
    writes: &[MemoryEmbeddingWrite<'_>],
) -> Result<Vec<EmbeddingWriteOutcome>, StorageError> {
    if writes.is_empty() {
        return Ok(Vec::new());
    }
    if dim != EMBEDDING_DIM || writes.iter().any(|write| write.vec.len() != EMBEDDING_DIM) {
        return Err(StorageError::ConstraintViolation(format!(
            "embedding length must be {EMBEDDING_DIM}"
        )));
    }

    let mut lock_keys: Vec<String> = writes
        .iter()
        .map(|write| embedding_lock_key(write.entity_kind, write.memory_id.into_inner(), model_id))
        .collect();
    lock_keys.sort_unstable();
    lock_keys.dedup();
    sqlx::query(LOCK_EMBEDDING_ENTITIES_SQL)
        .bind(&lock_keys)
        .execute(tx.as_mut())
        .await
        .map_err(map_err)?;

    let mut entity_kinds = Vec::with_capacity(writes.len());
    let mut entity_ids = Vec::with_capacity(writes.len());
    let mut vectors = Vec::with_capacity(writes.len());
    let mut owner_kinds = Vec::with_capacity(writes.len());
    let mut owner_ids = Vec::with_capacity(writes.len());
    for write in writes {
        let (owner_kind, owner_id) = owner_parts(&write.owner);
        entity_kinds.push(write.entity_kind);
        entity_ids.push(write.memory_id.into_inner());
        vectors.push(crate::pgvector::literal(write.vec));
        owner_kinds.push(owner_kind);
        owner_ids.push(owner_id);
    }

    let rows = sqlx::query_as::<_, BatchEmbeddingVersionRow>(INSERT_EMBEDDING_BATCH_SQL)
        .bind(model_id)
        .bind(&entity_kinds)
        .bind(&entity_ids)
        .bind(&vectors)
        .bind(&owner_kinds)
        .bind(&owner_ids)
        .fetch_all(tx.as_mut())
        .await
        .map_err(map_err)?;

    let mut outcomes = vec![
        EmbeddingWriteOutcome {
            embedding_version: 0
        };
        writes.len()
    ];
    for row in rows {
        let index = usize::try_from(row.unit)
            .ok()
            .and_then(|unit| unit.checked_sub(1))
            .filter(|index| *index < outcomes.len())
            .ok_or_else(|| {
                StorageError::Internal(format!(
                    "batch embedding write returned unit {} outside the batch",
                    row.unit
                ))
            })?;
        outcomes[index] = EmbeddingWriteOutcome {
            embedding_version: row.embedding_version,
        };
    }
    Ok(outcomes)
}

/// The advisory-lock key serializing embedding version allocation for one
/// entity. Shared by both write paths on purpose: a batch that hashed a
/// different string would not serialize against a single-row write of the
/// same entity, and both could allocate the same version.
fn embedding_lock_key(entity_kind: EntityKind, entity_id: uuid::Uuid, model_id: &str) -> String {
    format!(
        "proxima-embedding:{}:{}:{}",
        entity_kind.as_str(),
        entity_id,
        model_id
    )
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

#[cfg(test)]
mod tests {
    use super::*;

    /// One claimed batch, one write statement: the round trip is the whole
    /// reason the drain batches at all, and a second statement hidden in
    /// this text would spend it again for every batch.
    #[test]
    fn the_batch_write_is_a_single_statement() {
        assert!(!INSERT_EMBEDDING_BATCH_SQL.contains(';'));
        assert!(!LOCK_EMBEDDING_ENTITIES_SQL.contains(';'));
    }

    /// Golden text: this statement carries the version allocation and the
    /// head advance for every batched write, so changing it has to be a
    /// change to this test as well.
    #[test]
    fn the_batch_write_sql_is_byte_identical() {
        assert_eq!(INSERT_EMBEDDING_BATCH_SQL, BATCH_WRITE_GOLDEN);
        assert_eq!(LOCK_EMBEDDING_ENTITIES_SQL, BATCH_LOCK_GOLDEN);
    }

    /// A batch that hashed a different string than the single-row path
    /// would serialize against nothing, and both paths could hand out the
    /// same embedding version.
    #[test]
    fn the_batch_locks_the_key_the_single_row_path_locks() {
        let entity_id = uuid::Uuid::from_u128(0x1111_2222_3333_4444);

        assert_eq!(
            embedding_lock_key(EntityKind::Fact, entity_id, "golden-embed"),
            format!("proxima-embedding:Fact:{entity_id}:golden-embed")
        );
    }

    const BATCH_LOCK_GOLDEN: &str = r"SELECT pg_advisory_xact_lock(hashtextextended(key, 0))
       FROM unnest($1::text[]) AS t(key)";

    const BATCH_WRITE_GOLDEN: &str = r"WITH input AS (
             SELECT *
               FROM unnest($2::proxima_core.entity_kind[], $3::uuid[], $4::text[],
                           $5::proxima_core.owner_ref_kind[], $6::uuid[])
                    WITH ORDINALITY
                      AS t(entity_kind, entity_id, vec, owner_kind, owner_id, unit)
         ),
         eligible AS (
             SELECT i.*
               FROM input i
              WHERE EXISTS (
                    SELECT 1
                      FROM proxima_core.memories m
                     WHERE m.memory_id = i.entity_id
                       AND m.owner_kind = i.owner_kind
                       AND m.owner_id IS NOT DISTINCT FROM i.owner_id
                       AND NULLIF(btrim(m.text), '') IS NOT NULL
                       AND m.tombstoned_at IS NULL
                       AND (
                           (i.entity_kind = 'Fact'::proxima_core.entity_kind
                               AND m.kind IS NULL)
                           OR m.kind = i.entity_kind
                       )
              )
         ),
         versioned AS MATERIALIZED (
             SELECT e.entity_kind, e.entity_id, e.vec, e.owner_kind, e.owner_id, e.unit,
                    ((SELECT COALESCE(max(prior.embedding_version), 0)
                        FROM proxima_core.embeddings prior
                       WHERE prior.entity_kind = e.entity_kind
                         AND prior.entity_id = e.entity_id
                         AND prior.model_id = $1)
                     + row_number() OVER (PARTITION BY e.entity_kind, e.entity_id
                                              ORDER BY e.unit))::int
                        AS embedding_version
               FROM eligible e
         ),
         written AS (
             INSERT INTO proxima_core.embeddings
                 (entity_kind, entity_id, embedding_version, model_id, vec,
                  owner_kind, owner_id, chunk_index)
             SELECT v.entity_kind, v.entity_id, v.embedding_version, $1, v.vec::vector,
                    v.owner_kind, v.owner_id, 0
               FROM versioned v
         ),
         heads AS (
             INSERT INTO proxima_core.embedding_heads
                 (entity_kind, entity_id, model_id, embedding_version,
                  owner_kind, owner_id)
             SELECT DISTINCT ON (v.entity_kind, v.entity_id)
                    v.entity_kind, v.entity_id, $1, v.embedding_version,
                    v.owner_kind, v.owner_id
               FROM versioned v
              ORDER BY v.entity_kind, v.entity_id, v.unit DESC
             ON CONFLICT (entity_kind, entity_id, model_id)
             DO UPDATE SET
                 embedding_version = EXCLUDED.embedding_version,
                 owner_kind = EXCLUDED.owner_kind,
                 owner_id = EXCLUDED.owner_id,
                 updated_at = now()
         )
         SELECT v.unit, v.embedding_version
           FROM versioned v
          ORDER BY v.unit";
}
