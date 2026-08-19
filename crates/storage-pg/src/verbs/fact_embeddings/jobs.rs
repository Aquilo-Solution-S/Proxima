use proxima_core::storage_ports::{EmbeddingJobStatusCounts, OwnerWritePermit};
use proxima_core::{EmbeddingJobClaim, EntityKind, MemoryId, Owner, OwnerRefKind, StorageError};
use sqlx::PgPool;

use crate::error::map_err;

use super::ensure_nonnegative_limit;

/// Claim pending jobs for one model, ordered by `job_id`.
///
/// One arm: `status = 'pending'`. Rides
/// `embedding_jobs_pending_claim_idx (model_id, job_id) WHERE status =
/// 'pending'`. Locked unclaimed rows release with the statement's
/// transaction. `claimed_at` is what makes a crashed drainer's row
/// recoverable ([`reclaim_stale_embedding_jobs`]); there is no
/// `next_attempt_at` column. Each claim also gets a fencing token so a
/// reclaimed worker cannot complete a successor's claim. The table's unique
/// `(owner_id, entity_id, model_id)` key guarantees at most one job for an
/// entity in this model; callers never need an invocation-sized exclusion
/// list to prevent duplicate work.
const CLAIM_EMBEDDING_JOBS_SQL: &str = "WITH claimed AS (
             SELECT job_id
               FROM proxima_core.embedding_jobs
              WHERE model_id = $1
                AND status = 'pending'
              ORDER BY job_id ASC
              FOR UPDATE SKIP LOCKED
              LIMIT $2
         )
         UPDATE proxima_core.embedding_jobs j
            SET status = 'processing',
                claimed_at = now(),
                claim_token = uuidv7()
           FROM claimed, proxima_core.memory m, proxima_core.owners o
          WHERE j.job_id = claimed.job_id
            AND m.t = j.entity_id
            AND o.owner_id = j.owner_id
        RETURNING o.kind::text AS owner_kind,
                  j.job_id,
                  j.owner_id,
                  m.kind::text AS entity_kind,
                  j.entity_id,
                  j.model_id,
                  j.claim_token";

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
    job_id: uuid::Uuid,
    owner_id: uuid::Uuid,
    entity_kind: String,
    entity_id: uuid::Uuid,
    model_id: String,
    claim_token: uuid::Uuid,
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
            job_id: row.job_id,
            owner: owner_kind
                .with_uuid(Some(row.owner_id))
                .expect("embedding job row has valid owner_ref shape"),
            entity_kind,
            entity_id: MemoryId::new(row.entity_id),
            model_id: row.model_id,
            embedding_version: 1,
            attempts: 0,
            claim_token: row.claim_token,
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
    missing_embedding_ids(pool, owner_id, model_id, limit, non_embeddable_schemas).await
}

async fn missing_embedding_ids(
    pool: &PgPool,
    owner_id: uuid::Uuid,
    model_id: &str,
    limit: i64,
    non_embeddable_schemas: &[String],
) -> Result<Vec<MemoryId>, StorageError> {
    // Chunks are memory rows. A second arm against code_chunk_v1 is a
    // subset of this anti-join and used to duplicate t.
    let rows = sqlx::query_scalar::<_, uuid::Uuid>(
        "SELECT m.t
           FROM proxima_core.memory_head h
           JOIN proxima_core.memory m ON m.handle = h.handle AND m.t = h.t
          WHERE m.owner_id = $1
            AND NOT (m.schema_id = ANY($4::text[]))
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
    .bind(non_embeddable_schemas)
    .fetch_all(pool)
    .await
    .map_err(map_err)?;
    Ok(rows.into_iter().map(MemoryId::new).collect())
}

/// Atomically claim pending embedding jobs for one model.
///
/// Selects `status = 'pending'` rows for `$1`, `FOR UPDATE SKIP LOCKED`, then
/// sets `processing` and stamps `claimed_at`. v0.0.8 has no
/// `next_attempt_at`; a claim a drainer never finishes is recovered by
/// [`reclaim_stale_embedding_jobs`].
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
    let limit = ensure_nonnegative_limit(limit)?;
    if limit == 0 {
        return Ok(Vec::new());
    }
    // SQL-POLICY: fixed-fragment — the compile-time claim constant above;
    // every value is bound.
    let rows = sqlx::query_as::<_, EmbeddingJobClaimRow>(CLAIM_EMBEDDING_JOBS_SQL)
        .bind(model_id)
        .bind(limit)
        .fetch_all(pool)
        .await
        .map_err(map_err)?;
    Ok(rows.into_iter().map(EmbeddingJobClaim::from).collect())
}

