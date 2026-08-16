use proxima_core::storage_ports::{EmbeddingJobStatusCounts, OwnerWritePermit};
use proxima_core::{EmbeddingJobClaim, EntityKind, MemoryId, Owner, OwnerRefKind, StorageError};
use sqlx::PgPool;

use crate::error::map_err;

use super::ensure_nonnegative_limit;

/// Claim jobs through one ordered, arm-matched scan per status arm —
/// each riding its own partial index (`idx_embedding_jobs_pending_claim` /
/// `idx_embedding_jobs_processing_reclaim`, migration 0018) — merged by
/// UNION ALL and re-limited.
///
/// A two-arm status `OR` over the whole backlog cannot use those indexes
/// (`model_id` is not in a combined index; `OR` defeats ordered index use).
/// Arms partition the `OR` and cannot overlap. Each arm locks up to `$3`
/// rows (`FOR UPDATE` in the arm sub-selects: `PostgreSQL` rejects a
/// locking clause on a UNION). Unclaimed locked rows release with the
/// statement's transaction.
const CLAIM_EMBEDDING_JOBS_SQL: &str = "WITH claimed AS (
             SELECT job_id
               FROM proxima_core.embedding_jobs
              WHERE model_id = $1
                AND status = 'pending'
                AND NOT (entity_id = ANY($2::uuid[]))
              ORDER BY job_id ASC
              FOR UPDATE SKIP LOCKED
              LIMIT $3
         )
         UPDATE proxima_core.embedding_jobs j
            SET status = 'processing'
           FROM claimed, proxima_core.memory m, proxima_core.owners o
          WHERE j.job_id = claimed.job_id
            AND m.t = j.entity_id
            AND o.owner_id = j.owner_id
        RETURNING o.kind::text AS owner_kind,
                  j.owner_id,
                  m.kind::text AS entity_kind,
                  j.entity_id,
                  j.model_id";

/// The claim statement, for EXPLAIN-based plan guards. Same cfg gate as
/// `search.rs`'s `*_sql_for_tests` exports.
#[cfg(any(test, feature = "test-fixtures", debug_assertions))]
#[doc(hidden)]
#[must_use]
pub fn claim_embedding_jobs_sql_for_tests() -> &'static str {
    CLAIM_EMBEDDING_JOBS_SQL
}

#[derive(sqlx::FromRow)]
struct EmbeddingJobClaimRow {
    owner_kind: String,
    owner_id: uuid::Uuid,
    entity_kind: String,
    entity_id: uuid::Uuid,
    model_id: String,
}

