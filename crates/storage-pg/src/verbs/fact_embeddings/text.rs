use proxima_core::{EntityKind, MemoryId, Owner, StorageError};
use sqlx::{PgPool, Postgres, Transaction};

use crate::error::map_err;

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
    load_embedding_text(pool, owner, EntityKind::Fact, memory_id, &[]).await
}

/// Owner-scoped read of stored memory text for an embedding job.
///
/// Facts are encoded as `kind = 'Fact'`; derived memories carry
/// `Abstraction` / `Perspective` in `kind`.
///
/// # Errors
///
/// Returns `StorageError::Internal` for SQL failures.
pub async fn load_embedding_text(
    pool: &PgPool,
    owner: &Owner,
    _entity_kind: EntityKind,
    memory_id: MemoryId,
    _non_embeddable_schemas: &[String],
) -> Result<Option<String>, StorageError> {
    let owner_id = owner.stored_owner_id();
    sqlx::query_scalar(
        "SELECT NULLIF(btrim(c.text), '')
           FROM proxima_code.code_chunk_v1 c
           JOIN proxima_core.memory m ON m.t = c.memory_id
          WHERE c.memory_id = $1
            AND m.owner_id = $2
            AND c.state = 'Present'",
    )
    .bind(memory_id.into_inner())
    .bind(owner_id)
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
    let owner_id = owner.stored_owner_id();
    sqlx::query_scalar(
        "SELECT NULLIF(btrim(c.text), '')
           FROM proxima_code.code_chunk_v1 c
           JOIN proxima_core.memory m ON m.t = c.memory_id
          WHERE c.memory_id = $1
            AND m.owner_id = $2
            AND c.state = 'Present'",
    )
    .bind(memory_id.into_inner())
    .bind(owner_id)
    .fetch_optional(tx.as_mut())
    .await
    .map_err(map_err)
}
