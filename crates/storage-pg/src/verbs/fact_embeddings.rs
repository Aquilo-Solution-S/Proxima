use proxima_core::llm::{EMBEDDING_DIM, EMBEDDING_JOB_MAX_ATTEMPTS, EmbeddingClient};
use proxima_core::storage_ports::OwnerWritePermit;
use proxima_core::{
    EmbeddableEntityRef, EmbeddingAnnObservability, EmbeddingJobBacklog, EmbeddingJobClaim,
    EmbeddingOrphanCounts, EmbeddingOrphanSweepOutcome, EmbeddingRecallCanary,
    EmbeddingWriteOutcome, EntityKind, MemoryId, Owner, OwnerRefKind, StorageError,
};
use sqlx::{PgPool, Postgres, Transaction};
use std::collections::HashSet;
use time::OffsetDateTime;

use crate::error::map_err;
use crate::pgvector::{SET_HNSW_EF_SEARCH_SQL, SET_HNSW_ITERATIVE_SCAN_SQL};

fn owner_parts(owner: &Owner) -> (OwnerRefKind, Option<uuid::Uuid>) {
    owner.columns()
}

#[derive(sqlx::FromRow)]
struct EmbeddingJobClaimRow {
    owner_kind: OwnerRefKind,
    owner_id: Option<uuid::Uuid>,
    entity_kind: EntityKind,
    entity_id: uuid::Uuid,
    model_id: String,
    embedding_version: i32,
    attempts: i32,
}

impl From<EmbeddingJobClaimRow> for EmbeddingJobClaim {
    fn from(row: EmbeddingJobClaimRow) -> Self {
        Self {
            owner: row
                .owner_kind
                .with_uuid(row.owner_id)
                .expect("embedding job row has valid owner_ref shape"),
            entity_kind: row.entity_kind,
            entity_id: MemoryId::new(row.entity_id),
            model_id: row.model_id,
            embedding_version: row.embedding_version,
            attempts: row.attempts,
        }
    }
}

#[derive(sqlx::FromRow)]
struct EmbeddingAnnObservabilityRow {
    embedding_rows: i64,
    embedding_head_rows: i64,
    embedding_job_rows: i64,
    embedding_table_bytes: i64,
    embedding_total_relation_bytes: i64,
    hnsw_index_bytes: i64,
    pending_jobs: i64,
    processing_jobs: i64,
    failed_jobs: i64,
    stale_processing_jobs: i64,
    orphan_embeddings: i64,
    orphan_heads: i64,
    orphan_jobs: i64,
}

#[derive(sqlx::FromRow)]
struct RecallSampleRow {
    owner_kind: OwnerRefKind,
    owner_id: Option<uuid::Uuid>,
    model_id: String,
    vec: String,
}

#[derive(sqlx::FromRow)]
struct EmbeddingOrphanSweepRow {
    embeddings: i64,
    heads: i64,
    jobs: i64,
}

fn ensure_nonnegative_limit(limit: i64) -> Result<i64, StorageError> {
    if limit < 0 {
        return Err(StorageError::ConstraintViolation(
            "limit must be nonnegative".into(),
        ));
    }
    Ok(limit)
}

fn nonnegative_count(value: i64, name: &str) -> Result<u64, StorageError> {
    u64::try_from(value).map_err(|_| StorageError::Internal(format!("{name} count is negative")))
}

fn usize_count(value: usize, name: &str) -> Result<u64, StorageError> {
    u64::try_from(value).map_err(|_| StorageError::Internal(format!("{name} count too large")))
}

