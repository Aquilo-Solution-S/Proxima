use proxima_core::llm::EMBEDDING_DIM;
use proxima_core::{EntityKind, MemoryId, Owner, OwnerPrincipalKind, Principal, StorageError};
use sqlx::{PgPool, Postgres, Transaction};

use crate::error::map_err;

fn owner_parts(owner: &Owner) -> (OwnerPrincipalKind, uuid::Uuid, uuid::Uuid) {
    let (owner_kind, owner_principal_id) = match &owner.principal {
        Principal::User(user) => (OwnerPrincipalKind::User, user.into_inner()),
        Principal::Group(group) => (OwnerPrincipalKind::Group, group.into_inner()),
    };
    (owner_kind, owner_principal_id, owner.org_id.into_inner())
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
