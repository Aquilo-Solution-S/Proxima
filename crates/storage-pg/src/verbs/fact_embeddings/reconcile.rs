use proxima_core::llm::{EmbeddingClient, LlmError};
use proxima_core::{EmbeddingJobClaim, StorageError};
use sqlx::PgPool;

use crate::error::map_err;

use super::jobs::claim_pending_embedding_jobs_excluding;
use super::{
    complete_embedding_job, ensure_nonnegative_limit, fail_embedding_job,
    fail_embedding_job_permanently, insert_memory_embedding, load_embedding_text,
};

pub use proxima_core::{
    EmbeddingReconcileOptions, EmbeddingReconcileOutcome, EmbeddingReconcileScope,
};

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
     DO UPDATE SET status = 'pending'::proxima_core.embedding_job_status,
                   attempts = 0,
                   last_error = NULL,
                   next_attempt_at = now(),
                   updated_at = now()
         WHERE embedding_jobs.status = 'failed'::proxima_core.embedding_job_status
           -- Permanently rejected inputs (PERMANENT_EMBED_FAILURE_MARKER)
           -- stay terminal: requeueing them would only re-poison the queue.
           AND (embedding_jobs.last_error IS NULL
                OR embedding_jobs.last_error NOT LIKE 'permanent: %')
     RETURNING 1
 )
 SELECT
     (SELECT count(*)::bigint FROM limited) AS scanned,
     (SELECT count(*)::bigint FROM inserted) AS enqueued";

/// Global reconciliation for embeddable memories.
///
/// Scans Facts plus derived memories with stored text, skips rows by
/// scope-specific embedding coverage and target-model durable jobs, and
/// enqueues via `proxima_core.embedding_jobs`. A row that already holds a
/// `failed` job (retries exhausted per `fail_embedding_job`) is requeued —
/// status back to `pending`, attempts reset, backoff cleared — so reconcile is
/// the operator/startup reset that lifts a Fact out of the retry dead-end.
/// Jobs terminally failed for a permanent input rejection
/// (`fail_embedding_job_permanently`, marker-prefixed `last_error`) are NOT
/// requeued: the provider will always reject the same input again.
/// `pending`/`processing` jobs are left untouched.
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
            Err(EmbedClaimFailure::Permanent(message)) => {
                outcome.failed += 1;
                fail_embedding_job_permanently(pool, &claim, &message).await?;
            }
            Err(EmbedClaimFailure::Retryable(err)) => {
                outcome.failed += 1;
                fail_embedding_job(pool, &claim, &err.to_string()).await?;
            }
        }
    }
    Ok(outcome)
}

/// Why one claimed job could not embed: a permanent input rejection (job
/// must go terminal) versus everything retryable.
enum EmbedClaimFailure {
    Permanent(String),
    Retryable(StorageError),
}

impl From<StorageError> for EmbedClaimFailure {
    fn from(err: StorageError) -> Self {
        Self::Retryable(err)
    }
}

async fn embed_claim(
    pool: &PgPool,
    client: &dyn EmbeddingClient,
    claim: &EmbeddingJobClaim,
) -> Result<bool, EmbedClaimFailure> {
    let Some(text) =
        load_embedding_text(pool, &claim.owner, claim.entity_kind, claim.entity_id).await?
    else {
        return Ok(false);
    };
    let embedding = client.embed(&text).await.map_err(|err| match err {
        LlmError::EmbedPermanent(message) => {
            EmbedClaimFailure::Permanent(format!("embed memory text: {message}"))
        }
        other => EmbedClaimFailure::Retryable(StorageError::Internal(format!(
            "embed memory text: {other}"
        ))),
    })?;
    if embedding.len() != client.dim() {
        return Err(StorageError::ConstraintViolation(format!(
            "embedding dim mismatch: client dim {} but vector len {}",
            client.dim(),
            embedding.len(),
        ))
        .into());
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
