use proxima_core::{MemoryDependency, MemoryId, Owner, SchemaId, StorageError};
use sqlx::{PgPool, Row};

use crate::error::map_err;

/// What one memory points at.
///
/// A dependency used to be its own relation; it is now simply a reference the
/// memory's payload declared. The index answers "is there a connection"; the
/// payload answers "what is it".
///
/// # Errors
///
/// Returns [`StorageError`] when the index read fails.
pub async fn list_memory_dependencies(
    pool: &PgPool,
    owner: &Owner,
    source_memory_id: MemoryId,
) -> Result<Vec<MemoryDependency>, StorageError> {
    let (owner_kind, owner_id) = owner.columns();
    let rows = sqlx::query(
        "SELECT e.target_id AS target_memory_id, m.schema_id
         FROM proxima_core.edges e
         JOIN proxima_core.memories m
           ON m.memory_id = e.target_id
          AND m.tombstoned_at IS NULL
         WHERE e.kind = 'reference'::proxima_core.edge_kind
           AND e.source_kind IN ('Fact'::proxima_core.edge_endpoint_kind,
                                 'Abstraction'::proxima_core.edge_endpoint_kind,
                                 'Perspective'::proxima_core.edge_endpoint_kind)
           AND e.target_kind <> 'Goal'::proxima_core.edge_endpoint_kind
           AND e.source_id = $3
           AND e.owner_kind = $1
           AND e.owner_id IS NOT DISTINCT FROM $2
         ORDER BY e.created_at, e.target_id",
    )
    .bind(owner_kind)
    .bind(owner_id)
    .bind(source_memory_id.into_inner())
    .fetch_all(pool)
    .await
    .map_err(map_err)?;
    Ok(rows
        .into_iter()
        .map(|row| MemoryDependency {
            dependency_memory_id: MemoryId::new(row.get("target_memory_id")),
            dependency_schema_id: SchemaId::new(row.get::<String, _>("schema_id")),
        })
        .collect())
}
