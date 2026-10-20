use proxima_core::{OwnerId, StorageError};
use sqlx::PgPool;

use crate::error::map_err;

use super::{ensure_nonnegative_limit, reclaim_stale_embedding_jobs};

pub use proxima_core::{
    EmbeddingReconcileOptions, EmbeddingReconcileOutcome, EmbeddingReconcileScope,
};

/// Memories one reconcile step reads: the page the statement is bounded to.
const RECONCILE_PAGE: i64 = 2_000;

/// One page of the reconcile scan: the next `$9` live memories after `$8` in
/// key order, of the owners and age asked for. Of those, the heads that
/// lack coverage or a job in the space are the candidates, and the first `$2`
/// of them are enqueued. Each page row probes its head and its embedding head
/// by key. Returns what was enqueued, how many memories the page
/// held and the last of them. The page is read by key, so a step reads a page
/// of the table however the filters fall.
const RECONCILE_EMBEDDINGS_SQL: &str = "
WITH page AS MATERIALIZED (
     SELECT m.t, m.handle, m.owner_id
       FROM proxima_core.memory m /* FORGET-POLICY: live-only (embedding drain and reconcile act on live memories) */
      WHERE m.forgotten_at IS NULL
        AND ($8::uuid IS NULL OR m.t > $8)
        AND ($7::uuid[] IS NULL OR m.owner_id = ANY($7::uuid[]))
        AND ($3::text <> 'since'
             OR proxima_core.memory_created_at(m.t) >= $4)
      ORDER BY m.t ASC
      LIMIT $9
 ),
 scoped AS MATERIALIZED (
     SELECT p.t AS memory_id,
            p.owner_id,
            h2.embedding_version AS head_version
       FROM page p
      CROSS JOIN LATERAL (
            SELECT mh.schema_id
              FROM proxima_core.memory_head mh
             WHERE mh.handle = p.handle AND mh.t = p.t
             LIMIT 1
       ) mh
       LEFT JOIN LATERAL (
            SELECT eh.embedding_version
              FROM proxima_core.embedding_heads eh
             WHERE eh.entity_id = p.t
               AND eh.model_id = $1
               AND eh.dim = $6
             LIMIT 1
       ) h2 ON true
      WHERE mh.schema_id <> ALL($5::text[])
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
       JOIN proxima_core.memory m ON m.t = e.memory_id AND m.forgotten_at IS NULL /* FORGET-POLICY: live-only (embedding drain and reconcile act on live memories) */
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
     (SELECT count(*)::bigint FROM inserted) AS enqueued,
     (SELECT count(*)::bigint FROM page) AS examined,
     (SELECT t FROM page ORDER BY t DESC LIMIT 1) AS last_examined";

/// The reconcile page statement, for EXPLAIN-based plan guards.
#[cfg(any(test, feature = "test-fixtures", debug_assertions))]
#[doc(hidden)]
#[must_use]
pub fn reconcile_embeddings_sql_for_tests() -> &'static str {
    RECONCILE_EMBEDDINGS_SQL
}

/// Global reconciliation for embeddable memories.
///
/// Scans Facts plus derived memories with stored text — every Owner's, or
/// only `options.owners` when the engine names the Owners routed to
/// `options.space` — skips rows by
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
#[cfg(test)]
pub async fn reconcile_embeddings(
    pool: &PgPool,
    platform: &crate::PgPlatformScope,
    options: EmbeddingReconcileOptions<'_>,
    stale_claim_timeout_seconds: i64,
) -> Result<EmbeddingReconcileOutcome, StorageError> {
    reconcile_embeddings_with_platform(pool, platform, options, stale_claim_timeout_seconds).await
}

pub(crate) async fn reconcile_embeddings_with_platform(
    pool: &PgPool,
    platform: &crate::PgPlatformScope,
    options: EmbeddingReconcileOptions<'_>,
    stale_claim_timeout_seconds: i64,
) -> Result<EmbeddingReconcileOutcome, StorageError> {
    let mut budget = resolve_reconcile_limit(options.limit)?;
    let mut total = EmbeddingReconcileOutcome::default();
    if budget == 0 {
        return Ok(total);
    }
    let mut tx = crate::platform_scope::begin_platform_transaction(pool, platform).await?;
    let reclaimed = reclaim_stale_embedding_jobs(tx.as_mut(), stale_claim_timeout_seconds).await;
    crate::owner_scope::finish_transaction(
        tx,
        reclaimed.map(|reclaimed| {
            if reclaimed > 0 {
                tracing::warn!(
                    reclaimed,
                    stale_after_seconds = stale_claim_timeout_seconds,
                    "reclaimed abandoned processing embedding jobs"
                );
            }
        }),
    )
    .await?;

    // One page per transaction: no statement reads more than a page of heads,
    // however many memories the store holds.
    let mut after = None;
    loop {
        let mut tx = crate::platform_scope::begin_platform_transaction(pool, platform).await?;
        let page = reconcile_page(tx.as_mut(), options, budget, after).await;
        let page = crate::owner_scope::finish_transaction(tx, page).await?;
        total.scanned += page.scanned;
        total.enqueued += page.enqueued;
        total.skipped += page.scanned.saturating_sub(page.enqueued);
        budget = budget.saturating_sub(i64::try_from(page.scanned).unwrap_or(i64::MAX));
        match page.next {
            Some(next) if budget > 0 => after = Some(next),
            _ => return Ok(total),
        }
    }
}