fn ratio_count(value: u64, name: &str) -> Result<u32, StorageError> {
    u32::try_from(value).map_err(|_| StorageError::Internal(format!("{name} count too large")))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbeddingReconcileScope {
    MissingOnly,
    IncludeStale,
    Since(OffsetDateTime),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmbeddingReconcileOptions<'a> {
    pub model_id: &'a str,
    pub scope: EmbeddingReconcileScope,
    pub limit: Option<i64>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EmbeddingReconcileOutcome {
    pub scanned: u64,
    pub enqueued: u64,
    pub skipped: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EmbeddingInlineDrainOutcome {
    pub embedded: usize,
    pub failed: usize,
}

const RECONCILE_EMBEDDINGS_SQL: &str = "
WITH scoped AS MATERIALIZED (
     SELECT COALESCE(m.kind, 'Fact'::proxima_core.entity_kind) AS entity_kind,
            m.memory_id,
            m.owner_kind,
            m.owner_id,
            m.created_at,
            h.embedding_version AS head_version
       FROM proxima_core.memories m
       LEFT JOIN proxima_core.embedding_heads h
         ON h.entity_kind = COALESCE(m.kind, 'Fact'::proxima_core.entity_kind)
        AND h.entity_id = m.memory_id
        AND h.model_id = $1
WHERE NULLIF(btrim(m.text), '') IS NOT NULL
        AND m.tombstoned_at IS NULL
        AND (
            m.kind IS NULL
            OR m.kind IN (
                'Abstraction'::proxima_core.entity_kind,
                'Perspective'::proxima_core.entity_kind
            )
        )
        AND ($3::text <> 'since' OR m.created_at >= $4)
 ),
 eligible AS MATERIALIZED (
     SELECT s.*,
            COALESCE(s.head_version + 1, 1) AS desired_embedding_version
       FROM scoped s
      WHERE (
            CASE WHEN $3::text = 'missing_only'
            THEN s.head_version IS NULL
            ELSE true
            END
        )
        AND NOT EXISTS (
            SELECT 1
              FROM proxima_core.embedding_jobs j
             WHERE j.owner_kind = s.owner_kind
               AND j.owner_id = s.owner_id
               AND j.entity_kind = s.entity_kind
               AND j.entity_id = s.memory_id
               AND j.model_id = $1
               AND j.embedding_version = COALESCE(s.head_version + 1, 1)
               AND j.status IN ('pending'::proxima_core.embedding_job_status,
                                'processing'::proxima_core.embedding_job_status)
        )
 ),
 limited AS MATERIALIZED (
     SELECT *
       FROM eligible
      ORDER BY created_at ASC, memory_id ASC
      LIMIT $2
 ),
 inserted AS (
     INSERT INTO proxima_core.embedding_jobs
         (owner_kind, owner_id,
          entity_kind, entity_id, model_id, embedding_version)
     SELECT owner_kind, owner_id,
            entity_kind, memory_id, $1, desired_embedding_version
       FROM limited
     ON CONFLICT (owner_kind, owner_id,
                  entity_kind, entity_id, model_id, embedding_version)
     DO NOTHING
     RETURNING 1
 )
 SELECT
     (SELECT count(*)::bigint FROM limited) AS scanned,
     (SELECT count(*)::bigint FROM inserted) AS enqueued";

/// Owner-scoped read of the rendered text stored on a Fact memory row.
///
/// # Errors
///
/// Returns `StorageError::Internal` for SQL failures.
pub async fn load_fact_text(
    pool: &PgPool,
    owner: &Owner,
    memory_id: MemoryId,
) -> Result<Option<String>, StorageError> {
    let (owner_kind, owner_id) = owner_parts(owner);
    sqlx::query_scalar(
        "SELECT text
           FROM proxima_core.memories
          WHERE memory_id = $1
            AND EXISTS (
                SELECT 1
                  FROM (SELECT memory_id AS entity_id, owner_kind, owner_id FROM proxima_core.memories UNION ALL SELECT goal_id AS entity_id, owner_kind, owner_id FROM proxima_core.goals) eo
                 WHERE eo.entity_id = memory_id
                   AND eo.owner_kind = $2
                   AND eo.owner_id = $3
)
            AND kind IS NULL
            AND tombstoned_at IS NULL",
    )
    .bind(memory_id.into_inner())
    .bind(owner_kind)
    .bind(owner_id)
    .fetch_optional(pool)
    .await
    .map_err(map_err)
}

/// Owner-scoped read of stored memory text for an embedding job.
///
/// Facts are encoded as `kind IS NULL`; derived memories carry
/// `Abstraction` / `Perspective` in `kind`.
///
/// # Errors
///
/// Returns `StorageError::Internal` for SQL failures.
pub async fn load_embedding_text(
    pool: &PgPool,
    owner: &Owner,
    entity_kind: EntityKind,
    memory_id: MemoryId,
) -> Result<Option<String>, StorageError> {
    let (owner_kind, owner_id) = owner_parts(owner);
    sqlx::query_scalar(
        "SELECT text
           FROM proxima_core.memories
          WHERE memory_id = $1
            AND EXISTS (
                SELECT 1
                  FROM (SELECT memory_id AS entity_id, owner_kind, owner_id FROM proxima_core.memories UNION ALL SELECT goal_id AS entity_id, owner_kind, owner_id FROM proxima_core.goals) eo
                 WHERE eo.entity_id = memory_id
                   AND eo.owner_kind = $2
                   AND eo.owner_id = $3
)
            AND text IS NOT NULL
            AND tombstoned_at IS NULL
            AND (
                ($4 = 'Fact'::proxima_core.entity_kind
                 AND kind IS NULL)
                OR kind = $4
            )",
    )
    .bind(memory_id.into_inner())
    .bind(owner_kind)
    .bind(owner_id)
    .bind(entity_kind)
    .fetch_optional(pool)
    .await
    .map_err(map_err)
}

/// Transaction-scoped variant of [`load_fact_text`].
///
/// # Errors
///
/// Returns `StorageError::Internal` for SQL failures.
pub async fn load_fact_text_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    owner: &Owner,
    memory_id: MemoryId,
) -> Result<Option<String>, StorageError> {
    let (owner_kind, owner_id) = owner_parts(owner);
    sqlx::query_scalar(
        "SELECT text
           FROM proxima_core.memories
          WHERE memory_id = $1
            AND EXISTS (
                SELECT 1
                  FROM (SELECT memory_id AS entity_id, owner_kind, owner_id FROM proxima_core.memories UNION ALL SELECT goal_id AS entity_id, owner_kind, owner_id FROM proxima_core.goals) eo
                 WHERE eo.entity_id = memory_id
                   AND eo.owner_kind = $2
                   AND eo.owner_id = $3
)
            AND kind IS NULL
            AND tombstoned_at IS NULL",
    )
    .bind(memory_id.into_inner())
    .bind(owner_kind)
    .bind(owner_id)
    .fetch_optional(tx.as_mut())
    .await
    .map_err(map_err)
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
    let (owner_kind, owner_id) = owner_parts(owner);
    if dim != EMBEDDING_DIM || vec.len() != EMBEDDING_DIM {
        return Err(StorageError::ConstraintViolation(
            "embedding length must be 1024".into(),
        ));
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

    let vec_literal = crate::pgvector::literal(vec);
    sqlx::query(
        "INSERT INTO proxima_core.embeddings
            (entity_kind, entity_id, embedding_version, model_id, vec,
             owner_kind, owner_id)
         VALUES ($1, $2, $3, $4, $5::vector, $6, $7)",
    )
    .bind(entity_kind)
    .bind(entity_id)
    .bind(embedding_version)
    .bind(model_id)
    .bind(vec_literal)
    .bind(owner_kind)
    .bind(owner_id)
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;

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

/// Owner-scoped list of Facts with rendered text and no embedding row
/// for `model_id`.
///
/// # Errors
///
/// Returns `ConstraintViolation` when `limit` is too large for
/// Postgres `bigint`, otherwise maps SQL failures through the shared
/// mapper.
pub async fn list_facts_missing_embedding(
    pool: &PgPool,
    owner: &Owner,
    model_id: &str,
    limit: usize,
) -> Result<Vec<MemoryId>, StorageError> {
    let (owner_kind, owner_id) = owner_parts(owner);
    let limit = i64::try_from(limit)
        .map_err(|_| StorageError::ConstraintViolation("limit too large".into()))?;
    let rows = sqlx::query_scalar::<_, uuid::Uuid>(
        "SELECT m.memory_id
           FROM proxima_core.memories m
          WHERE EXISTS (
                    SELECT 1
                      FROM (SELECT memory_id AS entity_id, owner_kind, owner_id FROM proxima_core.memories UNION ALL SELECT goal_id AS entity_id, owner_kind, owner_id FROM proxima_core.goals) eo
                     WHERE eo.entity_id = m.memory_id
                       AND eo.owner_kind = $1
                       AND eo.owner_id = $2
)
            AND m.kind IS NULL
            AND m.text IS NOT NULL
            AND m.tombstoned_at IS NULL
            AND NOT EXISTS (
                SELECT 1
                  FROM proxima_core.embedding_heads h
                 WHERE h.entity_kind = 'Fact'
                   AND h.entity_id = m.memory_id
                   AND h.model_id = $3
            )
          ORDER BY m.created_at ASC, m.memory_id ASC
          LIMIT $4",
    )
    .bind(owner_kind)
    .bind(owner_id)
    .bind(model_id)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(map_err)?;
    Ok(rows.into_iter().map(MemoryId::new).collect())
}

/// Atomically claim pending Fact embedding jobs for one model.
///
/// Stale `processing` jobs orphaned by a crashed or restarted drainer are
/// reclaimed after fifteen minutes. The window MUST exceed the embedding
/// client's request timeout (currently ten minutes,
/// `crates/llm-openai-compat/src/openai_compat.rs:29`). Inline drainers call
/// this with `limit = 1` in a loop, so a claimed row is actively processed
/// immediately and cannot age past the reclaim window behind earlier jobs in
/// the same batch. The claim `UPDATE` resets `updated_at = now()`, so a
/// reclaimed orphan restarts its clock. Reclaim does not increment attempts;
/// crash-loop poison-pill bounding is out of scope, while embed-error retries
/// still go through `fail_embedding_job`.
///
/// # Errors
///
/// Returns `ConstraintViolation` for negative limits, otherwise maps SQL
/// failures through the shared mapper.
pub async fn claim_pending_embedding_jobs(
    pool: &PgPool,
    model_id: &str,
    limit: i64,
) -> Result<Vec<EmbeddingJobClaim>, StorageError> {
    claim_pending_embedding_jobs_excluding(pool, model_id, limit, &[]).await
}

async fn claim_pending_embedding_jobs_excluding(
    pool: &PgPool,
    model_id: &str,
    limit: i64,
    exclude_entity_ids: &[uuid::Uuid],
) -> Result<Vec<EmbeddingJobClaim>, StorageError> {
    let limit = ensure_nonnegative_limit(limit)?;
    if limit == 0 {
        return Ok(Vec::new());
    }
    let rows = sqlx::query_as::<_, EmbeddingJobClaimRow>(
        "WITH claimed AS (
             SELECT owner_kind, owner_id,
                    entity_kind, entity_id, model_id, embedding_version
               FROM proxima_core.embedding_jobs
              WHERE model_id = $1
                AND (status = 'pending'
                     OR (status = 'processing'
                         AND updated_at < now() - interval '15 minutes'))
                AND NOT (entity_id = ANY($2::uuid[]))
              ORDER BY enqueued_at ASC,
                       owner_kind ASC,
                       owner_id ASC,
                       entity_kind ASC,
                       entity_id ASC,
                       embedding_version ASC
              FOR UPDATE SKIP LOCKED
              LIMIT $3
         )
         UPDATE proxima_core.embedding_jobs j
            SET status = 'processing',
                updated_at = now()
           FROM claimed
          WHERE j.owner_kind = claimed.owner_kind
            AND j.owner_id = claimed.owner_id
            AND j.entity_kind = claimed.entity_kind
            AND j.entity_id = claimed.entity_id
            AND j.model_id = claimed.model_id
            AND j.embedding_version = claimed.embedding_version
        RETURNING j.owner_kind, j.owner_id,
                  j.entity_kind, j.entity_id, j.model_id, j.embedding_version,
                  j.attempts",
    )
    .bind(model_id)
    .bind(exclude_entity_ids)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(map_err)?;
    Ok(rows.into_iter().map(EmbeddingJobClaim::from).collect())
}

/// Delete a completed embedding job.
///
/// # Errors
///
/// Maps SQL failures through the shared mapper.
pub async fn complete_embedding_job(
    pool: &PgPool,
    claim: &EmbeddingJobClaim,
) -> Result<(), StorageError> {
    let (owner_kind, owner_id) = owner_parts(&claim.owner);
    sqlx::query(
        "DELETE FROM proxima_core.embedding_jobs
          WHERE owner_kind = $1
            AND owner_id = $2
            AND entity_kind = $3
            AND entity_id = $4
            AND model_id = $5
            AND embedding_version = $6",
    )
    .bind(owner_kind)
    .bind(owner_id)
    .bind(claim.entity_kind)
    .bind(claim.entity_id.into_inner())
    .bind(&claim.model_id)
    .bind(claim.embedding_version)
    .execute(pool)
    .await
    .map_err(map_err)?;
    Ok(())
}

/// Record a failed embedding attempt and retry until the cap.
///
/// # Errors
///
/// Maps SQL failures through the shared mapper.
pub async fn fail_embedding_job(
    pool: &PgPool,
    claim: &EmbeddingJobClaim,
    error: &str,
) -> Result<(), StorageError> {
    let (owner_kind, owner_id) = owner_parts(&claim.owner);
    sqlx::query(
        "UPDATE proxima_core.embedding_jobs
            SET attempts = attempts + 1,
                last_error = $7,
                updated_at = now(),
                status = CASE
                    WHEN attempts + 1 >= $8
                    THEN 'failed'::proxima_core.embedding_job_status
                    ELSE 'pending'::proxima_core.embedding_job_status
                END
          WHERE owner_kind = $1
            AND owner_id = $2
            AND entity_kind = $3
            AND entity_id = $4
            AND model_id = $5
            AND embedding_version = $6
            AND status = 'processing'",
    )
    .bind(owner_kind)
    .bind(owner_id)
    .bind(claim.entity_kind)
    .bind(claim.entity_id.into_inner())
    .bind(&claim.model_id)
    .bind(claim.embedding_version)
    .bind(error)
    .bind(EMBEDDING_JOB_MAX_ATTEMPTS)
    .execute(pool)
    .await
    .map_err(map_err)?;
    Ok(())
}

/// Enqueue pending jobs for owner-scoped Facts missing a current
/// embedding.
///
/// # Errors
///
/// Returns `ConstraintViolation` for negative limits, otherwise maps SQL
/// failures through the shared mapper.
pub async fn enqueue_missing_embedding_jobs(
    pool: &PgPool,
    permit: &OwnerWritePermit,
    model_id: &str,
    limit: i64,
) -> Result<u64, StorageError> {
    let limit = ensure_nonnegative_limit(limit)?;
    if limit == 0 {
        return Ok(0);
    }
    let (owner_kind, owner_id) = owner_parts(permit.owner());
    let result = sqlx::query(
        "WITH missing AS (
             SELECT m.memory_id
               FROM proxima_core.memories m
              WHERE EXISTS (
                        SELECT 1
                          FROM (SELECT memory_id AS entity_id, owner_kind, owner_id FROM proxima_core.memories UNION ALL SELECT goal_id AS entity_id, owner_kind, owner_id FROM proxima_core.goals) eo
                         WHERE eo.entity_id = m.memory_id
                           AND eo.owner_kind = $1
                           AND eo.owner_id = $2
)
                AND m.kind IS NULL
                AND m.text IS NOT NULL
                AND m.tombstoned_at IS NULL
                AND NOT EXISTS (
                    SELECT 1
                      FROM proxima_core.embedding_heads h
                     WHERE h.entity_kind = 'Fact'
                       AND h.entity_id = m.memory_id
                       AND h.model_id = $3
                )
              ORDER BY m.created_at ASC, m.memory_id ASC
              LIMIT $4
         )
         INSERT INTO proxima_core.embedding_jobs
             (owner_kind, owner_id,
              entity_kind, entity_id, model_id)
         SELECT $1, $2, 'Fact'::proxima_core.entity_kind, memory_id, $3
           FROM missing
         ON CONFLICT (owner_kind, owner_id,
                      entity_kind, entity_id, model_id, embedding_version)
         DO NOTHING",
    )
    .bind(owner_kind)
    .bind(owner_id)
    .bind(model_id)
    .bind(limit)
    .execute(pool)
    .await
    .map_err(map_err)?;
    Ok(result.rows_affected())
}

/// Owner-scoped count of embedding jobs not yet embedded.
///
/// # Errors
///
/// Maps SQL failures through the shared mapper.
pub async fn count_pending_embedding_jobs(
    pool: &PgPool,
    owner: &Owner,
) -> Result<u64, StorageError> {
    let (owner_kind, owner_id) = owner_parts(owner);
    let row: (i64,) = sqlx::query_as(
        "SELECT count(*)
           FROM proxima_core.embedding_jobs
          WHERE owner_kind = $1
            AND owner_id = $2
            AND status IN ('pending', 'processing')",
    )
    .bind(owner_kind)
    .bind(owner_id)
    .fetch_one(pool)
    .await
    .map_err(map_err)?;
    u64::try_from(row.0)
        .map_err(|_| StorageError::Internal("pending embedding job count is negative".into()))
}

/// Owner-agnostic embedding ANN health signals for operator surfaces.
///
/// Authorization is intentionally outside storage; callers must gate this
/// read through `Engine::embedding_ann_observability`.
///
/// # Errors
///
/// Maps SQL failures through the shared mapper.
pub(crate) async fn embedding_ann_observability(
    pool: &PgPool,
) -> Result<EmbeddingAnnObservability, StorageError> {
    let row = sqlx::query_as::<_, EmbeddingAnnObservabilityRow>(
        "WITH source_entities AS MATERIALIZED (
             SELECT 'Goal'::proxima_core.entity_kind AS entity_kind,
                    goal_id AS entity_id
               FROM proxima_core.goals
             UNION ALL
             SELECT proxima_core.memory_entity_kind(kind) AS entity_kind,
                    memory_id AS entity_id
               FROM proxima_core.memories
         ),
         orphan_embeddings AS (
             SELECT count(*)::bigint AS count
               FROM proxima_core.embeddings emb
              WHERE NOT EXISTS (
                    SELECT 1
                      FROM source_entities src
                     WHERE src.entity_kind = emb.entity_kind
                       AND src.entity_id = emb.entity_id
              )
         ),
         orphan_heads AS (
             SELECT count(*)::bigint AS count
               FROM proxima_core.embedding_heads head
              WHERE NOT EXISTS (
                    SELECT 1
                      FROM source_entities src
                     WHERE src.entity_kind = head.entity_kind
                       AND src.entity_id = head.entity_id
              )
         ),
         orphan_jobs AS (
             SELECT count(*)::bigint AS count
               FROM proxima_core.embedding_jobs job
              WHERE NOT EXISTS (
                    SELECT 1
                      FROM source_entities src
                     WHERE src.entity_kind = job.entity_kind
                       AND src.entity_id = job.entity_id
              )
         )
         SELECT
             (SELECT count(*)::bigint FROM proxima_core.embeddings)
                 AS embedding_rows,
             (SELECT count(*)::bigint FROM proxima_core.embedding_heads)
                 AS embedding_head_rows,
             (SELECT count(*)::bigint FROM proxima_core.embedding_jobs)
                 AS embedding_job_rows,
             pg_relation_size('proxima_core.embeddings'::regclass)::bigint
                 AS embedding_table_bytes,
             pg_total_relation_size('proxima_core.embeddings'::regclass)::bigint
                 AS embedding_total_relation_bytes,
             pg_relation_size('proxima_core.idx_embeddings_vec_hnsw'::regclass)::bigint
                 AS hnsw_index_bytes,
             (SELECT count(*)::bigint
                FROM proxima_core.embedding_jobs
               WHERE status = 'pending'::proxima_core.embedding_job_status)
                 AS pending_jobs,
             (SELECT count(*)::bigint
                FROM proxima_core.embedding_jobs
               WHERE status = 'processing'::proxima_core.embedding_job_status)
                 AS processing_jobs,
             (SELECT count(*)::bigint
                FROM proxima_core.embedding_jobs
               WHERE status = 'failed'::proxima_core.embedding_job_status)
                 AS failed_jobs,
             (SELECT count(*)::bigint
                FROM proxima_core.embedding_jobs
               WHERE status = 'processing'::proxima_core.embedding_job_status
                 AND updated_at < now() - interval '15 minutes')
                 AS stale_processing_jobs,
             (SELECT count FROM orphan_embeddings) AS orphan_embeddings,
             (SELECT count FROM orphan_heads) AS orphan_heads,
             (SELECT count FROM orphan_jobs) AS orphan_jobs",
    )
    .fetch_one(pool)
    .await
    .map_err(map_err)?;

    observability_from_row(&row, embedding_recall_canary(pool, 10).await?)
}

fn observability_from_row(
    row: &EmbeddingAnnObservabilityRow,
    recall_canary: Option<EmbeddingRecallCanary>,
) -> Result<EmbeddingAnnObservability, StorageError> {
    Ok(EmbeddingAnnObservability {
        embedding_rows: nonnegative_count(row.embedding_rows, "embedding rows")?,
        embedding_head_rows: nonnegative_count(row.embedding_head_rows, "embedding head rows")?,
        embedding_job_rows: nonnegative_count(row.embedding_job_rows, "embedding job rows")?,
        embedding_table_bytes: nonnegative_count(
            row.embedding_table_bytes,
            "embedding table bytes",
        )?,
        embedding_total_relation_bytes: nonnegative_count(
            row.embedding_total_relation_bytes,
            "embedding total relation bytes",
        )?,
        hnsw_index_bytes: nonnegative_count(row.hnsw_index_bytes, "hnsw index bytes")?,
        backlog: EmbeddingJobBacklog {
            pending: nonnegative_count(row.pending_jobs, "pending embedding jobs")?,
            processing: nonnegative_count(row.processing_jobs, "processing embedding jobs")?,
            failed: nonnegative_count(row.failed_jobs, "failed embedding jobs")?,
        },
        stale_processing_jobs: nonnegative_count(
            row.stale_processing_jobs,
            "stale processing embedding jobs",
        )?,
        orphan_rows: EmbeddingOrphanCounts {
            embeddings: nonnegative_count(row.orphan_embeddings, "orphan embeddings")?,
            heads: nonnegative_count(row.orphan_heads, "orphan embedding heads")?,
            jobs: nonnegative_count(row.orphan_jobs, "orphan embedding jobs")?,
        },
        recall_canary,
    })
}

async fn embedding_recall_canary(
    pool: &PgPool,
    k: i64,
) -> Result<Option<EmbeddingRecallCanary>, StorageError> {
    let Some(sample) = sqlx::query_as::<_, RecallSampleRow>(
        "SELECT emb.owner_kind, emb.owner_id, emb.model_id, emb.vec::text AS vec
           FROM proxima_core.embeddings emb
           JOIN proxima_core.embedding_heads head
             ON head.entity_kind = emb.entity_kind
            AND head.entity_id = emb.entity_id
            AND head.model_id = emb.model_id
            AND head.embedding_version = emb.embedding_version
            AND head.owner_kind = emb.owner_kind
            AND head.owner_id IS NOT DISTINCT FROM emb.owner_id
          ORDER BY emb.created_at DESC, emb.entity_id DESC
          LIMIT 1",
    )
    .fetch_optional(pool)
    .await
    .map_err(map_err)?
    else {
        return Ok(None);
    };

    let exact_ids = current_embedding_ids_by_distance(
        pool,
        sample.owner_kind,
        sample.owner_id,
        &sample.model_id,
        &sample.vec,
        k,
        DistancePlan::Exact,
    )
    .await?;
    let ann_ids = current_embedding_ids_by_distance(
        pool,
        sample.owner_kind,
        sample.owner_id,
        &sample.model_id,
        &sample.vec,
        k,
        DistancePlan::Ann,
    )
    .await?;
    let exact_set: HashSet<_> = exact_ids.iter().copied().collect();
    let overlap_count = ann_ids
        .iter()
        .filter(|entity_id| exact_set.contains(entity_id))
        .count();
    let exact_count = usize_count(exact_ids.len(), "exact recall")?;
    let ann_count = usize_count(ann_ids.len(), "ANN recall")?;
    let overlap_count = usize_count(overlap_count, "recall overlap")?;
    let recall_at_k = if exact_count == 0 {
        1.0
    } else {
        f64::from(ratio_count(overlap_count, "recall overlap")?)
            / f64::from(ratio_count(exact_count, "exact recall")?)
    };

    Ok(Some(EmbeddingRecallCanary {
        model_id: sample.model_id,
        k: nonnegative_count(k, "recall canary k")?,
        exact_count,
        ann_count,
        overlap_count,
        recall_at_k,
    }))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DistancePlan {
    Exact,
    Ann,
}

async fn current_embedding_ids_by_distance(
    pool: &PgPool,
    owner_kind: OwnerRefKind,
    owner_id: Option<uuid::Uuid>,
    model_id: &str,
    vec: &str,
    k: i64,
    plan: DistancePlan,
) -> Result<Vec<uuid::Uuid>, StorageError> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|err| StorageError::Internal(format!("begin recall canary tx: {err}")))?;
    match plan {
        DistancePlan::Exact => {
            sqlx::query("SET LOCAL enable_indexscan = off")
                .execute(tx.as_mut())
                .await
                .map_err(map_err)?;
            sqlx::query("SET LOCAL enable_indexonlyscan = off")
                .execute(tx.as_mut())
                .await
                .map_err(map_err)?;
            sqlx::query("SET LOCAL enable_bitmapscan = off")
                .execute(tx.as_mut())
                .await
                .map_err(map_err)?;
        }
        DistancePlan::Ann => {
            sqlx::query("SET LOCAL enable_seqscan = off")
                .execute(tx.as_mut())
                .await
                .map_err(map_err)?;
            sqlx::query(SET_HNSW_EF_SEARCH_SQL)
                .execute(tx.as_mut())
                .await
                .map_err(map_err)?;
            sqlx::query(SET_HNSW_ITERATIVE_SCAN_SQL)
                .execute(tx.as_mut())
                .await
                .map_err(map_err)?;
        }
    }
    let rows = sqlx::query_scalar::<_, uuid::Uuid>(
        "SELECT emb.entity_id
           FROM proxima_core.embeddings emb
           JOIN proxima_core.embedding_heads head
             ON head.entity_kind = emb.entity_kind
            AND head.entity_id = emb.entity_id
            AND head.model_id = emb.model_id
            AND head.embedding_version = emb.embedding_version
            AND head.owner_kind = emb.owner_kind
            AND head.owner_id IS NOT DISTINCT FROM emb.owner_id
          WHERE emb.model_id = $1
            AND emb.owner_kind = $2
            AND emb.owner_id IS NOT DISTINCT FROM $3
          ORDER BY emb.vec <=> $4::vector,
                   emb.entity_id ASC
          LIMIT $5",
    )
    .bind(model_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(vec)
    .bind(k)
    .fetch_all(tx.as_mut())
    .await
    .map_err(map_err)?;
    tx.commit().await.map_err(map_err)?;
    Ok(rows)
}

/// Delete embedding infrastructure rows whose source entity no longer exists.
///
/// Compliance erase performs synchronous cascade deletes and must not rely on
/// this crash-residue maintenance path for lawful wipe semantics.
///
/// # Errors
///
/// Maps SQL failures through the shared mapper.
pub(crate) async fn sweep_orphan_embedding_rows(
    pool: &PgPool,
) -> Result<EmbeddingOrphanSweepOutcome, StorageError> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|err| StorageError::Internal(format!("begin embedding orphan sweep tx: {err}")))?;

    let row = sqlx::query_as::<_, EmbeddingOrphanSweepRow>(
        "WITH source_entities AS MATERIALIZED (
             SELECT 'Goal'::proxima_core.entity_kind AS entity_kind,
                    goal_id AS entity_id
               FROM proxima_core.goals
             UNION ALL
             SELECT proxima_core.memory_entity_kind(kind) AS entity_kind,
                    memory_id AS entity_id
               FROM proxima_core.memories
         ),
         deleted_jobs AS (
             DELETE FROM proxima_core.embedding_jobs job
              WHERE NOT EXISTS (
                    SELECT 1
                      FROM source_entities src
                     WHERE src.entity_kind = job.entity_kind
                       AND src.entity_id = job.entity_id
              )
              RETURNING 1
         ),
         deleted_heads AS (
             DELETE FROM proxima_core.embedding_heads head
              WHERE NOT EXISTS (
                    SELECT 1
                      FROM source_entities src
                     WHERE src.entity_kind = head.entity_kind
                       AND src.entity_id = head.entity_id
              )
              RETURNING 1
         ),
         deleted_embeddings AS (
             DELETE FROM proxima_core.embeddings emb
              WHERE NOT EXISTS (
                    SELECT 1
                      FROM source_entities src
                     WHERE src.entity_kind = emb.entity_kind
                       AND src.entity_id = emb.entity_id
              )
              RETURNING 1
         )
         SELECT
          (SELECT count(*)::bigint FROM deleted_embeddings) AS embeddings,
          (SELECT count(*)::bigint FROM deleted_heads) AS heads,
          (SELECT count(*)::bigint FROM deleted_jobs) AS jobs",
    )
    .fetch_one(tx.as_mut())
    .await
    .map_err(map_err)?;

    tx.commit().await.map_err(map_err)?;
    Ok(EmbeddingOrphanSweepOutcome {
        embeddings_deleted: nonnegative_count(row.embeddings, "deleted embeddings")?,
        heads_deleted: nonnegative_count(row.heads, "deleted embedding heads")?,
        jobs_deleted: nonnegative_count(row.jobs, "deleted embedding jobs")?,
    })
}

