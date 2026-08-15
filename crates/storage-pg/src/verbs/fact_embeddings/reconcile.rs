use proxima_core::llm::{EmbeddingClient, LlmError};
use proxima_core::{EmbeddableEntityRef, EmbeddingJobClaim, StorageError};
use sqlx::PgPool;

use crate::error::map_err;

use super::jobs::claim_pending_embedding_jobs_excluding;
use super::{
    complete_embedding_job, ensure_nonnegative_limit, fail_embedding_job,
    fail_embedding_job_permanently, insert_embedding_chunks, insert_memory_embedding,
    load_embedding_text, release_embedding_jobs,
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
            OR m.kind IN ('Abstraction', 'Perspective')
        )
        AND ($3::text <> 'since' OR m.created_at >= $4)
        -- Declined a vector rather than lacking one; see
        -- FactPayload::EMBEDDABLE. This is the call that heals a missing
        -- job, which is exactly the state such a schema must stay in, so
        -- the exclusion belongs here as much as on the write path.
        -- <> ALL of an empty array is TRUE, so an empty list is a no-op.
        AND m.schema_id <> ALL($5::text[])
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
               AND j.status IN ('pending', 'processing')
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
     DO UPDATE SET status = 'pending',
                   attempts = 0,
                   last_error = NULL,
                   next_attempt_at = now(),
                   updated_at = now()
         WHERE embedding_jobs.status = 'failed'
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
        .bind(options.non_embeddable_schemas)
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
/// Claims the whole batch in one statement, the way the engine batch drain
/// always has. This used to claim `limit = 1` per iteration while excluding
/// the entities it had already processed, which is O(backlog sort × jobs)
/// plus O(n²) exclusion-array compares (sql-sweep S2). Every processed job
/// leaves a state (`deleted`, `failed`, or backoff-gated `pending`) the
/// claim cannot re-claim, so the exclusion list's only remaining work —
/// skipping a second queued version of an entity already embedded this
/// drain — moves to the shared drain's duplicate release.
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
    let claims =
        claim_pending_embedding_jobs_excluding(pool, client.model_id(), limit, &[]).await?;
    drain_claimed_jobs(pool, client, claims).await
}

/// Process one claimed batch: each entity's first claim embeds, and a
/// second claimed job for an entity already handled this drain (two queued
/// `embedding_version`s of one entity) is released back to `pending`
/// unprocessed — the per-iteration path's exclusion list kept such a job
/// unclaimed, and embedding it again would only mint an identical extra
/// version.
async fn drain_claimed_jobs(
    pool: &PgPool,
    client: &dyn EmbeddingClient,
    claims: Vec<EmbeddingJobClaim>,
) -> Result<EmbeddingInlineDrainOutcome, StorageError> {
    let mut outcome = EmbeddingInlineDrainOutcome::default();
    let mut processed_entity_ids = Vec::new();
    let mut duplicates = Vec::new();
    for claim in claims {
        if processed_entity_ids.contains(&claim.entity_id) {
            duplicates.push(claim);
            continue;
        }
        processed_entity_ids.push(claim.entity_id);
        outcome = drain_one_claim(pool, client, &claim, outcome).await?;
    }
    if !duplicates.is_empty() {
        release_embedding_jobs(
            pool,
            &duplicates,
            "released unprocessed: entity already embedded by this drain batch",
        )
        .await?;
    }
    Ok(outcome)
}