/// What one page of the scan did.
struct ReconcilePage {
    scanned: u64,
    enqueued: u64,
    /// The last head the page read, when the page was full and more may
    /// follow it.
    next: Option<uuid::Uuid>,
}

async fn reconcile_page(
    pool: &mut sqlx::PgConnection,
    options: EmbeddingReconcileOptions<'_>,
    limit: i64,
    after: Option<uuid::Uuid>,
) -> Result<ReconcilePage, StorageError> {
    let (scope, since) = match options.scope {
        EmbeddingReconcileScope::MissingOnly => ("missing_only", None),
        EmbeddingReconcileScope::IncludeStale => ("include_stale", None),
        EmbeddingReconcileScope::Since(since) => ("since", Some(since)),
    };

    let owner_ids: Option<Vec<uuid::Uuid>> = options
        .owners
        .map(|owners| owners.iter().copied().map(OwnerId::into_inner).collect());
    let row: (i64, i64, i64, Option<uuid::Uuid>) = sqlx::query_as(RECONCILE_EMBEDDINGS_SQL)
        .bind(options.space.model_id())
        .bind(limit)
        .bind(scope)
        .bind(since)
        .bind(options.non_embeddable_schemas)
        .bind(crate::pgvector::Lane::of(options.space.dim()).width)
        .bind(owner_ids)
        .bind(after)
        .bind(RECONCILE_PAGE)
        .fetch_one(&mut *pool)
        .await
        .map_err(map_err)?;

    let count = |value: i64, what: &str| {
        u64::try_from(value)
            .map_err(|_| StorageError::Internal(format!("{what} count is negative")))
    };
    Ok(ReconcilePage {
        scanned: count(row.0, "scanned")?,
        enqueued: count(row.1, "enqueued")?,
        next: if count(row.2, "examined")? == u64::try_from(RECONCILE_PAGE).unwrap_or(u64::MAX) {
            row.3
        } else {
            None
        },
    })
}

const EMBEDDING_OWNER_PAGE_SQL: &str = "
SELECT owner_id
  FROM proxima_core.owners
 WHERE ($1::uuid IS NULL OR owner_id > $1)
 ORDER BY owner_id ASC
 LIMIT $2";

/// Up to `limit` Owners after `after`, in `owner_id` order.
///
/// The engine pages every Owner through its embedding router before
/// reconciling, so this reads the Owner registry, not the memory table: an
/// Owner with nothing to embed routes once and reconciles nothing.
///
/// # Errors
///
/// Returns `ConstraintViolation` for negative limits, otherwise maps SQL
/// failures through the shared mapper.
pub(crate) async fn embedding_owner_page(
    pool: &PgPool,
    platform: &crate::PgPlatformScope,
    after: Option<OwnerId>,
    limit: i64,
) -> Result<Vec<OwnerId>, StorageError> {
    let limit = ensure_nonnegative_limit(limit)?;
    if limit == 0 {
        return Ok(Vec::new());
    }
    let mut tx = crate::platform_scope::begin_platform_transaction(pool, platform).await?;
    let result = sqlx::query_scalar::<_, uuid::Uuid>(EMBEDDING_OWNER_PAGE_SQL)
        .bind(after.map(OwnerId::into_inner))
        .bind(limit)
        .fetch_all(tx.as_mut())
        .await
        .map_err(map_err)
        .map(|rows| rows.into_iter().map(OwnerId::new).collect());
    crate::owner_scope::finish_transaction(tx, result).await
}

fn resolve_reconcile_limit(limit: Option<i64>) -> Result<i64, StorageError> {
    match limit {
        Some(limit) => ensure_nonnegative_limit(limit),
        None => Err(StorageError::ConstraintViolation(
            "reconcile limit is required".into(),
        )),
    }
}
