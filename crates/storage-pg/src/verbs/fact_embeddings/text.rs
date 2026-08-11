use proxima_core::{EntityKind, MemoryId, Owner, StorageError};
use sqlx::{PgPool, Postgres, Transaction};

use crate::error::map_err;

use super::owner_parts;

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
    let (owner_kind, owner_id) = owner_parts(owner);
    // SQL-POLICY: fixed-fragment — the only interpolation is the shared
    // entity-owner-union constant; every value is bound.
    sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT text
           FROM proxima_core.memories
          WHERE memory_id = $1
            AND EXISTS (
                SELECT 1
                  FROM {eo_union} eo
                 WHERE eo.entity_id = memory_id
                   AND eo.owner_kind = $2
                   AND eo.owner_id = $3
)
            AND kind IS NULL
            AND tombstoned_at IS NULL",
        eo_union = crate::verbs::query::entity_owner_union(),
    )))
    .bind(memory_id.into_inner())
    .bind(owner_kind)
    .bind(owner_id)
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
    non_embeddable_schemas: &[String],
) -> Result<Option<String>, StorageError> {
    let (owner_kind, owner_id) = owner_parts(owner);
    // SQL-POLICY: fixed-fragment — the only interpolation is the shared
    // entity-owner-union constant; every value is bound.
    sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT text
           FROM proxima_core.memories
          WHERE memory_id = $1
            AND EXISTS (
                SELECT 1
                  FROM {eo_union} eo
                 WHERE eo.entity_id = memory_id
                   AND eo.owner_kind = $2
                   AND eo.owner_id = $3
)
            AND text IS NOT NULL
            AND tombstoned_at IS NULL
            AND schema_id <> ALL($5::text[])
            AND (
                ($4 = 'Fact'::proxima_core.entity_kind
                 AND kind IS NULL)
                OR kind = $4
            )",
        eo_union = crate::verbs::query::entity_owner_union(),
    )))
    .bind(memory_id.into_inner())
    .bind(owner_kind)
    .bind(owner_id)
    .bind(entity_kind)
    .bind(non_embeddable_schemas)
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
    let (owner_kind, owner_id) = owner_parts(owner);
    // SQL-POLICY: fixed-fragment — the only interpolation is the shared
    // entity-owner-union constant; every value is bound.
    sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT text
           FROM proxima_core.memories
          WHERE memory_id = $1
            AND EXISTS (
                SELECT 1
                  FROM {eo_union} eo
                 WHERE eo.entity_id = memory_id
                   AND eo.owner_kind = $2
                   AND eo.owner_id = $3
)
            AND kind IS NULL
            AND tombstoned_at IS NULL",
        eo_union = crate::verbs::query::entity_owner_union(),
    )))
    .bind(memory_id.into_inner())
    .bind(owner_kind)
    .bind(owner_id)
    .fetch_optional(tx.as_mut())
    .await
    .map_err(map_err)
}