/// Embed one claim and record its terminal job state, counting the result
/// into `outcome`. Shared by both drain shapes so the flag can only change
/// how jobs are claimed, never what happens to a claimed job.
async fn drain_one_claim(
    pool: &PgPool,
    client: &dyn EmbeddingClient,
    claim: &EmbeddingJobClaim,
    mut outcome: EmbeddingInlineDrainOutcome,
) -> Result<EmbeddingInlineDrainOutcome, StorageError> {
    match embed_claim(pool, client, claim).await {
        Ok(true) => {
            complete_embedding_job(pool, claim).await?;
            outcome.embedded += 1;
        }
        Ok(false) => {
            complete_embedding_job(pool, claim).await?;
        }
        Err(EmbedClaimFailure::Permanent(message)) => {
            outcome.failed += 1;
            fail_embedding_job_permanently(pool, claim, &message).await?;
        }
        Err(EmbedClaimFailure::Retryable(err)) => {
            outcome.failed += 1;
            fail_embedding_job(pool, claim, &err.to_string()).await?;
        }
    }
    Ok(outcome)
}

/// Write one chunked embedding version for a claim, mirroring the engine
/// drain's `store_claim_embedding_chunks`. A dimension mismatch in any chunk
/// is an ordinary retryable failure, not a terminal one — the provider
/// answered, it just answered in the wrong space.
async fn store_claim_chunks(
    pool: &PgPool,
    client: &dyn EmbeddingClient,
    claim: &EmbeddingJobClaim,
    vectors: &[Vec<f32>],
) -> Result<(), EmbedClaimFailure> {
    if let Some(bad) = vectors.iter().find(|vec| vec.len() != client.dim()) {
        return Err(StorageError::ConstraintViolation(format!(
            "chunked embedding dim mismatch: client dim {} but got a chunk of len {}",
            client.dim(),
            bad.len(),
        ))
        .into());
    }
    let chunks: Vec<&[f32]> = vectors.iter().map(Vec::as_slice).collect();
    let mut tx = pool.begin().await.map_err(|err| {
        StorageError::Internal(format!("begin chunked embedding upsert tx: {err}"))
    })?;
    insert_embedding_chunks(
        &mut tx,
        &claim.owner,
        EmbeddableEntityRef::Memory {
            kind: claim.entity_kind,
            memory_id: claim.entity_id,
        },
        client.model_id(),
        client.dim(),
        &chunks,
    )
    .await?;
    tx.commit().await.map_err(map_err)?;
    Ok(())
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
    // Empty exclusion list: this drain only ever sees rows that a job was
    // enqueued for, and every enqueue site already filters the schemas that
    // declined a vector. Storage does not hold the flavor registry, so the
    // list is not reachable here — the gate for a caller that bypasses the
    // queue lives on `EmbeddingTextPort::load_embedding_text`.
    let Some(text) =
        load_embedding_text(pool, &claim.owner, claim.entity_kind, claim.entity_id, &[]).await?
    else {
        return Ok(false);
    };
    let embedding = match client.embed(&text).await {
        Ok(embedding) => embedding,
        // An over-limit input is not a dead memory. The engine's drain
        // rescues it as a chunked embedding version; this drain must do the
        // same, or which drain happens to reach a job first decides whether
        // the memory is recoverable — and a job failed here is marked
        // terminal, which `reconcile_embeddings` then refuses to requeue.
        Err(LlmError::EmbedPermanent(message)) => {
            return match proxima_core::llm::embed_in_chunks(client, &text).await {
                Ok(Some(vectors)) => {
                    tracing::warn!(
                        entity_id = ?claim.entity_id,
                        chunks = vectors.len(),
                        total_bytes = text.len(),
                        "over-limit embedding input rescued as chunked embeddings"
                    );
                    store_claim_chunks(pool, client, claim, &vectors).await?;
                    Ok(true)
                }
                // Rejected at every length: genuinely invalid input, so the
                // job goes terminal exactly as it did before.
                Ok(None) => Err(EmbedClaimFailure::Permanent(format!(
                    "embed memory text: {message}"
                ))),
                Err(err) => Err(EmbedClaimFailure::Retryable(StorageError::Internal(
                    format!("embed memory text in chunks: {err}"),
                ))),
            };
        }
        Err(other) => {
            return Err(EmbedClaimFailure::Retryable(StorageError::Internal(
                format!("embed memory text: {other}"),
            )));
        }
    };
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