/// Global enqueue-only reconciliation for embeddable memories.
///
/// Scans Facts plus derived memories with stored text, skips rows by
/// scope-specific embedding coverage and target-model durable jobs,
/// and enqueues via `proxima_core.embedding_jobs`.
///
/// # Errors
///
/// Returns `ConstraintViolation` for negative limits, otherwise maps SQL
/// failures through the shared mapper.
pub async fn reconcile_embeddings(
    pool: &PgPool,
    options: EmbeddingReconcileOptions<'_>,
) -> Result<EmbeddingReconcileOutcome, StorageError> {
    let limit = match options.limit {
        Some(limit) => ensure_nonnegative_limit(limit)?,
        None => i64::MAX,
    };
    if limit == 0 {
        return Ok(EmbeddingReconcileOutcome::default());
    }

    let (scope, since) = match options.scope {
        EmbeddingReconcileScope::MissingOnly => ("missing_only", None),
        EmbeddingReconcileScope::IncludeStale => ("include_stale", None),
        EmbeddingReconcileScope::Since(since) => ("since", Some(since)),
    };

    let row: (i64, i64) = sqlx::query_as(RECONCILE_EMBEDDINGS_SQL)
        .bind(options.model_id)
        .bind(limit)
        .bind(scope)
        .bind(since)
        .fetch_one(pool)
        .await
        .map_err(map_err)?;

    let scanned = u64::try_from(row.0)
        .map_err(|_| StorageError::Internal("scanned count is negative".into()))?;
    let enqueued = u64::try_from(row.1)
        .map_err(|_| StorageError::Internal("enqueued count is negative".into()))?;
    Ok(EmbeddingReconcileOutcome {
        scanned,
        enqueued,
        skipped: scanned.saturating_sub(enqueued),
    })
}

