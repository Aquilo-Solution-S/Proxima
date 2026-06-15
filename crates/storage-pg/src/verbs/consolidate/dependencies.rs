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
    let (owner_kind, owner_principal_id, _owner_org_id) = owner.columns();
    let rows = sqlx::query(
        "SELECT e.target_memory_id, m.schema_id
         FROM proxima_core.edges e
         JOIN proxima_core.memories m
	           ON m.memory_id = e.target_memory_id
	          AND m.owner_principal_kind = e.owner_principal_kind
	          AND m.owner_principal_id = e.owner_principal_id
	          AND m.tombstoned_at IS NULL
         WHERE e.owner_principal_kind = $1
           AND e.owner_principal_id = $2
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

pub async fn has_satisfied_code_test_request(
    pool: &PgPool,
    owner: &Owner,
    test_request_memory_id: MemoryId,
) -> Result<bool, StorageError> {
    let (owner_kind, owner_principal_id, _owner_org_id) = owner.columns();
    let satisfied: bool = sqlx::query_scalar(
        "WITH required AS (
             SELECT criterion->>'key' AS criterion_key
             FROM proxima_code.test_request_v1 t
	             JOIN proxima_core.memories mt
	               ON mt.memory_id = t.memory_id
             CROSS JOIN jsonb_array_elements(t.criteria_json) criterion
             WHERE t.memory_id = $3
	               AND mt.owner_principal_kind = $1
	               AND mt.owner_principal_id = $2
	               AND mt.tombstoned_at IS NULL
               AND COALESCE((criterion->>'required')::boolean, false)
         ),
         evidence AS (
             SELECT v.criterion_key, v.status::text AS status
             FROM proxima_core.edges e
             JOIN proxima_code.verification_evidence_v1 v
               ON v.memory_id = e.source_memory_id
	             JOIN proxima_core.memories mv
	               ON mv.memory_id = v.memory_id
	              AND mv.owner_principal_kind = e.owner_principal_kind
	              AND mv.owner_principal_id = e.owner_principal_id
	              AND mv.tombstoned_at IS NULL
             WHERE e.owner_principal_kind = $1
               AND e.owner_principal_id = $2
               AND e.relation = 'core/derived-from'
               AND e.source_kind = 'Fact'
               AND e.target_kind = 'Fact'
               AND e.target_memory_id = $3
         )
         SELECT EXISTS(SELECT 1 FROM required)
            AND NOT EXISTS(SELECT 1 FROM evidence WHERE status = 'failed')
            AND NOT EXISTS(
                SELECT 1
                FROM required r
                WHERE NOT EXISTS(
                    SELECT 1
                    FROM evidence e
                    WHERE e.criterion_key = r.criterion_key
                      AND e.status = 'passed'
                )
            )",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(test_request_memory_id.into_inner())
    .fetch_one(pool)
    .await
    .map_err(map_err)?;
    Ok(satisfied)
}
