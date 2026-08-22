use proxima_core::llm::{
    EMBED_LIVENESS_PROBE, EmbeddingClient, LlmError, embed_in_chunks_after_failure_with_timeout,
    embed_many_with_timeout, embed_with_timeout,
};
use proxima_core::verbs::schema::MemoryEmbedUnit;
use proxima_core::{EmbeddableEntityRef, EmbeddingJobClaim, StorageError};
use sqlx::PgPool;

use crate::error::map_err;

use super::{
    claim_pending_embedding_jobs, complete_embedding_job, ensure_nonnegative_limit,
    fail_embedding_job, fail_embedding_job_permanently, insert_embedding_chunks,
    insert_memory_embedding, load_embedding_texts, reclaim_stale_embedding_jobs,
    release_embedding_jobs, renew_embedding_jobs,
};

pub use proxima_core::{
    EmbeddingReconcileOptions, EmbeddingReconcileOutcome, EmbeddingReconcileScope,
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EmbeddingInlineDrainOutcome {
    pub embedded: usize,
    pub failed: usize,
}

struct EmbeddingClaimHeartbeat {
    handle: tokio::task::JoinHandle<()>,
}

impl Drop for EmbeddingClaimHeartbeat {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

fn spawn_claim_heartbeat(
    pool: PgPool,
    claims: Vec<EmbeddingJobClaim>,
    interval: std::time::Duration,
) -> EmbeddingClaimHeartbeat {
    let handle = tokio::spawn(async move {
        loop {
            tokio::time::sleep(interval).await;
            if let Err(err) = renew_embedding_jobs(&pool, &claims).await {
                tracing::warn!(error = %err, "inline embedding claim heartbeat failed");
            }
        }
    });
    EmbeddingClaimHeartbeat { handle }
}

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
         (entity_id, model_id, owner_id)
     SELECT memory_id, $1, owner_id
       FROM limited
     ON CONFLICT (owner_id, entity_id, model_id)
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
/// scope-specific embedding coverage and target-model durable jobs, and
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
    let limit = resolve_reconcile_limit(options.limit)?;
    if limit == 0 {
        return Ok(EmbeddingReconcileOutcome::default());
    }

    let reclaimed = reclaim_stale_embedding_jobs(pool, stale_claim_timeout_seconds).await?;
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

fn resolve_reconcile_limit(limit: Option<i64>) -> Result<i64, StorageError> {
    match limit {
        Some(limit) => ensure_nonnegative_limit(limit),
        None => Err(StorageError::ConstraintViolation(
            "reconcile limit is required".into(),
        )),
    }
}

/// Drain queued embedding jobs inline. Claims at most the host policy's
/// provider batch width at once; a processed entity cannot be re-claimed by
/// this invocation.
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
    units: &[MemoryEmbedUnit],
    policy: proxima_core::EmbeddingRuntimePolicy,
) -> Result<EmbeddingInlineDrainOutcome, StorageError> {
    let mut remaining = ensure_nonnegative_limit(limit)?;
    let batch_size = i64::try_from(policy.batch_size())
        .map_err(|_| StorageError::ConstraintViolation("embedding batch size too large".into()))?;
    let mut outcome = EmbeddingInlineDrainOutcome::default();
    while remaining > 0 {
        let claims =
            claim_pending_embedding_jobs(pool, client.model_id(), remaining.min(batch_size))
                .await?;
        if claims.is_empty() {
            break;
        }
        remaining = remaining.saturating_sub(i64::try_from(claims.len()).map_err(|_| {
            StorageError::ConstraintViolation("claimed embedding job count too large".into())
        })?);
        let keep_draining =
            drain_claimed_jobs(pool, client, claims, units, policy, &mut outcome).await?;
        if !keep_draining {
            break;
        }
    }
    Ok(outcome)
}

/// Process one claimed batch. The database unique key admits at most one job
/// per `(owner_id, entity_id, model_id)`, so the batch contains each entity at
/// most once without caller-maintained exclusion state.
async fn drain_claimed_jobs(
    pool: &PgPool,
    client: &dyn EmbeddingClient,
    claims: Vec<EmbeddingJobClaim>,
    units: &[MemoryEmbedUnit],
    policy: proxima_core::EmbeddingRuntimePolicy,
    outcome: &mut EmbeddingInlineDrainOutcome,
) -> Result<bool, StorageError> {
    let _heartbeat = spawn_claim_heartbeat(
        pool.clone(),
        claims.clone(),
        policy.claim_heartbeat_interval(),
    );
    let items: Vec<_> = claims
        .iter()
        .map(|claim| (claim.owner, claim.entity_kind, claim.entity_id))
        .collect();
    let texts = load_embedding_texts(pool, &items, &[], units).await?;
    let mut batch = Vec::with_capacity(claims.len());
    for (claim, text) in claims.into_iter().zip(texts) {
        match text {
            Some(text) => batch.push((claim, text)),
            None => complete_embedding_job(pool, &claim).await?,
        }
    }
    if batch.is_empty() {
        return Ok(true);
    }

    let texts: Vec<String> = batch.iter().map(|(_, text)| text.clone()).collect();
    match embed_many_with_timeout(client, &texts, policy.request_timeout()).await {
        Ok(vectors) if vectors.len() != batch.len() => {
            let error = format!(
                "embedding batch cardinality mismatch: sent {} texts but received {} vectors",
                batch.len(),
                vectors.len(),
            );
            tracing::warn!(
                sent = batch.len(),
                received = vectors.len(),
                "inline embedding provider returned malformed batch cardinality"
            );
            let claims: Vec<EmbeddingJobClaim> =
                batch.into_iter().map(|(claim, _)| claim).collect();
            release_embedding_jobs(pool, &claims, &error).await?;
            Ok(false)
        }
        Ok(vectors) => {
            for ((claim, _), vector) in batch.iter().zip(vectors) {
                finish_claim(
                    pool,
                    claim,
                    store_claim_embedding(pool, client, claim, &vector).await,
                    outcome,
                )
                .await?;
            }
            Ok(true)
        }
        Err(LlmError::EmbedPermanent(_)) => {
            drain_claims_individually(pool, client, batch, policy, outcome).await?;
            Ok(true)
        }
        Err(err) => {
            if embed_with_timeout(client, EMBED_LIVENESS_PROBE, policy.request_timeout())
                .await
                .is_ok()
            {
                tracing::warn!(
                    error = %err,
                    jobs = batch.len(),
                    "transient inline embedding batch failure but provider answers; isolating inputs"
                );
                drain_claims_individually(pool, client, batch, policy, outcome).await?;
                return Ok(true);
            }
            let claims: Vec<EmbeddingJobClaim> =
                batch.into_iter().map(|(claim, _)| claim).collect();
            release_embedding_jobs(pool, &claims, &format!("embed memory text: {err}")).await?;
            Ok(false)
        }
    }
}

async fn drain_claims_individually(
    pool: &PgPool,
    client: &dyn EmbeddingClient,
    batch: Vec<(EmbeddingJobClaim, String)>,
    policy: proxima_core::EmbeddingRuntimePolicy,
    outcome: &mut EmbeddingInlineDrainOutcome,
) -> Result<(), StorageError> {
    for (claim, text) in batch {
        let result = embed_claim(pool, client, &claim, &text, policy).await;
        finish_claim(pool, &claim, result, outcome).await?;
    }
    Ok(())
}

async fn finish_claim(
    pool: &PgPool,
    claim: &EmbeddingJobClaim,
    result: Result<bool, EmbedClaimFailure>,
    outcome: &mut EmbeddingInlineDrainOutcome,
) -> Result<(), StorageError> {
    match result {
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
    Ok(())
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
    super::lock_embedding_job_claim_for_claim(&mut tx, claim, client.model_id()).await?;
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
    text: &str,
    policy: proxima_core::EmbeddingRuntimePolicy,
) -> Result<bool, EmbedClaimFailure> {
    let embedding = match embed_with_timeout(client, text, policy.request_timeout()).await {
        Ok(embedding) => embedding,
        // A live provider's content-attributed rejection is not a dead
        // memory. The engine's drain rescues it as a chunked embedding
        // version; this drain must do the same, or which drain happens to
        // reach a job first decides whether the memory is recoverable. An
        // ambiguous rejection is eligible only after a successful liveness
        // probe; a failed probe remains retryable.
        Err(error) => {
            let initial_error = error.to_string();
            return match embed_in_chunks_after_failure_with_timeout(
                client,
                text,
                error,
                policy.request_timeout(),
            )
            .await
            {
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
                // Rejected at every length by a live provider: genuinely invalid input, so
                // the job goes terminal.
                Ok(None) => Err(EmbedClaimFailure::Permanent(format!(
                    "embed memory text: {initial_error}"
                ))),
                Err(err) => Err(EmbedClaimFailure::Retryable(StorageError::Internal(
                    format!("embed memory text in chunks: {err}"),
                ))),
            };
        }
    };
    store_claim_embedding(pool, client, claim, &embedding).await
}

async fn store_claim_embedding(
    pool: &PgPool,
    client: &dyn EmbeddingClient,
    claim: &EmbeddingJobClaim,
    embedding: &[f32],
) -> Result<bool, EmbedClaimFailure> {
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
    super::lock_embedding_job_claim_for_claim(&mut tx, claim, client.model_id()).await?;
    insert_memory_embedding(
        &mut tx,
        &claim.owner,
        claim.entity_kind,
        claim.entity_id,
        client.model_id(),
        client.dim(),
        embedding,
    )
    .await?;
    tx.commit().await.map_err(map_err)?;
    Ok(true)
}

#[cfg(test)]
mod limit_tests {
    use proxima_core::StorageError;

    #[test]
    fn missing_limit_is_constraint() {
        let err = super::resolve_reconcile_limit(None).expect_err("None");
        assert!(
            matches!(err, StorageError::ConstraintViolation(ref msg) if msg.contains("required")),
            "{err}"
        );
    }

    #[test]
    fn some_limit_is_kept() {
        assert_eq!(
            super::resolve_reconcile_limit(Some(50_000)).expect("ok"),
            50_000
        );
    }
}
