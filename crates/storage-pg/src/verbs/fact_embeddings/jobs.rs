use proxima_core::llm::EMBEDDING_JOB_MAX_ATTEMPTS;
use proxima_core::storage_ports::{
    EmbeddingJobStatusCounts, OwnerWritePermit, PERMANENT_EMBED_FAILURE_MARKER,
};
use proxima_core::{EmbeddingJobClaim, EntityKind, MemoryId, Owner, OwnerRefKind, StorageError};
use sqlx::PgPool;

use crate::error::map_err;

use super::{ensure_nonnegative_limit, owner_parts};

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
    non_embeddable_schemas: &[String],
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
            -- Declined a vector rather than lacking one; see
            -- `FactPayload::EMBEDDABLE`. `<> ALL('{}')` is TRUE, so an
            -- empty list leaves the query exactly as it was.
            AND m.schema_id <> ALL($5::text[])
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
    .bind(non_embeddable_schemas)
    .fetch_all(pool)
    .await
    .map_err(map_err)?;
    Ok(rows.into_iter().map(MemoryId::new).collect())
}

/// Atomically claim pending Fact embedding jobs for one model.
///
/// A `pending` job is claimable only once `next_attempt_at` has elapsed
/// (`fail_embedding_job` stamps exponential backoff there), so a transient
/// provider outage no longer burns all attempts in a tight re-claim loop.
///
/// Stale `processing` jobs orphaned by a crashed or restarted drainer are
/// reclaimed after fifteen minutes regardless of backoff. The window MUST
/// exceed the embedding client's request timeout (the OpenAI-compat default,
/// `crates/llm-openai-compat/src/openai_compat.rs` `DEFAULT_EMBED_TIMEOUT`).
/// Inline drainers call
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