/// Drain queued embedding jobs inline for one embedding client.
///
/// # Errors
///
/// Returns storage errors from claiming, embedding row writes, or final
/// job-state writes. Per-job embedding failures are recorded on job rows
/// and counted in the returned outcome.
pub async fn drain_embedding_jobs_inline(
    pool: &PgPool,
    client: &dyn EmbeddingClient,
    limit: i64,
) -> Result<EmbeddingInlineDrainOutcome, StorageError> {
    let limit = ensure_nonnegative_limit(limit)?;
    let mut outcome = EmbeddingInlineDrainOutcome::default();
    let mut processed_entity_ids = Vec::new();
    for _ in 0..limit {
        let Some(claim) = claim_pending_embedding_jobs_excluding(
            pool,
            client.model_id(),
            1,
            &processed_entity_ids,
        )
        .await?
        .into_iter()
        .next() else {
            break;
        };
        processed_entity_ids.push(claim.entity_id.into_inner());
        match embed_claim(pool, client, &claim).await {
            Ok(true) => {
                complete_embedding_job(pool, &claim).await?;
                outcome.embedded += 1;
            }
            Ok(false) => {
                complete_embedding_job(pool, &claim).await?;
            }
            Err(err) => {
                outcome.failed += 1;
                fail_embedding_job(pool, &claim, &err.to_string()).await?;
            }
        }
    }
    Ok(outcome)
}