/// Delete a completed embedding job, fenced by the claim token.
///
/// # Errors
///
/// Returns `Conflict` for a stale/non-processing claim; maps SQL failures
/// through the shared mapper.
pub async fn complete_embedding_job(
    pool: &PgPool,
    claim: &EmbeddingJobClaim,
) -> Result<(), StorageError> {
    let result = sqlx::query(
        "DELETE FROM proxima_core.embedding_jobs
          WHERE job_id = $1
            AND claim_token = $2
            AND status = 'processing'",
    )
    .bind(claim.job_id)
    .bind(claim.claim_token)
    .execute(pool)
    .await
    .map_err(map_err)?;
    if result.rows_affected() == 0 {
        return Err(StorageError::Conflict(
            "embedding job claim is stale or no longer processing".into(),
        ));
    }
    Ok(())
}

/// Refresh the lease timestamp for token-matching processing claims.
///
/// Missing claims are skipped rather than treated as a conflict: a batch
/// heartbeat includes claims that earlier steps may already have completed.
/// Claim-token fencing still prevents an old drainer from renewing or
/// mutating a successor claim.
///
/// # Errors
///
/// Maps SQL failures through the shared mapper.
pub async fn renew_embedding_jobs(
    pool: &PgPool,
    claims: &[EmbeddingJobClaim],
) -> Result<u64, StorageError> {
    if claims.is_empty() {
        return Ok(0);
    }
    let job_ids: Vec<uuid::Uuid> = claims.iter().map(|claim| claim.job_id).collect();
    let claim_tokens: Vec<uuid::Uuid> = claims.iter().map(|claim| claim.claim_token).collect();
    let result = sqlx::query(
        "UPDATE proxima_core.embedding_jobs j
            SET claimed_at = now()
           FROM unnest($1::uuid[], $2::uuid[]) AS claim(job_id, claim_token)
          WHERE j.job_id = claim.job_id
            AND j.claim_token = claim.claim_token
            AND j.status = 'processing'",
    )
    .bind(&job_ids)
    .bind(&claim_tokens)
    .execute(pool)
    .await
    .map_err(map_err)?;
    Ok(result.rows_affected())
}

/// Fail an attempted job for a retryable cause: `failed`, with `error`
/// kept on the row.
///
/// v0.0.8 has no attempt counter or `next_attempt_at`, so `failed` is the
/// retry dead-end that `reconcile_embeddings` lifts a memory out of —
/// requeueing here instead would spin a broken provider at the drain
/// loop's interval with nothing recording why. Permanent rejection uses
/// [`fail_embedding_job_permanently`]; a claimed-but-unattempted job uses
/// [`release_embedding_jobs`].
///
/// # Errors
///
/// Returns `Conflict` for a stale/non-processing claim; maps SQL failures
/// through the shared mapper.
pub async fn fail_embedding_job(
    pool: &PgPool,
    claim: &EmbeddingJobClaim,
    error: &str,
) -> Result<(), StorageError> {
    let result = sqlx::query(
        "UPDATE proxima_core.embedding_jobs
            SET status = 'failed',
                claimed_at = NULL,
                claim_token = NULL,
                last_error = $3
          WHERE job_id = $1
            AND claim_token = $2
            AND status = 'processing'",
    )
    .bind(claim.job_id)
    .bind(claim.claim_token)
    .bind(error)
    .execute(pool)
    .await
    .map_err(map_err)?;
    if result.rows_affected() == 0 {
        return Err(StorageError::Conflict(
            "embedding job claim is stale or no longer processing".into(),
        ));
    }
    Ok(())
}

/// Terminally fail a job whose input the provider rejects for a permanent
/// cause (e.g. over the embedding model's token limit): `failed_permanent`,
/// with `error` kept on the row.
///
/// The separate status — not a marker string inside `last_error` — is what
/// keeps `reconcile_embeddings` from cycling the job reject-retry forever:
/// its requeue arm names `failed` only.
///
/// # Errors
///
/// Returns `Conflict` for a stale/non-processing claim; maps SQL failures
/// through the shared mapper.
pub async fn fail_embedding_job_permanently(
    pool: &PgPool,
    claim: &EmbeddingJobClaim,
    error: &str,
) -> Result<(), StorageError> {
    let result = sqlx::query(
        "UPDATE proxima_core.embedding_jobs
            SET status = 'failed_permanent',
                claimed_at = NULL,
                claim_token = NULL,
                last_error = $3
          WHERE job_id = $1
            AND claim_token = $2
            AND status = 'processing'",
    )
    .bind(claim.job_id)
    .bind(claim.claim_token)
    .bind(error)
    .execute(pool)
    .await
    .map_err(map_err)?;
    if result.rows_affected() == 0 {
        return Err(StorageError::Conflict(
            "embedding job claim is stale or no longer processing".into(),
        ));
    }
    Ok(())
}

