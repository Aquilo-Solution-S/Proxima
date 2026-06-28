use proxima_core::llm::{EMBEDDING_DIM, EMBEDDING_JOB_MAX_ATTEMPTS, EmbeddingClient};
use proxima_core::{EmbeddingJobClaim, EntityKind, MemoryId, Owner, OwnerRefKind, StorageError};
use sqlx::{PgPool, Postgres, Transaction};
use time::OffsetDateTime;

use crate::error::map_err;

fn owner_parts(owner: &Owner) -> (OwnerRefKind, uuid::Uuid) {
    owner.columns()
}

#[derive(sqlx::FromRow)]
struct EmbeddingJobClaimRow {
    owner_principal_kind: OwnerRefKind,
    owner_principal_id: uuid::Uuid,
    entity_kind: EntityKind,
    entity_id: uuid::Uuid,
    model_id: String,
    embedding_version: i32,
    attempts: i32,
}

impl From<EmbeddingJobClaimRow> for EmbeddingJobClaim {
    fn from(row: EmbeddingJobClaimRow) -> Self {
        Self {
            owner: row.owner_principal_kind.with_uuid(row.owner_principal_id),
            entity_kind: row.entity_kind,
            entity_id: MemoryId::new(row.entity_id),
            model_id: row.model_id,
            embedding_version: row.embedding_version,
            attempts: row.attempts,
        }
    }
}

fn ensure_nonnegative_limit(limit: i64) -> Result<i64, StorageError> {
    if limit < 0 {
        return Err(StorageError::ConstraintViolation(
            "limit must be nonnegative".into(),
        ));
    }
    Ok(limit)
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
            home_owner.owner_principal_kind,
            home_owner.owner_principal_id
       FROM proxima_core.memories m
       LEFT JOIN __PROXIMA_ENTITY_OWNER__ home_owner
         ON home_owner.entity_id = m.memory_id
        AND home_owner.is_home
      WHERE m.text IS NOT NULL
        AND m.tombstoned_at IS NULL
        AND (
            (m.event_id IS NOT NULL AND m.kind IS NULL)
            OR m.kind IN (
                'Abstraction'::proxima_core.entity_kind,
                'Perspective'::proxima_core.entity_kind
            )
        )
        AND ($3::text <> 'since' OR m.created_at >= $4)
      ORDER BY m.created_at ASC, m.memory_id ASC
      LIMIT $2
 ),
 eligible AS MATERIALIZED (
     SELECT s.*
       FROM scoped s
      WHERE (
            CASE WHEN $3::text = 'missing_only'
            THEN NOT EXISTS (
                SELECT 1
                  FROM proxima_core.embeddings e
                 WHERE e.entity_kind = s.entity_kind
                   AND e.entity_id = s.memory_id
                   AND e.embedding_version = 1
            )
            ELSE NOT EXISTS (
                SELECT 1
                  FROM proxima_core.embeddings e
                 WHERE e.entity_kind = s.entity_kind
                   AND e.entity_id = s.memory_id
                   AND e.embedding_version = 1
                   AND e.model_id = $1
            )
            END
        )
        AND NOT EXISTS (
            SELECT 1
              FROM proxima_core.embedding_jobs j
             WHERE j.owner_principal_kind = s.owner_principal_kind
               AND j.owner_principal_id = s.owner_principal_id
               AND j.entity_kind = s.entity_kind
               AND j.entity_id = s.memory_id
               AND j.model_id = $1
               AND j.embedding_version = 1
        )
 ),
 inserted AS (
     INSERT INTO proxima_core.embedding_jobs
         (owner_principal_kind, owner_principal_id,
          entity_kind, entity_id, model_id)
     SELECT owner_principal_kind, owner_principal_id,
            entity_kind, memory_id, $1
       FROM eligible
     ON CONFLICT (owner_principal_kind, owner_principal_id,
                  entity_kind, entity_id, model_id, embedding_version)
     DO NOTHING
     RETURNING 1
 )
 SELECT
     (SELECT count(*)::bigint FROM scoped) AS scanned,
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
    let (owner_kind, owner_principal_id) = owner_parts(owner);
    sqlx::query_scalar(crate::access::owner_ref_compat::sql(
        "SELECT text
           FROM proxima_core.memories
          WHERE memory_id = $1
            AND EXISTS (
                SELECT 1
                  FROM __PROXIMA_ENTITY_OWNER__ eo
                 WHERE eo.entity_id = memory_id
                   AND eo.owner_principal_kind = $2
                   AND eo.owner_principal_id = $3
                   AND eo.is_home
            )
            AND event_id IS NOT NULL
            AND tombstoned_at IS NULL",
    ))
    .bind(memory_id.into_inner())
    .bind(owner_kind)
    .bind(owner_principal_id)
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
    let (owner_kind, owner_principal_id) = owner_parts(owner);
    sqlx::query_scalar(crate::access::owner_ref_compat::sql(
        "SELECT text
           FROM proxima_core.memories
          WHERE memory_id = $1
            AND EXISTS (
                SELECT 1
                  FROM __PROXIMA_ENTITY_OWNER__ eo
                 WHERE eo.entity_id = memory_id
                   AND eo.owner_principal_kind = $2
                   AND eo.owner_principal_id = $3
                   AND eo.is_home
            )
            AND text IS NOT NULL
            AND tombstoned_at IS NULL
            AND (
                ($4 = 'Fact'::proxima_core.entity_kind
                 AND event_id IS NOT NULL
                 AND kind IS NULL)
                OR kind = $4
            )",
    ))
    .bind(memory_id.into_inner())
    .bind(owner_kind)
    .bind(owner_principal_id)
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
    let (owner_kind, owner_principal_id) = owner_parts(owner);
    sqlx::query_scalar(crate::access::owner_ref_compat::sql(
        "SELECT text
           FROM proxima_core.memories
          WHERE memory_id = $1
            AND EXISTS (
                SELECT 1
                  FROM __PROXIMA_ENTITY_OWNER__ eo
                 WHERE eo.entity_id = memory_id
                   AND eo.owner_principal_kind = $2
                   AND eo.owner_principal_id = $3
                   AND eo.is_home
            )
            AND event_id IS NOT NULL
            AND tombstoned_at IS NULL",
    ))
    .bind(memory_id.into_inner())
    .bind(owner_kind)
    .bind(owner_principal_id)
    .fetch_optional(tx.as_mut())
    .await
    .map_err(map_err)
}