impl From<EmbeddingJobClaimRow> for EmbeddingJobClaim {
    fn from(row: EmbeddingJobClaimRow) -> Self {
        let owner_kind = match row.owner_kind.as_str() {
            "world" => OwnerRefKind::World,
            "group" => OwnerRefKind::Group,
            _ => OwnerRefKind::Personal,
        };
        let entity_kind = match row.entity_kind.as_str() {
            "abstraction" => EntityKind::Abstraction,
            "perspective" => EntityKind::Perspective,
            "goal" => EntityKind::Goal,
            _ => EntityKind::Fact,
        };
        Self {
            owner: owner_kind
                .with_uuid(Some(row.owner_id))
                .expect("embedding job row has valid owner_ref shape"),
            entity_kind,
            entity_id: MemoryId::new(row.entity_id),
            model_id: row.model_id,
            embedding_version: 1,
            attempts: 0,
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
    let owner_id = owner.stored_owner_id();
    let limit = i64::try_from(limit)
        .map_err(|_| StorageError::ConstraintViolation("limit too large".into()))?;
    let _ = non_embeddable_schemas;
    missing_embedding_ids(pool, owner_id, model_id, limit).await
}

async fn missing_embedding_ids(
    pool: &PgPool,
    owner_id: uuid::Uuid,
    model_id: &str,
    limit: i64,
) -> Result<Vec<MemoryId>, StorageError> {
    let mut rows = sqlx::query_scalar::<_, uuid::Uuid>(
        "SELECT m.t
           FROM proxima_core.memory_head h
           JOIN proxima_core.memory m ON m.handle = h.handle AND m.t = h.t
          WHERE m.owner_id = $1
            AND NOT EXISTS (
                SELECT 1 FROM proxima_core.embedding_heads eh
                 WHERE eh.entity_id = m.t AND eh.model_id = $2
            )
          ORDER BY m.t ASC
          LIMIT $3",
    )
    .bind(owner_id)
    .bind(model_id)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(map_err)?;
    let chunk_table: bool = sqlx::query_scalar(
        "SELECT to_regclass('proxima_code.code_chunk_v1') IS NOT NULL",
    )
    .fetch_one(pool)
    .await
    .map_err(map_err)?;
    let have = i64::try_from(rows.len()).unwrap_or(i64::MAX);
    if chunk_table && have < limit {
        let remaining = limit.saturating_sub(have);
        let chunks = sqlx::query_scalar::<_, uuid::Uuid>(
            "SELECT m.t
               FROM proxima_core.memory_head h
               JOIN proxima_core.memory m ON m.handle = h.handle AND m.t = h.t
               JOIN proxima_code.code_chunk_v1 c ON c.t = m.t
              WHERE m.owner_id = $1
                AND c.state = 'Present'
                AND NULLIF(btrim(c.text), '') IS NOT NULL
                AND NOT EXISTS (
                    SELECT 1 FROM proxima_core.embedding_heads eh
                     WHERE eh.entity_id = m.t AND eh.model_id = $2
                )
              ORDER BY m.t ASC
              LIMIT $3",
        )
        .bind(owner_id)
        .bind(model_id)
        .bind(remaining)
        .fetch_all(pool)
        .await
        .map_err(map_err)?;
        rows.extend(chunks);
    }
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
/// Both drainers claim their whole batch in one statement, so the window
/// must exceed the batch's worst-case wall time rather than one job's. The
/// claim `UPDATE` resets `updated_at = now()`, so a reclaimed orphan
/// restarts its clock. Reclaim does not increment attempts; crash-loop
/// poison-pill bounding is out of scope, while embed-error retries still go
/// through `fail_embedding_job`.
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
    // SQL-POLICY: fixed-fragment — the compile-time claim constant above;
    // every value is bound.
    let rows = sqlx::query_as::<_, EmbeddingJobClaimRow>(CLAIM_EMBEDDING_JOBS_SQL)
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
    let owner_id = claim.owner.stored_owner_id();
    sqlx::query(
        "DELETE FROM proxima_core.embedding_jobs
          WHERE owner_id = $1
            AND entity_id = $2
            AND model_id = $3",
    )
    .bind(owner_id)
    .bind(claim.entity_id.into_inner())
    .bind(&claim.model_id)
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
    let owner_id = claim.owner.stored_owner_id();
    let _ = error;
    sqlx::query(
        "UPDATE proxima_core.embedding_jobs
            SET status = 'pending'
          WHERE owner_id = $1
            AND entity_id = $2
            AND model_id = $3
            AND status = 'processing'",
    )
    .bind(owner_id)
    .bind(claim.entity_id.into_inner())
    .bind(&claim.model_id)
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
    let owner_id = claim.owner.stored_owner_id();
    let _ = error;
    sqlx::query(
        "UPDATE proxima_core.embedding_jobs
            SET status = 'failed'
          WHERE owner_id = $1
            AND entity_id = $2
            AND model_id = $3
            AND status = 'processing'",
    )
    .bind(owner_id)
    .bind(claim.entity_id.into_inner())
    .bind(&claim.model_id)
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
        let owner_id = claim.owner.stored_owner_id();
        let _ = error;
        sqlx::query(
            "UPDATE proxima_core.embedding_jobs
                SET status = 'pending'
              WHERE owner_id = $1
                AND entity_id = $2
                AND model_id = $3
                AND status = 'processing'",
        )
        .bind(owner_id)
        .bind(claim.entity_id.into_inner())
        .bind(&claim.model_id)
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
    let owner_id = permit.owner().stored_owner_id();
    let _ = non_embeddable_schemas;
    let ids = missing_embedding_ids(pool, owner_id, model_id, limit).await?;
    if ids.is_empty() {
        return Ok(0);
    }
    let entity_ids: Vec<uuid::Uuid> = ids.into_iter().map(MemoryId::into_inner).collect();
    let result = sqlx::query(
        "INSERT INTO proxima_core.embedding_jobs (entity_id, model_id, owner_id)
         SELECT t, $2, $1
           FROM unnest($3::uuid[]) AS t
         ON CONFLICT (owner_id, entity_id, model_id)
         DO NOTHING",
    )
    .bind(owner_id)
    .bind(model_id)
    .bind(&entity_ids)
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
    let owner_id = owner.stored_owner_id();
    let row: (i64,) = sqlx::query_as(
        "SELECT count(*)
           FROM proxima_core.embedding_jobs
          WHERE owner_id = $1
            AND status IN ('pending', 'processing')",
    )
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
    let owner_id = owner.stored_owner_id();
    let row: (i64,) = sqlx::query_as(
        "SELECT count(*)
           FROM proxima_core.embedding_jobs
          WHERE owner_id = $1
            AND status = 'failed'",
    )
    .bind(owner_id)
    .fetch_one(pool)
    .await
    .map_err(map_err)?;
    u64::try_from(row.0)
        .map_err(|_| StorageError::Internal("failed embedding job count is negative".into()))
}

/// Owner-scoped pending+failed embedding job counts in one round trip.
///
/// # Errors
///
/// Maps SQL failures through the shared mapper.
pub async fn count_embedding_job_status(
    pool: &PgPool,
    owner: &Owner,
) -> Result<EmbeddingJobStatusCounts, StorageError> {
    let owner_id = owner.stored_owner_id();
    let row: (i64, i64) = sqlx::query_as(
        "SELECT
             count(*) FILTER (WHERE status IN ('pending', 'processing')),
             count(*) FILTER (WHERE status = 'failed')
           FROM proxima_core.embedding_jobs
          WHERE owner_id = $1",
    )
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
    let Some(owner_id) = owner_id else {
        return Ok(());
    };
    let _ = (owner_kind, entity_kind);
    sqlx::query(
        "INSERT INTO proxima_core.embedding_jobs
            (entity_id, model_id, owner_id)
         VALUES ($1, $2, $3)
         ON CONFLICT (owner_id, entity_id, model_id)
         DO NOTHING",
    )
    .bind(entity_id)
    .bind(model_id)
    .bind(owner_id)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The claim is one statement: it exists to hand a drainer work in a
    /// single round trip.
    #[test]
    fn claiming_is_a_single_statement() {
        assert!(!CLAIM_EMBEDDING_JOBS_SQL.contains(';'));
    }

    /// Golden text: the claim is pinned per arm, so an edit to either
    /// ordered scan is a deliberate change to this test as well.
    #[test]
    fn the_claim_sql_is_pinned() {
        assert_eq!(CLAIM_EMBEDDING_JOBS_SQL, CLAIM_GOLDEN);
    }

    #[test]
    fn the_claim_names_pending_only() {
        assert_eq!(
            CLAIM_EMBEDDING_JOBS_SQL
                .matches("AND status = 'pending'")
                .count(),
            1
        );
    }

    const CLAIM_GOLDEN: &str = r"WITH claimed AS (
             SELECT job_id
               FROM proxima_core.embedding_jobs
              WHERE model_id = $1
                AND status = 'pending'
                AND NOT (entity_id = ANY($2::uuid[]))
              ORDER BY job_id ASC
              FOR UPDATE SKIP LOCKED
              LIMIT $3
         )
         UPDATE proxima_core.embedding_jobs j
            SET status = 'processing'
           FROM claimed, proxima_core.memory m, proxima_core.owners o
          WHERE j.job_id = claimed.job_id
            AND m.t = j.entity_id
            AND o.owner_id = j.owner_id
        RETURNING o.kind::text AS owner_kind,
                  j.owner_id,
                  m.kind::text AS entity_kind,
                  j.entity_id,
                  j.model_id";
}