async fn embed_claim(
    pool: &PgPool,
    client: &dyn EmbeddingClient,
    claim: &EmbeddingJobClaim,
) -> Result<bool, StorageError> {
    let Some(text) =
        load_embedding_text(pool, &claim.owner, claim.entity_kind, claim.entity_id).await?
    else {
        return Ok(false);
    };
    let embedding = client
        .embed(&text)
        .await
        .map_err(|err| StorageError::Internal(format!("embed memory text: {err}")))?;
    if embedding.len() != client.dim() {
        return Err(StorageError::ConstraintViolation(format!(
            "embedding dim mismatch: client dim {} but vector len {}",
            client.dim(),
            embedding.len(),
        )));
    }

    let mut tx = pool.begin().await.map_err(|err| {
        StorageError::Internal(format!("begin memory embedding upsert tx: {err}"))
    })?;
    insert_memory_embedding(
        &mut tx,
        &claim.owner,
        claim.entity_kind,
        claim.entity_id,
        client.model_id(),
        client.dim(),
        &embedding,
    )
    .await?;
    tx.commit().await.map_err(map_err)?;
    Ok(true)
}

// Raw-owner write behavior tests live in-crate: `insert_embedding` /
// `insert_memory_embedding` are `pub(crate)` (below the proof gate), so
// external test binaries cannot reach them without a forgeable-proof surface.
#[cfg(test)]
mod pg_tests {
    use proxima_core::storage_ports::OwnerWritePermit;
    use proxima_core::test_fixtures::owner_fixture;
    use proxima_core::verbs::fact_ingest::{FactReceiptDraft, FactWriteCommand};
    use proxima_core::{
        AccessKind, AuthPath, AuthzContext, Engine, EntityKind, FactIngestPort, FlavorRegistry,
        GoalId, Owner, SchemaId, SchemaVersion, SourceBatchId, SourceId, StorageError,
    };
    use proxima_pg_testkit::drop_db;
    use uuid::Uuid;

