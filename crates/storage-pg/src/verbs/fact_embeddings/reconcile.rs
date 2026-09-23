use proxima_core::StorageError;
use sqlx::PgPool;

use crate::error::map_err;

use super::{ensure_nonnegative_limit, reclaim_stale_embedding_jobs};

pub use proxima_core::{
    EmbeddingReconcileOptions, EmbeddingReconcileOutcome, EmbeddingReconcileScope,
};

const RECONCILE_EMBEDDINGS_SQL: &str = "
WITH scoped AS MATERIALIZED (
     SELECT m.t AS memory_id,
            m.owner_id,
            h2.embedding_version AS head_version
       FROM proxima_core.memory_head mh
       JOIN proxima_core.memory m ON m.handle = mh.handle AND m.t = mh.t
       LEFT JOIN proxima_core.embedding_heads h2
         ON h2.entity_id = m.t
        AND h2.model_id = $1
        AND h2.dim = $6
      WHERE ($3::text <> 'since'
             OR COALESCE(uuid_extract_timestamp(m.t), TIMESTAMPTZ '1970-01-01') >= $4)
        AND mh.schema_id <> ALL($5::text[])
 ),
 eligible AS MATERIALIZED (
     SELECT s.*
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
             WHERE j.owner_id = s.owner_id
               AND j.entity_id = s.memory_id
               AND j.model_id = $1
               AND j.dim = $6
               AND j.status IN ('pending', 'processing', 'failed_permanent')
        )
 ),
 limited AS MATERIALIZED (
     SELECT e.*
       FROM eligible e
       JOIN proxima_core.memory m ON m.t = e.memory_id
      ORDER BY e.memory_id ASC
      LIMIT $2
      FOR UPDATE OF m
 ),
 inserted AS (
     INSERT INTO proxima_core.embedding_jobs
         (entity_id, model_id, dim, owner_id)
     SELECT memory_id, $1, $6, owner_id
       FROM limited
     ON CONFLICT (owner_id, entity_id, model_id, dim)
         DO UPDATE SET status = 'pending',
                       claimed_at = NULL,
                       claim_token = NULL,
                       last_error = NULL
         WHERE embedding_jobs.status = 'failed'
     RETURNING 1
 )
 SELECT
     (SELECT count(*)::bigint FROM limited) AS scanned,
     (SELECT count(*)::bigint FROM inserted) AS enqueued";

/// Global reconciliation for embeddable memories.
///
/// Scans Facts plus derived memories with stored text, skips rows by
/// scope-specific embedding coverage and target-space durable jobs, and
/// enqueues via `proxima_core.embedding_jobs`. A row that already holds a
/// `failed` job (retryable cause, per `fail_embedding_job`) is requeued —
/// status back to `pending`, `last_error` cleared — so reconcile is the
/// operator/startup reset that lifts a Fact out of the retry dead-end.
/// `failed_permanent` jobs (`fail_embedding_job_permanently`) are NOT
/// requeued: the provider will always reject the same input again.
/// Live `pending`/`processing` jobs are left untouched, except that the pass
/// first reclaims claims older than the host's configured stale-claim timeout
/// — a `processing` row whose drainer died is the one backlog no enqueue can
/// reach, because the job's unique key is already taken.
///
/// # Errors
///
/// Returns `ConstraintViolation` for negative limits, otherwise maps SQL
/// failures through the shared mapper.
pub async fn reconcile_embeddings(
    pool: &PgPool,
    options: EmbeddingReconcileOptions<'_>,
    stale_claim_timeout_seconds: i64,
) -> Result<EmbeddingReconcileOutcome, StorageError> {
    reconcile_embeddings_with_platform(pool, None, options, stale_claim_timeout_seconds).await
}

pub(crate) async fn reconcile_embeddings_with_platform(
    pool: &PgPool,
    platform: Option<&crate::PgPlatformScope>,
    options: EmbeddingReconcileOptions<'_>,
    stale_claim_timeout_seconds: i64,
) -> Result<EmbeddingReconcileOutcome, StorageError> {
    let mut tx = crate::platform_scope::begin_platform_transaction(pool, platform).await?;
    let result =
        reconcile_embeddings_on_connection(tx.as_mut(), options, stale_claim_timeout_seconds).await;
    crate::owner_scope::finish_transaction(tx, result).await
}

async fn reconcile_embeddings_on_connection(
    pool: &mut sqlx::PgConnection,
    options: EmbeddingReconcileOptions<'_>,
    stale_claim_timeout_seconds: i64,
) -> Result<EmbeddingReconcileOutcome, StorageError> {
    let limit = resolve_reconcile_limit(options.limit)?;
    if limit == 0 {
        return Ok(EmbeddingReconcileOutcome::default());
    }

    let reclaimed = reclaim_stale_embedding_jobs(&mut *pool, stale_claim_timeout_seconds).await?;
    if reclaimed > 0 {
        tracing::warn!(
            reclaimed,
            stale_after_seconds = stale_claim_timeout_seconds,
            "reclaimed abandoned processing embedding jobs"
        );
    }

    let (scope, since) = match options.scope {
        EmbeddingReconcileScope::MissingOnly => ("missing_only", None),
        EmbeddingReconcileScope::IncludeStale => ("include_stale", None),
        EmbeddingReconcileScope::Since(since) => ("since", Some(since)),
    };

    let row: (i64, i64) = sqlx::query_as(RECONCILE_EMBEDDINGS_SQL)
        .bind(options.space.model_id())
        .bind(limit)
        .bind(scope)
        .bind(since)
        .bind(options.non_embeddable_schemas)
        .bind(crate::pgvector::Lane::of(options.space.dim()).width)
        .fetch_one(&mut *pool)
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

fn resolve_reconcile_limit(limit: Option<i64>) -> Result<i64, StorageError> {
    match limit {
        Some(limit) => ensure_nonnegative_limit(limit),
        None => Err(StorageError::ConstraintViolation(
            "reconcile limit is required".into(),
        )),
    }
}
