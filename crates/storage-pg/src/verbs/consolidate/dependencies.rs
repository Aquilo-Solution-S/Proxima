use proxima_core::{
    BlockedWakeCandidate, CORE_DEPENDS_ON_RELATION, MemoryDependency, MemoryId, Owner,
    PersonalityInstanceId, SchemaId, StorageError,
};
use sqlx::{PgPool, Row};

use crate::error::map_err;

use super::rows::owner_columns;

pub async fn list_memory_dependencies(
    pool: &PgPool,
    owner: &Owner,
    source_memory_id: MemoryId,
) -> Result<Vec<MemoryDependency>, StorageError> {
    let (owner_kind, owner_principal_id, _owner_org_id) = owner_columns(owner);
    let rows = sqlx::query(
        "SELECT e.target_memory_id, m.schema_id
         FROM proxima_core.edges e
         JOIN proxima_core.memories m
           ON m.memory_id = e.target_memory_id
          AND m.owner_principal_kind = e.owner_principal_kind
          AND m.owner_principal_id = e.owner_principal_id
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

pub async fn has_successful_core_workspace_run_derived_from(
    pool: &PgPool,
    owner: &Owner,
    source_memory_id: MemoryId,
) -> Result<bool, StorageError> {
    let (owner_kind, owner_principal_id, _owner_org_id) = owner_columns(owner);
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(
             SELECT 1
             FROM proxima_core.edges e
             JOIN proxima_core.workspace_run_v1 r
               ON r.memory_id = e.source_memory_id
             JOIN proxima_core.personality_wake_invocations i
               ON i.invocation_id = r.wake_invocation_id
             WHERE e.owner_principal_kind = $1
               AND e.owner_principal_id = $2
               AND e.relation = 'core/derived-from'
               AND e.source_kind = 'Fact'
               AND e.target_memory_id = $3
               AND i.status = 'succeeded'
         )",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(source_memory_id.into_inner())
    .fetch_one(pool)
    .await
    .map_err(map_err)?;
    Ok(exists)
}

pub async fn has_satisfied_code_test_request(
    pool: &PgPool,
    owner: &Owner,
    test_request_memory_id: MemoryId,
) -> Result<bool, StorageError> {
    let (owner_kind, owner_principal_id, _owner_org_id) = owner_columns(owner);
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

pub async fn upsert_blocked_wake_candidate(
    pool: &PgPool,
    candidate: &BlockedWakeCandidate,
) -> Result<(), StorageError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_columns(&candidate.owner);
    sqlx::query(
        "INSERT INTO proxima_core.blocked_wake_candidates
            (owner_principal_kind, owner_principal_id, owner_org_id,
             personality_instance_id, wake_entry_id, change_event_seq,
             triggering_memory_id, dependency_memory_id, dependency_schema_id, reason)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
         ON CONFLICT (owner_principal_kind, owner_principal_id, owner_org_id,
                      personality_instance_id, wake_entry_id, change_event_seq)
         DO UPDATE SET
             triggering_memory_id = EXCLUDED.triggering_memory_id,
             dependency_memory_id = EXCLUDED.dependency_memory_id,
             dependency_schema_id = EXCLUDED.dependency_schema_id,
             reason = EXCLUDED.reason,
             updated_at = now()",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(candidate.personality_instance_id.into_inner())
    .bind(candidate.wake_entry_id)
    .bind(candidate.change_event_seq)
    .bind(candidate.triggering_memory_id.into_inner())
    .bind(candidate.dependency_memory_id.into_inner())
    .bind(candidate.dependency_schema_id.as_str())
    .bind(&candidate.reason)
    .execute(pool)
    .await
    .map_err(map_err)?;
    Ok(())
}

pub async fn list_blocked_wake_candidates(
    pool: &PgPool,
    owner: &Owner,
    personality_instance_id: PersonalityInstanceId,
    limit: usize,
) -> Result<Vec<BlockedWakeCandidate>, StorageError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_columns(owner);
    let rows = sqlx::query(
        "SELECT wake_entry_id, change_event_seq, triggering_memory_id,
                dependency_memory_id, dependency_schema_id, reason
         FROM proxima_core.blocked_wake_candidates
         WHERE owner_principal_kind = $1
           AND owner_principal_id = $2
           AND owner_org_id = $3
           AND personality_instance_id = $4
         ORDER BY updated_at, change_event_seq
         LIMIT $5",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(personality_instance_id.into_inner())
    .bind(i64::try_from(limit).unwrap_or(i64::MAX))
    .fetch_all(pool)
    .await
    .map_err(map_err)?;
    Ok(rows
        .into_iter()
        .map(|row| BlockedWakeCandidate {
            owner: owner.clone(),
            personality_instance_id,
            wake_entry_id: row.get("wake_entry_id"),
            change_event_seq: row.get("change_event_seq"),
            triggering_memory_id: MemoryId::new(row.get("triggering_memory_id")),
            dependency_memory_id: MemoryId::new(row.get("dependency_memory_id")),
            dependency_schema_id: SchemaId::new(row.get::<String, _>("dependency_schema_id")),
            reason: row.get("reason"),
        })
        .collect())
}

pub async fn delete_blocked_wake_candidate(
    pool: &PgPool,
    owner: &Owner,
    personality_instance_id: PersonalityInstanceId,
    wake_entry_id: uuid::Uuid,
    change_event_seq: uuid::Uuid,
) -> Result<(), StorageError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_columns(owner);
    sqlx::query(
        "DELETE FROM proxima_core.blocked_wake_candidates
         WHERE owner_principal_kind = $1
           AND owner_principal_id = $2
           AND owner_org_id = $3
           AND personality_instance_id = $4
           AND wake_entry_id = $5
           AND change_event_seq = $6",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(personality_instance_id.into_inner())
    .bind(wake_entry_id)
    .bind(change_event_seq)
    .execute(pool)
    .await
    .map_err(map_err)?;
    Ok(())
}