pub(super) async fn claim_pending_embedding_jobs_excluding(
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
                AND (
                     (status = 'pending'
                         AND (next_attempt_at IS NULL OR next_attempt_at <= now()))
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
/// Below the cap the job returns to `pending` with `next_attempt_at` set to an
/// exponential backoff (`30s * 2^attempts`), so the drainer stops re-claiming
/// it immediately; at the cap it settles to `failed` (see `reconcile_embeddings`
/// for the requeue path out of that terminal state).
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
                END,
                next_attempt_at = CASE
                    WHEN attempts + 1 >= $8
                    THEN next_attempt_at
                    ELSE now() + (interval '30 seconds' * power(2, attempts))
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

/// Terminally fail a job whose input the provider rejects for a permanent
/// cause (e.g. over the embedding model's token limit). Goes straight to
/// `failed` with a [`PERMANENT_EMBED_FAILURE_MARKER`]-prefixed
/// `last_error`; `reconcile_embeddings` skips marker-prefixed rows so the
/// job stays terminal instead of cycling reject-retry forever.
///
/// # Errors
///
/// Maps SQL failures through the shared mapper.
pub async fn fail_embedding_job_permanently(
    pool: &PgPool,
    claim: &EmbeddingJobClaim,
    error: &str,
) -> Result<(), StorageError> {
    let (owner_kind, owner_id) = owner_parts(&claim.owner);
    let marked = format!("{PERMANENT_EMBED_FAILURE_MARKER}{error}");
    sqlx::query(
        "UPDATE proxima_core.embedding_jobs
            SET attempts = attempts + 1,
                last_error = $7,
                updated_at = now(),
                status = 'failed'::proxima_core.embedding_job_status
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
    .bind(&marked)
    .execute(pool)
    .await
    .map_err(map_err)?;
    Ok(())
}

/// Return claimed-but-unattempted jobs to `pending` without incrementing
/// `attempts`. Batch-drain uses this when one provider call covering many
/// jobs fails for a transient cause: the failure is not evidence against
/// any individual job, so none should march toward the attempt cap. The
/// flat 30s `next_attempt_at` keeps concurrent drainers from immediately
/// re-claiming the same set.
///
/// # Errors
///
/// Maps SQL failures through the shared mapper.
pub async fn release_embedding_jobs(
    pool: &PgPool,
    claims: &[EmbeddingJobClaim],
    error: &str,
) -> Result<(), StorageError> {
    for claim in claims {
        let (owner_kind, owner_id) = owner_parts(&claim.owner);
        sqlx::query(
            "UPDATE proxima_core.embedding_jobs
                SET status = 'pending'::proxima_core.embedding_job_status,
                    last_error = $7,
                    updated_at = now(),
                    next_attempt_at = now() + interval '30 seconds'
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
        .execute(pool)
        .await
        .map_err(map_err)?;
    }
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
    non_embeddable_schemas: &[String],
) -> Result<u64, StorageError> {
    let limit = ensure_nonnegative_limit(limit)?;
    if limit == 0 {
        return Ok(0);
    }
    let (owner_kind, owner_id) = owner_parts(permit.owner());
    let result = sqlx::query(
        "WITH missing AS (
             SELECT m.memory_id,
                    COALESCE(m.kind, 'Fact'::proxima_core.entity_kind) AS entity_kind
               FROM proxima_core.memories m
              WHERE EXISTS (
                        SELECT 1
                          FROM (SELECT memory_id AS entity_id, owner_kind, owner_id FROM proxima_core.memories UNION ALL SELECT goal_id AS entity_id, owner_kind, owner_id FROM proxima_core.goals) eo
                         WHERE eo.entity_id = m.memory_id
                           AND eo.owner_kind = $1
                           AND eo.owner_id = $2
)
                -- Facts (kind IS NULL) plus derived memories. Derived rows
                -- belong here because a flavor can materialize Abstractions
                -- through its own sidecar path without an embedding client in
                -- scope — code-chunk ingest does exactly that — and those rows
                -- would otherwise stay unembedded until an operator ran a
                -- global reconcile. Matches the kinds `reconcile_embeddings`
                -- scans, owner-scoped.
                AND (
                    m.kind IS NULL
                    OR m.kind IN (
                        'Abstraction'::proxima_core.entity_kind,
                        'Perspective'::proxima_core.entity_kind
                    )
                )
                AND m.text IS NOT NULL
                -- See `FactPayload::EMBEDDABLE`. Gating only the inline
                -- write path would leave this call to re-enqueue every
                -- row that path skipped.
                AND m.schema_id <> ALL($5::text[])
                AND m.tombstoned_at IS NULL
                AND NOT EXISTS (
                    SELECT 1
                      FROM proxima_core.embedding_heads h
                     WHERE h.entity_kind
                           = COALESCE(m.kind, 'Fact'::proxima_core.entity_kind)
                       AND h.entity_id = m.memory_id
                       AND h.model_id = $3
                )
              ORDER BY m.created_at ASC, m.memory_id ASC
              LIMIT $4
         )
         INSERT INTO proxima_core.embedding_jobs
             (owner_kind, owner_id,
              entity_kind, entity_id, model_id)
         SELECT $1, $2, entity_kind, memory_id, $3
           FROM missing
         ON CONFLICT (owner_kind, owner_id,
                      entity_kind, entity_id, model_id, embedding_version)
         DO NOTHING",
    )
    .bind(owner_kind)
    .bind(owner_id)
    .bind(model_id)
    .bind(limit)
    .bind(non_embeddable_schemas)
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

/// Owner-scoped count of embedding jobs in the terminal `failed` state.
///
/// # Errors
///
/// Maps SQL failures through the shared mapper.
pub async fn count_failed_embedding_jobs(
    pool: &PgPool,
    owner: &Owner,
) -> Result<u64, StorageError> {
    let (owner_kind, owner_id) = owner_parts(owner);
    let row: (i64,) = sqlx::query_as(
        "SELECT count(*)
           FROM proxima_core.embedding_jobs
          WHERE owner_kind = $1
            AND owner_id = $2
            AND status = 'failed'",
    )
    .bind(owner_kind)
    .bind(owner_id)
    .fetch_one(pool)
    .await
    .map_err(map_err)?;
    u64::try_from(row.0)
        .map_err(|_| StorageError::Internal("failed embedding job count is negative".into()))
}

/// Owner-scoped pending+failed embedding job counts in a single round trip.
/// `get_graph_authorized` used to run [`count_pending_embedding_jobs`] and
/// [`count_failed_embedding_jobs`] strictly in series even though both read
/// `embedding_jobs` and differ only in the status predicate; this merges
/// them into one `count(*) FILTER (WHERE …)` query.
///
/// # Errors
///
/// Maps SQL failures through the shared mapper.
pub async fn count_embedding_job_status(
    pool: &PgPool,
    owner: &Owner,
) -> Result<EmbeddingJobStatusCounts, StorageError> {
    let (owner_kind, owner_id) = owner_parts(owner);
    let row: (i64, i64) = sqlx::query_as(
        "SELECT
             count(*) FILTER (WHERE status IN ('pending', 'processing')),
             count(*) FILTER (WHERE status = 'failed')
           FROM proxima_core.embedding_jobs
          WHERE owner_kind = $1
            AND owner_id = $2",
    )
    .bind(owner_kind)
    .bind(owner_id)
    .fetch_one(pool)
    .await
    .map_err(map_err)?;
    let pending = u64::try_from(row.0)
        .map_err(|_| StorageError::Internal("pending embedding job count is negative".into()))?;
    let failed = u64::try_from(row.1)
        .map_err(|_| StorageError::Internal("failed embedding job count is negative".into()))?;
    Ok(EmbeddingJobStatusCounts { pending, failed })
}

/// Enqueue one durable embedding job in the caller's transaction, so the
/// job row and the memory row land together or not at all.
///
/// Idempotent on the table's natural key `(owner, entity_kind, entity_id,
/// model_id, embedding_version)`, which is why a replayed write and a
/// re-enqueued deferral are both free.
///
/// `entity_kind` is deliberately a parameter rather than `Fact`: the
/// column is the full `proxima_core.entity_kind` enum, so an `Abstraction`
/// or `Perspective` job needs no schema change — only a caller. Facts have
/// enqueued here since ingest existed; derived memories acquired the same
/// rescue when an unembeddable text stopped meaning a failed write.
///
/// # Errors
///
/// Maps SQL failures through the shared mapper.
pub(crate) async fn enqueue_embedding_job_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    owner_kind: OwnerRefKind,
    owner_id: Option<uuid::Uuid>,
    entity_kind: EntityKind,
    entity_id: uuid::Uuid,
    model_id: &str,
) -> Result<(), StorageError> {
    sqlx::query(
        "INSERT INTO proxima_core.embedding_jobs
            (owner_kind, owner_id,
             entity_kind, entity_id, model_id)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (owner_kind, owner_id,
                      entity_kind, entity_id, model_id, embedding_version)
         DO NOTHING",
    )
    .bind(owner_kind)
    .bind(owner_id)
    .bind(entity_kind)
    .bind(entity_id)
    .bind(model_id)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(())
}
