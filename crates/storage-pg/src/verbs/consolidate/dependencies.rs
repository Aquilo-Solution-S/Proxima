use proxima_core::{
    CORE_DEPENDS_ON_RELATION, MemoryDependency, MemoryId, Owner, SchemaId, StorageError,
};
use sqlx::{PgPool, Row};

use crate::error::map_err;

pub async fn list_memory_dependencies(
    pool: &PgPool,
    owner: &Owner,
    source_memory_id: MemoryId,
) -> Result<Vec<MemoryDependency>, StorageError> {
    let (owner_kind, owner_principal_id) = owner.columns();
    let rows = sqlx::query(
        "SELECT e.target_memory_id, m.schema_id
         FROM proxima_core.edges e
         JOIN proxima_core.memories m
           ON m.memory_id = e.target_memory_id
          AND m.tombstoned_at IS NULL
         WHERE EXISTS (
                    SELECT 1
                      FROM proxima_core.entity_owner eo
                     WHERE eo.entity_id = e.source_memory_id
                       AND eo.owner_principal_kind = $1
                       AND eo.owner_principal_id = $2
                       AND eo.is_home
               )
           AND e.relation = $3
           AND e.source_kind IN ('Fact', 'Abstraction', 'Perspective')
           AND e.source_memory_id = $4
           AND e.target_memory_id IS NOT NULL
         ORDER BY e.created_at, e.edge_id",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(CORE_DEPENDS_ON_RELATION)
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
