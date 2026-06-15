use proxima_core::llm::{EMBEDDING_DIM, EMBEDDING_JOB_MAX_ATTEMPTS};
use proxima_core::{
    EmbeddingJobClaim, EntityKind, MemoryId, OrgId, Owner, OwnerPrincipalKind, Principal,
    StorageError,
};
use sqlx::{PgPool, Postgres, Transaction};

use crate::error::map_err;

fn owner_parts(owner: &Owner) -> (OwnerPrincipalKind, uuid::Uuid, uuid::Uuid) {
    let (owner_kind, owner_principal_id) = match &owner.principal {
        Principal::User(user) => (OwnerPrincipalKind::User, user.into_inner()),
        Principal::Group(group) => (OwnerPrincipalKind::Group, group.into_inner()),
    };
    (owner_kind, owner_principal_id, owner.org_id.into_inner())
}

#[derive(sqlx::FromRow)]
struct EmbeddingJobClaimRow {
    owner_principal_kind: OwnerPrincipalKind,
    owner_principal_id: uuid::Uuid,
    owner_org_id: uuid::Uuid,
    entity_kind: EntityKind,
    entity_id: uuid::Uuid,
    model_id: String,
    embedding_version: i32,
    attempts: i32,
}

impl From<EmbeddingJobClaimRow> for EmbeddingJobClaim {
    fn from(row: EmbeddingJobClaimRow) -> Self {
        Self {
            owner: Owner {
                principal: row.owner_principal_kind.with_uuid(row.owner_principal_id),
                org_id: OrgId::new(row.owner_org_id),
            },
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
    let (owner_kind, owner_principal_id, owner_org_id) = owner_parts(owner);
    sqlx::query_scalar(
        "SELECT text
           FROM proxima_core.memories
          WHERE memory_id = $1
            AND owner_principal_kind = $2
            AND owner_principal_id = $3
            AND owner_org_id = $4
            AND event_id IS NOT NULL
            AND tombstoned_at IS NULL",
    )
    .bind(memory_id.into_inner())
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
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
    let (owner_kind, owner_principal_id, owner_org_id) = owner_parts(owner);
    sqlx::query_scalar(
        "SELECT text
           FROM proxima_core.memories
          WHERE memory_id = $1
            AND owner_principal_kind = $2
            AND owner_principal_id = $3
            AND owner_org_id = $4
            AND event_id IS NOT NULL
            AND tombstoned_at IS NULL",
    )
    .bind(memory_id.into_inner())
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
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
    let (owner_kind, owner_principal_id, owner_org_id) = owner_parts(owner);
    if dim != EMBEDDING_DIM || vec.len() != EMBEDDING_DIM {
        return Err(StorageError::ConstraintViolation(
            "embedding length must be 1024".into(),
        ));
    }
    let vec_literal = crate::pgvector::literal(vec);
    sqlx::query(
        "INSERT INTO proxima_core.embeddings
            (entity_kind, entity_id, embedding_version, model_id, vec,
             owner_principal_kind, owner_principal_id, owner_org_id)
         VALUES ($1, $2, 1, $3, $4::vector, $5, $6, $7)
         ON CONFLICT (entity_kind, entity_id, embedding_version, model_id)
         DO UPDATE SET
             vec = EXCLUDED.vec,
             owner_principal_kind = EXCLUDED.owner_principal_kind,
             owner_principal_id = EXCLUDED.owner_principal_id,
             owner_org_id = EXCLUDED.owner_org_id",
    )
    .bind(EntityKind::Fact)
    .bind(memory_id.into_inner())
    .bind(model_id)
    .bind(vec_literal)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
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
    let (owner_kind, owner_principal_id, owner_org_id) = owner_parts(owner);
    let limit = i64::try_from(limit)
        .map_err(|_| StorageError::ConstraintViolation("limit too large".into()))?;
    let rows = sqlx::query_scalar::<_, uuid::Uuid>(
        "SELECT m.memory_id
           FROM proxima_core.memories m
          WHERE m.owner_principal_kind = $1
            AND m.owner_principal_id = $2
            AND m.owner_org_id = $3
            AND m.event_id IS NOT NULL
            AND m.text IS NOT NULL
            AND m.tombstoned_at IS NULL
            AND NOT EXISTS (
                SELECT 1
                  FROM proxima_core.embeddings e
                 WHERE e.entity_kind = 'Fact'
                   AND e.entity_id = m.memory_id
                   AND e.embedding_version = 1
                   AND e.model_id = $4
            )
          ORDER BY m.created_at ASC, m.memory_id ASC
          LIMIT $5",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(model_id)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(map_err)?;
    Ok(rows.into_iter().map(MemoryId::new).collect())
}

/// Atomically claim pending Fact embedding jobs for one model.
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
    let rows = sqlx::query_as::<_, EmbeddingJobClaimRow>(
        "WITH claimed AS (
             SELECT owner_principal_kind, owner_principal_id, owner_org_id,
                    entity_kind, entity_id, model_id, embedding_version
               FROM proxima_core.embedding_jobs
              WHERE status = 'pending'
                AND model_id = $1
              ORDER BY enqueued_at ASC,
                       owner_principal_kind ASC,
                       owner_principal_id ASC,
                       owner_org_id ASC,
                       entity_kind ASC,
                       entity_id ASC,
                       embedding_version ASC
              FOR UPDATE SKIP LOCKED
              LIMIT $2
         )
         UPDATE proxima_core.embedding_jobs j
            SET status = 'processing',
                updated_at = now()
           FROM claimed
          WHERE j.owner_principal_kind = claimed.owner_principal_kind
            AND j.owner_principal_id = claimed.owner_principal_id
            AND j.owner_org_id = claimed.owner_org_id
            AND j.entity_kind = claimed.entity_kind
            AND j.entity_id = claimed.entity_id
            AND j.model_id = claimed.model_id
            AND j.embedding_version = claimed.embedding_version
        RETURNING j.owner_principal_kind, j.owner_principal_id, j.owner_org_id,
                  j.entity_kind, j.entity_id, j.model_id, j.embedding_version,
                  j.attempts",
    )
    .bind(model_id)
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
    let (owner_kind, owner_principal_id, owner_org_id) = owner_parts(&claim.owner);
    sqlx::query(
        "DELETE FROM proxima_core.embedding_jobs
          WHERE owner_principal_kind = $1
            AND owner_principal_id = $2
            AND owner_org_id = $3
            AND entity_kind = $4
            AND entity_id = $5
            AND model_id = $6
            AND embedding_version = $7",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
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
    let (owner_kind, owner_principal_id, owner_org_id) = owner_parts(&claim.owner);
    sqlx::query(
        "UPDATE proxima_core.embedding_jobs
            SET attempts = attempts + 1,
                last_error = $8,
                updated_at = now(),
                status = CASE
                    WHEN attempts + 1 >= $9
                    THEN 'failed'::proxima_core.embedding_job_status
                    ELSE 'pending'::proxima_core.embedding_job_status
                END
          WHERE owner_principal_kind = $1
            AND owner_principal_id = $2
            AND owner_org_id = $3
            AND entity_kind = $4
            AND entity_id = $5
            AND model_id = $6
            AND embedding_version = $7
            AND status = 'processing'",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
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
    let (owner_kind, owner_principal_id, owner_org_id) = owner_parts(owner);
    let result = sqlx::query(
        "WITH missing AS (
             SELECT m.memory_id
               FROM proxima_core.memories m
              WHERE m.owner_principal_kind = $1
                AND m.owner_principal_id = $2
                AND m.owner_org_id = $3
                AND m.event_id IS NOT NULL
                AND m.text IS NOT NULL
                AND m.tombstoned_at IS NULL
                AND NOT EXISTS (
                    SELECT 1
                      FROM proxima_core.embeddings e
                     WHERE e.entity_kind = 'Fact'
                       AND e.entity_id = m.memory_id
                       AND e.embedding_version = 1
                       AND e.model_id = $4
                )
              ORDER BY m.created_at ASC, m.memory_id ASC
              LIMIT $5
         )
         INSERT INTO proxima_core.embedding_jobs
             (owner_principal_kind, owner_principal_id, owner_org_id,
              entity_kind, entity_id, model_id)
         SELECT $1, $2, $3, 'Fact'::proxima_core.entity_kind, memory_id, $4
           FROM missing
         ON CONFLICT (owner_principal_kind, owner_principal_id, owner_org_id,
                      entity_kind, entity_id, model_id, embedding_version)
         DO NOTHING",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(model_id)
    .bind(limit)
    .execute(pool)
    .await
    .map_err(map_err)?;
    Ok(result.rows_affected())
}