/// Return claimed-but-unattempted jobs to `pending`.
///
/// Batch-drain uses this when one provider call covering many jobs
/// fails for a transient cause. Nothing was tried, so the rows are
/// immediately claimable again; `error` records why they were let go.
///
/// # Errors
///
/// Returns `Conflict` when any claim is stale/non-processing; maps SQL
/// failures through the shared mapper. The batch is atomic.
pub async fn release_embedding_jobs(
    pool: &PgPool,
    claims: &[EmbeddingJobClaim],
    error: &str,
) -> Result<(), StorageError> {
    let mut tx = pool.begin().await.map_err(map_err)?;
    for claim in claims {
        let result = sqlx::query(
            "UPDATE proxima_core.embedding_jobs
                SET status = 'pending',
                    claimed_at = NULL,
                    claim_token = NULL,
                    last_error = $3
              WHERE job_id = $1
                AND claim_token = $2
                AND status = 'processing'",
        )
        .bind(claim.job_id)
        .bind(claim.claim_token)
        .bind(error)
        .execute(&mut *tx)
        .await
        .map_err(map_err)?;
        if result.rows_affected() == 0 {
            return Err(StorageError::Conflict(
                "embedding job claim is stale or no longer processing".into(),
            ));
        }
    }
    tx.commit().await.map_err(map_err)?;
    Ok(())
}

/// Return `processing` jobs claimed more than `older_than_seconds` ago to
/// `pending`.
///
/// The one recovery path for a drainer that died holding a claim. Rows with
/// no `claimed_at` at all are stale by definition — nothing can date them,
/// so nothing else can ever free them.
///
/// # Errors
///
/// Returns `ConstraintViolation` for a non-positive window, otherwise maps
/// SQL failures through the shared mapper.
pub async fn reclaim_stale_embedding_jobs(
    pool: &PgPool,
    older_than_seconds: i64,
) -> Result<u64, StorageError> {
    if older_than_seconds < 1 {
        return Err(StorageError::ConstraintViolation(
            "stale processing reclaim window must be positive".into(),
        ));
    }
    let result = sqlx::query(
        "UPDATE proxima_core.embedding_jobs
            SET status = 'pending',
                claimed_at = NULL,
                claim_token = NULL
          WHERE status = 'processing'
            AND (
                claimed_at IS NULL
                OR claimed_at < now()
                    - make_interval(secs => ($1::bigint)::double precision)
            )",
    )
    .bind(older_than_seconds)
    .execute(pool)
    .await
    .map_err(map_err)?;
    Ok(result.rows_affected())
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
    let ids =
        missing_embedding_ids(pool, owner_id, model_id, limit, non_embeddable_schemas).await?;
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

/// Owner-scoped count of embedding jobs in a terminal state — both the
/// requeueable `failed` and the permanently rejected `failed_permanent`.
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
            AND status IN ('failed', 'failed_permanent')",
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
             count(*) FILTER (WHERE status IN ('failed', 'failed_permanent'))
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
/// Idempotent on the table's natural key `(owner_id, entity_id, model_id)`,
/// which is why a replayed write and a re-enqueued deferral are both free.
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
    fn missing_embedding_scan_does_not_probe_flavor_tables() {
        let src = include_str!("jobs.rs");
        let needle = format!("{}{}", "to_reg", "class(");
        assert!(
            !src.contains(&needle),
            "chunks are memory rows; do not dual-scan proxima_code"
        );
    }

    /// `reclaim_stale_embedding_jobs` can only date a claim the claim
    /// itself stamped.
    #[test]
    fn the_claim_stamps_claimed_at() {
        assert!(CLAIM_EMBEDDING_JOBS_SQL.contains("claimed_at = now()"));
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

    #[test]
    fn claim_needs_no_growing_entity_exclusion_parameter() {
        let migration = include_str!("../../../migrations/0001_v008.sql");
        assert!(
            migration.contains("UNIQUE (owner_id, entity_id, model_id)"),
            "the DB must admit at most one job per entity and model"
        );
        assert!(!CLAIM_EMBEDDING_JOBS_SQL.contains("ANY("));
        assert!(!CLAIM_EMBEDDING_JOBS_SQL.contains("$3"));
    }

    const CLAIM_GOLDEN: &str = r"WITH claimed AS (
             SELECT job_id
               FROM proxima_core.embedding_jobs
              WHERE model_id = $1
                AND status = 'pending'
              ORDER BY job_id ASC
              FOR UPDATE SKIP LOCKED
              LIMIT $2
         )
         UPDATE proxima_core.embedding_jobs j
            SET status = 'processing',
                claimed_at = now(),
                claim_token = uuidv7()
           FROM claimed, proxima_core.memory m, proxima_core.owners o
          WHERE j.job_id = claimed.job_id
            AND m.t = j.entity_id
            AND o.owner_id = j.owner_id
        RETURNING o.kind::text AS owner_kind,
                  j.job_id,
                  j.owner_id,
                  m.kind::text AS entity_kind,
                  j.entity_id,
                  j.model_id,
                  j.claim_token";
}