    use super::{
        EMBEDDING_DIM, EmbeddableEntityRef, claim_pending_embedding_jobs, insert_embedding,
        insert_memory_embedding, load_embedding_text,
    };
    use crate::test_fixtures::fresh_pg;

    fn padded_embedding(prefix: [f32; 3]) -> Vec<f32> {
        let mut embedding = vec![0.0; EMBEDDING_DIM];
        embedding[..prefix.len()].copy_from_slice(&prefix);
        embedding
    }

    fn fact_draft(label: &str) -> FactWriteCommand {
        let now = time::OffsetDateTime::now_utc();
        FactWriteCommand {
            schema_id: SchemaId::new("proxima-test/fact-embedding-v1".into()),
            schema_version: SchemaVersion::new(1),
            payload: label.as_bytes().to_vec(),
            rendered_text: Some(label.to_string()),
            receipt: Some(FactReceiptDraft {
                source_id: SourceId::new("proxima-test/fact-embedding"),
                source_batch_id: SourceBatchId::new(Uuid::now_v7()),
                observed_at: now,
                occurred_at: now,
            }),
            citation: None,
        }
    }

    async fn owner_fact_write_permit(owner: &Owner) -> Result<OwnerWritePermit, StorageError> {
        let Owner::Personal(user_id) = owner else {
            return Err(StorageError::Internal(
                "fact embedding test helper expects a personal owner".into(),
            ));
        };
        let engine = Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests());
        let authz = AuthzContext::for_subject(*user_id, AuthPath::HostBearer);
        engine
            .authorize_owner_write(&authz, owner, AccessKind::Fact)
            .await
            .map_err(|err| StorageError::Internal(err.to_string()))
    }

    async fn load_embedding_versions(
        pool: &sqlx::PgPool,
        entity_kind: EntityKind,
        entity_id: Uuid,
        model_id: &str,
    ) -> Result<Vec<i32>, sqlx::Error> {
        sqlx::query_scalar(
            "SELECT embedding_version
               FROM proxima_core.embeddings
              WHERE entity_kind = $1
                AND entity_id = $2
                AND model_id = $3
              ORDER BY embedding_version",
        )
        .bind(entity_kind)
        .bind(entity_id)
        .bind(model_id)
        .fetch_all(pool)
        .await
    }

    async fn load_embedding_head_version(
        pool: &sqlx::PgPool,
        entity_kind: EntityKind,
        entity_id: Uuid,
        model_id: &str,
    ) -> Result<Option<i32>, sqlx::Error> {
        sqlx::query_scalar(
            "SELECT embedding_version
               FROM proxima_core.embedding_heads
              WHERE entity_kind = $1
                AND entity_id = $2
                AND model_id = $3",
        )
        .bind(entity_kind)
        .bind(entity_id)
        .bind(model_id)
        .fetch_optional(pool)
        .await
    }

    async fn count_fact_embeddings(
        pool: &sqlx::PgPool,
        memory_id: proxima_core::MemoryId,
    ) -> Result<i64, sqlx::Error> {
        sqlx::query_scalar(
            "SELECT count(*)::bigint
               FROM proxima_core.embeddings
              WHERE entity_kind = 'Fact'
                AND entity_id = $1
                AND model_id = 'stub-fact-embed'",
        )
        .bind(memory_id.into_inner())
        .fetch_one(pool)
        .await
    }

    async fn insert_goal_for_embedding(
        pool: &sqlx::PgPool,
        owner: &Owner,
        goal_id: Uuid,
    ) -> Result<GoalId, sqlx::Error> {
        let (owner_kind, owner_id) = owner.columns();
        sqlx::query(
            "INSERT INTO proxima_core.goals
                (goal_id, owner_kind, owner_id, schema_id, schema_version,
                 title, text, payload, state, authorship_kind, request_id,
                 idempotency_key)
             VALUES ($1, $2, $3, 'proxima-test/goal-embedding-v1', 1,
                     'Embedding goal', 'Embedding goal text', $4,
                     'Active', 'User', $5, $6)",
        )
        .bind(goal_id)
        .bind(owner_kind)
        .bind(owner_id)
        .bind(br#"{"goal":true}"#.to_vec())
        .bind(format!("goal-embedding:{goal_id}"))
        .bind(format!("goal-embedding:{goal_id}"))
        .execute(pool)
        .await?;
        Ok(GoalId::new(goal_id))
    }

    #[tokio::test]
    async fn concurrent_reembedding_allocates_contiguous_versions()
    -> Result<(), Box<dyn std::error::Error>> {
        let (pg, db_name) = fresh_pg("proxima_spg_embed").await;
        let result: Result<(), Box<dyn std::error::Error>> = async {
            let owner = owner_fixture();
            let permit = owner_fact_write_permit(&owner).await?;
            let outcome = pg
                .ingest_fact_atomic(&permit, &fact_draft("concurrent embedding fact"), None)
                .await?;
            let pool_a = pg.pool_for_tests().clone();
            let pool_b = pg.pool_for_tests().clone();
            let owner_a = owner;
            let owner_b = owner;
            let memory_id = outcome.memory_id;
            let first_vec = padded_embedding([1.0, 0.0, 0.0]);
            let second_vec = padded_embedding([0.0, 1.0, 0.0]);
            let (first, second) = tokio::try_join!(
                async move {
                    let mut tx = pool_a.begin().await.map_err(|err| {
                        StorageError::Internal(format!("begin embedding insert tx: {err}"))
                    })?;
                    let outcome = insert_memory_embedding(
                        &mut tx,
                        &owner_a,
                        EntityKind::Fact,
                        memory_id,
                        "stub-fact-embed",
                        EMBEDDING_DIM,
                        &first_vec,
                    )
                    .await?;
                    tx.commit().await.map_err(|err| {
                        StorageError::Internal(format!("commit embedding insert tx: {err}"))
                    })?;
                    Ok::<_, StorageError>(outcome)
                },
                async move {
                    let mut tx = pool_b.begin().await.map_err(|err| {
                        StorageError::Internal(format!("begin embedding insert tx: {err}"))
                    })?;
                    let outcome = insert_memory_embedding(
                        &mut tx,
                        &owner_b,
                        EntityKind::Fact,
                        memory_id,
                        "stub-fact-embed",
                        EMBEDDING_DIM,
                        &second_vec,
                    )
                    .await?;
                    tx.commit().await.map_err(|err| {
                        StorageError::Internal(format!("commit embedding insert tx: {err}"))
                    })?;
                    Ok::<_, StorageError>(outcome)
                }
            )?;
            let mut outcome_versions = vec![first.embedding_version, second.embedding_version];
            outcome_versions.sort_unstable();
            assert_eq!(outcome_versions, vec![1, 2]);
            assert_eq!(
                load_embedding_versions(
                    pg.pool_for_tests(),
                    EntityKind::Fact,
                    memory_id.into_inner(),
                    "stub-fact-embed",
                )
                .await?,
                vec![1, 2]
            );
            assert_eq!(
                load_embedding_head_version(
                    pg.pool_for_tests(),
                    EntityKind::Fact,
                    memory_id.into_inner(),
                    "stub-fact-embed",
                )
                .await?,
                Some(2)
            );
            Ok(())
        }
        .await;
        drop(pg);
        drop_db(&db_name).await?;
        result
    }

    #[tokio::test]
    async fn goal_embedding_uses_goal_id_not_memory_id() -> Result<(), Box<dyn std::error::Error>> {
        let (pg, db_name) = fresh_pg("proxima_spg_embed").await;
        let result: Result<(), Box<dyn std::error::Error>> = async {
            let owner = owner_fixture();
            let goal_uuid = Uuid::now_v7();
            let goal_id = insert_goal_for_embedding(pg.pool_for_tests(), &owner, goal_uuid).await?;
            let mut tx = pg.pool_for_tests().begin().await?;
            let outcome = insert_embedding(
                &mut tx,
                &owner,
                EmbeddableEntityRef::Goal(goal_id),
                "stub-fact-embed",
                EMBEDDING_DIM,
                &padded_embedding([0.25, 0.5, 0.75]),
            )
            .await?;
            tx.commit().await?;

            assert_eq!(outcome.embedding_version, 1);
            assert_eq!(
                load_embedding_versions(
                    pg.pool_for_tests(),
                    EntityKind::Goal,
                    goal_uuid,
                    "stub-fact-embed",
                )
                .await?,
                vec![1]
            );
            assert_eq!(
                load_embedding_head_version(
                    pg.pool_for_tests(),
                    EntityKind::Goal,
                    goal_uuid,
                    "stub-fact-embed",
                )
                .await?,
                Some(1)
            );
            let memory_rows: i64 = sqlx::query_scalar(
                "SELECT count(*)::bigint FROM proxima_core.memories WHERE memory_id = $1",
            )
            .bind(goal_uuid)
            .fetch_one(pg.pool_for_tests())
            .await?;
            assert_eq!(
                memory_rows, 0,
                "goal embedding validation must not use memories"
            );
            Ok(())
        }
        .await;
        drop(pg);
        drop_db(&db_name).await?;
        result
    }

    #[tokio::test]
    async fn insert_memory_embedding_noops_after_source_memory_deleted()
    -> Result<(), Box<dyn std::error::Error>> {
        let (pg, db_name) = fresh_pg("proxima_spg_embed").await;
        let result: Result<(), Box<dyn std::error::Error>> = async {
            let owner = owner_fixture();
            let permit = owner_fact_write_permit(&owner).await?;
            let outcome = pg
                .ingest_fact_atomic(
                    &permit,
                    &fact_draft("deleted before embedding write"),
                    Some("stub-fact-embed"),
                )
                .await?;
            let claims =
                claim_pending_embedding_jobs(pg.pool_for_tests(), "stub-fact-embed", 1).await?;
            assert_eq!(claims.len(), 1);
            assert_eq!(claims[0].entity_id, outcome.memory_id);
            assert_eq!(
                load_embedding_text(
                    pg.pool_for_tests(),
                    &owner,
                    EntityKind::Fact,
                    outcome.memory_id
                )
                .await?,
                Some("deleted before embedding write".to_string()),
            );

            sqlx::query(
                "DELETE FROM proxima_core.embedding_jobs
                  WHERE entity_kind = 'Fact'
                    AND entity_id = $1",
            )
            .bind(outcome.memory_id.into_inner())
            .execute(pg.pool_for_tests())
            .await?;
            sqlx::query("DELETE FROM proxima_core.memories WHERE memory_id = $1")
                .bind(outcome.memory_id.into_inner())
                .execute(pg.pool_for_tests())
                .await?;

            let embedding = vec![0.125; EMBEDDING_DIM];
            let mut tx = pg.pool_for_tests().begin().await?;
            insert_memory_embedding(
                &mut tx,
                &owner,
                EntityKind::Fact,
                outcome.memory_id,
                "stub-fact-embed",
                EMBEDDING_DIM,
                &embedding,
            )
            .await?;
            tx.commit().await?;

            assert_eq!(
                load_embedding_text(
                    pg.pool_for_tests(),
                    &owner,
                    EntityKind::Fact,
                    outcome.memory_id
                )
                .await?,
                None,
            );
            assert_eq!(
                count_fact_embeddings(pg.pool_for_tests(), outcome.memory_id).await?,
                0
            );
            Ok(())
        }
        .await;
        drop(pg);
        drop_db(&db_name).await?;
        result
    }
}
