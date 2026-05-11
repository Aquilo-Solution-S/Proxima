use proxima_core::personality::{
    InstantiatePersonalityRequest, InstantiatePersonalityResponse, PersonalityInstanceId,
    PersonalityInstanceRow, ROOT_PERSONALITY_PERSPECTIVE_SCHEMA_ID, WakeEntryRow,
};
use proxima_core::{MemoryId, Owner, StorageError};
use sqlx::{PgPool, Row};

use super::parse::{
    parse_goal_scope, parse_model_tier, parse_row_authored_by, parse_row_execution_mode,
    parse_trigger_kind,
};
use super::rows::owner_columns;
use crate::error::map_err;

pub async fn list_personality_instances(
    pool: &PgPool,
    owner: &Owner,
    include_tombstoned: bool,
) -> Result<Vec<PersonalityInstanceRow>, StorageError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_columns(owner);
    let rows: Vec<(uuid::Uuid, uuid::Uuid, String, String)> = sqlx::query_as(
        "SELECT p.personality_instance_id,
                p.current_root_perspective_memory_id,
                m.text AS display_name,
                p.status
         FROM proxima_core.personality p
         JOIN proxima_core.memories m
           ON m.memory_id = p.current_root_perspective_memory_id
         WHERE p.owner_principal_kind = $1
           AND p.owner_principal_id = $2
           AND p.owner_org_id = $3
           AND ($4::bool OR p.status <> 'tombstoned')
         ORDER BY p.created_at, p.personality_instance_id",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(include_tombstoned)
    .fetch_all(pool)
    .await
    .map_err(map_err)?;

    let instance_ids: Vec<uuid::Uuid> = rows.iter().map(|(id, _, _, _)| *id).collect();

    let wake_rows = sqlx::query(
        "SELECT personality_instance_id,
                wake_entry_id, trigger_kind, trigger_id, label, enabled,
                execution_mode, authored_by, probability_promille, goal_scope, recipe_ref,
                model_tier, inference_target_ref, substrate_tool_palette,
                workspace_tool_palette, max_rounds, disabled_reason
         FROM proxima_core.personality_wake_entries
         WHERE owner_principal_kind = $1
           AND owner_principal_id = $2
           AND owner_org_id = $3
           AND personality_instance_id = ANY($4::uuid[])
           AND tombstoned_at IS NULL
         ORDER BY label, wake_entry_id",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(&instance_ids)
    .fetch_all(pool)
    .await
    .map_err(map_err)?;

    let mut wake_by_instance: std::collections::HashMap<uuid::Uuid, Vec<WakeEntryRow>> =
        std::collections::HashMap::with_capacity(instance_ids.len());
    for row in wake_rows {
        let pid: uuid::Uuid = row.get("personality_instance_id");
        wake_by_instance.entry(pid).or_default().push(WakeEntryRow {
            wake_entry_id: row.get("wake_entry_id"),
            trigger_kind: parse_trigger_kind(&row.get::<String, _>("trigger_kind")),
            trigger_id: row.get("trigger_id"),
            label: row.get("label"),
            enabled: row.get("enabled"),
            execution_mode: parse_row_execution_mode(&row.get::<String, _>("execution_mode")),
            authored_by: parse_row_authored_by(&row.get::<String, _>("authored_by")),
            probability_promille: u16::try_from(row.get::<i32, _>("probability_promille"))
                .unwrap_or(0),
            goal_scope: parse_goal_scope(&row.get::<String, _>("goal_scope")),
            recipe_ref: row.get("recipe_ref"),
            model_tier: parse_model_tier(&row.get::<String, _>("model_tier")),
            inference_target_ref: row.get("inference_target_ref"),
            substrate_tool_palette: row.get("substrate_tool_palette"),
            workspace_tool_palette: row.get("workspace_tool_palette"),
            max_rounds: u16::try_from(row.get::<i32, _>("max_rounds")).unwrap_or(1),
            disabled_reason: row.get("disabled_reason"),
        });
    }

    let mut out = Vec::with_capacity(rows.len());
    for (instance_id, root_memory_id, display_name, status) in rows {
        out.push(PersonalityInstanceRow {
            owner: owner.clone(),
            personality_instance_id: PersonalityInstanceId::new(instance_id),
            current_root_perspective_memory_id: MemoryId::new(root_memory_id),
            display_name,
            status,
            wake_entries: wake_by_instance.remove(&instance_id).unwrap_or_default(),
        });
    }

    Ok(out)
}

pub async fn instantiate_personality(
    pool: &PgPool,
    req: &InstantiatePersonalityRequest,
) -> Result<InstantiatePersonalityResponse, StorageError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_columns(&req.owner);
    let instance_id = uuid::Uuid::now_v7();
    let memory_id = uuid::Uuid::now_v7();
    let mut tx = pool.begin().await.map_err(map_err)?;

    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_principal_kind, owner_principal_id, owner_org_id,
             schema_id, schema_version, kind, text, operator_kind, model_id,
             prompt_version, personality_instance_id, wake_chain_depth)
         VALUES ($1, $2, $3, $4, $5, 1, 'Perspective', $6, 'Wake', 'substrate',
                 'self-v1', $7, 0)",
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(ROOT_PERSONALITY_PERSPECTIVE_SCHEMA_ID)
    .bind(&req.display_name)
    .bind(instance_id)
    .execute(&mut *tx)
    .await
    .map_err(map_err)?;

    sqlx::query(
        "INSERT INTO proxima_core.root_personality_perspective_v1
            (memory_id, display_name, purpose)
         VALUES ($1, $2, $3)",
    )
    .bind(memory_id)
    .bind(&req.display_name)
    .bind(&req.purpose)
    .execute(&mut *tx)
    .await
    .map_err(map_err)?;

    let change_seq = uuid::Uuid::now_v7();
    sqlx::query(
        "INSERT INTO proxima_core.change_event
            (seq, owner_principal_kind, owner_principal_id, owner_org_id, kind,
             entity_kind, entity_memory_id, entity_schema_id, entity_schema_version,
             entity_personality_instance_id, wake_chain_depth)
         VALUES ($1, $2, $3, $4, 'EntityAppend', 'Perspective', $5, $6, 1, $7, 0)",
    )
    .bind(change_seq)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(memory_id)
    .bind(ROOT_PERSONALITY_PERSPECTIVE_SCHEMA_ID)
    .bind(instance_id)
    .execute(&mut *tx)
    .await
    .map_err(map_err)?;

    sqlx::query(
        "INSERT INTO proxima_core.personality
            (owner_principal_kind, owner_principal_id, owner_org_id,
             personality_instance_id, current_root_perspective_memory_id,
             max_wake_chain_depth, status)
         VALUES ($1, $2, $3, $4, $5, $6, 'active')",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(instance_id)
    .bind(memory_id)
    .bind(i32::from(proxima_core::personality::MAX_WAKE_CHAIN_DEPTH))
    .execute(&mut *tx)
    .await
    .map_err(map_err)?;

    sqlx::query(
        "INSERT INTO proxima_core.personality_wake_cursor
            (owner_principal_kind, owner_principal_id, owner_org_id,
             personality_instance_id, last_considered_seq)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(instance_id)
    .bind(uuid::Uuid::nil())
    .execute(&mut *tx)
    .await
    .map_err(map_err)?;

    tx.commit().await.map_err(map_err)?;
    Ok(InstantiatePersonalityResponse {
        instance_id: PersonalityInstanceId::new(instance_id),
    })
}