/// Idempotently upsert one Fact embedding row inside an existing tx.
///
/// # Errors
///
/// Returns `ConstraintViolation` when `dim` or `vec.len()` do not match
/// the fixed embedding width, otherwise maps SQL failures through the
/// shared mapper.
pub async fn upsert_fact_embedding(
    tx: &mut Transaction<'_, Postgres>,
    owner: &Owner,
    memory_id: MemoryId,
    model_id: &str,
    dim: usize,
    vec: &[f32],
) -> Result<(), StorageError> {
    upsert_memory_embedding(tx, owner, EntityKind::Fact, memory_id, model_id, dim, vec).await
}

/// Idempotently upsert one memory embedding row inside an existing tx.
///
/// # Errors
///
/// Returns `ConstraintViolation` when `dim` or `vec.len()` do not match
/// the fixed embedding width, otherwise maps SQL failures through the
/// shared mapper.
pub async fn upsert_memory_embedding(
    tx: &mut Transaction<'_, Postgres>,
    owner: &Owner,
    entity_kind: EntityKind,
    memory_id: MemoryId,
    model_id: &str,
    dim: usize,
    vec: &[f32],
) -> Result<(), StorageError> {
    let (owner_kind, owner_principal_id) = owner_parts(owner);
    if dim != EMBEDDING_DIM || vec.len() != EMBEDDING_DIM {
        return Err(StorageError::ConstraintViolation(
            "embedding length must be 1024".into(),
        ));
    }
    let vec_literal = crate::pgvector::literal(vec);
    sqlx::query(crate::access::owner_ref_compat::sql(
        "INSERT INTO proxima_core.embeddings
            (entity_kind, entity_id, embedding_version, model_id, vec,
             owner_principal_kind, owner_principal_id)
         SELECT $1, $2, 1, $3, $4::vector, $5, $6
          WHERE EXISTS (
                SELECT 1
                  FROM proxima_core.memories m
                 WHERE m.memory_id = $2
                   AND EXISTS (
                        SELECT 1
                          FROM __PROXIMA_ENTITY_OWNER__ eo
                         WHERE eo.entity_id = m.memory_id
                           AND eo.owner_principal_kind = $5
                           AND eo.owner_principal_id = $6
                           AND eo.is_home
                   )
                   AND m.text IS NOT NULL
                   AND m.tombstoned_at IS NULL
                   AND (
                       ($1 = 'Fact'::proxima_core.entity_kind
                        AND m.event_id IS NOT NULL
                        AND m.kind IS NULL)
                       OR m.kind = $1
                   )
            )
         ON CONFLICT (entity_kind, entity_id, embedding_version, model_id)
         DO UPDATE SET
             vec = EXCLUDED.vec,
             owner_principal_kind = EXCLUDED.owner_principal_kind,
             owner_principal_id = EXCLUDED.owner_principal_id",
    ))
    .bind(entity_kind)
    .bind(memory_id.into_inner())
    .bind(model_id)
    .bind(vec_literal)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;
    Ok(())
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
    let (owner_kind, owner_principal_id) = owner_parts(owner);
    let limit = i64::try_from(limit)
        .map_err(|_| StorageError::ConstraintViolation("limit too large".into()))?;
    let rows = sqlx::query_scalar::<_, uuid::Uuid>(crate::access::owner_ref_compat::sql(
        "SELECT m.memory_id
           FROM proxima_core.memories m
          WHERE EXISTS (
                    SELECT 1
                      FROM __PROXIMA_ENTITY_OWNER__ eo
                     WHERE eo.entity_id = m.memory_id
                       AND eo.owner_principal_kind = $1
                       AND eo.owner_principal_id = $2
                       AND eo.is_home
                )
            AND m.event_id IS NOT NULL
            AND m.text IS NOT NULL
            AND m.tombstoned_at IS NULL
            AND NOT EXISTS (
                SELECT 1
                  FROM proxima_core.embeddings e
                 WHERE e.entity_kind = 'Fact'
                   AND e.entity_id = m.memory_id
                   AND e.embedding_version = 1
                   AND e.model_id = $3
            )
          ORDER BY m.created_at ASC, m.memory_id ASC
          LIMIT $4",
    ))
    .bind(owner_kind)
    .bind(owner_principal_id)
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
             SELECT owner_principal_kind, owner_principal_id,
                    entity_kind, entity_id, model_id, embedding_version
               FROM proxima_core.embedding_jobs
              WHERE model_id = $1
                AND (status = 'pending'
                     OR (status = 'processing'
                         AND updated_at < now() - interval '15 minutes'))
                AND NOT (entity_id = ANY($2::uuid[]))
              ORDER BY enqueued_at ASC,
                       owner_principal_kind ASC,
                       owner_principal_id ASC,
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
          WHERE j.owner_principal_kind = claimed.owner_principal_kind
            AND j.owner_principal_id = claimed.owner_principal_id
            AND j.entity_kind = claimed.entity_kind
            AND j.entity_id = claimed.entity_id
            AND j.model_id = claimed.model_id
            AND j.embedding_version = claimed.embedding_version
        RETURNING j.owner_principal_kind, j.owner_principal_id,
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
    let (owner_kind, owner_principal_id) = owner_parts(&claim.owner);
    sqlx::query(
        "DELETE FROM proxima_core.embedding_jobs
          WHERE owner_principal_kind = $1
            AND owner_principal_id = $2
            AND entity_kind = $3
            AND entity_id = $4
            AND model_id = $5
            AND embedding_version = $6",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
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
    let (owner_kind, owner_principal_id) = owner_parts(&claim.owner);
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
          WHERE owner_principal_kind = $1
            AND owner_principal_id = $2
            AND entity_kind = $3
            AND entity_id = $4
            AND model_id = $5
            AND embedding_version = $6
            AND status = 'processing'",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
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
    owner: &Owner,
    model_id: &str,
    limit: i64,
) -> Result<u64, StorageError> {
    let limit = ensure_nonnegative_limit(limit)?;
    if limit == 0 {
        return Ok(0);
    }
    let (owner_kind, owner_principal_id) = owner_parts(owner);
    let result = sqlx::query(crate::access::owner_ref_compat::sql(
        "WITH missing AS (
             SELECT m.memory_id
               FROM proxima_core.memories m
              WHERE EXISTS (
                        SELECT 1
                          FROM __PROXIMA_ENTITY_OWNER__ eo
                         WHERE eo.entity_id = m.memory_id
                           AND eo.owner_principal_kind = $1
                           AND eo.owner_principal_id = $2
                           AND eo.is_home
                    )
                AND m.event_id IS NOT NULL
                AND m.text IS NOT NULL
                AND m.tombstoned_at IS NULL
                AND NOT EXISTS (
                    SELECT 1
                      FROM proxima_core.embeddings e
                     WHERE e.entity_kind = 'Fact'
                       AND e.entity_id = m.memory_id
                       AND e.embedding_version = 1
                       AND e.model_id = $3
                )
              ORDER BY m.created_at ASC, m.memory_id ASC
              LIMIT $4
         )
         INSERT INTO proxima_core.embedding_jobs
             (owner_principal_kind, owner_principal_id,
              entity_kind, entity_id, model_id)
         SELECT $1, $2, 'Fact'::proxima_core.entity_kind, memory_id, $3
           FROM missing
         ON CONFLICT (owner_principal_kind, owner_principal_id,
                      entity_kind, entity_id, model_id, embedding_version)
         DO NOTHING",
    ))
    .bind(owner_kind)
    .bind(owner_principal_id)
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
    let (owner_kind, owner_principal_id) = owner_parts(owner);
    let row: (i64,) = sqlx::query_as(
        "SELECT count(*)
           FROM proxima_core.embedding_jobs
          WHERE owner_principal_kind = $1
            AND owner_principal_id = $2
            AND status IN ('pending', 'processing')",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .fetch_one(pool)
    .await
    .map_err(map_err)?;
    u64::try_from(row.0)
        .map_err(|_| StorageError::Internal("pending embedding job count is negative".into()))
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

    let row: (i64, i64) = sqlx::query_as(crate::access::owner_ref_compat::sql(
        RECONCILE_EMBEDDINGS_SQL,
    ))
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
    upsert_memory_embedding(
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
