use proxima_core::budget::BudgetExhaustionPolicy;
use proxima_core::personality::{
    InstantiatePersonalityRequest, InstantiatePersonalityResponse, PersonalityInstanceId,
    PersonalityInstanceRow, PersonalityStatus, ROOT_PERSONALITY_PERSPECTIVE_SCHEMA_ID,
    WakeEntryAuthoredBy, WakeEntryExecutionMode, WakeEntryGoalScope, WakeEntryRow,
    WakeEntryTriggerKind,
};
use proxima_core::{MemoryId, ModelTier, Owner, OwnerPrincipalKind, StorageError};
use sqlx::PgPool;

use super::rows::owner_columns;
use crate::error::map_err;

pub async fn list_personality_instances(
    pool: &PgPool,
    owner: &Owner,
    include_tombstoned: bool,
) -> Result<Vec<PersonalityInstanceRow>, StorageError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_columns(owner);
    let rows: Vec<(uuid::Uuid, uuid::Uuid, String, PersonalityStatus)> = sqlx::query_as(
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
           AND ($4::bool OR p.status <> 'tombstoned'::proxima_core.personality_status)
         ORDER BY p.created_at, p.personality_instance_id",
    )
    .bind(owner_kind as OwnerPrincipalKind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(include_tombstoned)
    .fetch_all(pool)
    .await
    .map_err(map_err)?;

    let instance_ids: Vec<uuid::Uuid> = rows.iter().map(|(id, _, _, _)| *id).collect();

    let wake_rows: Vec<WakeEntryProjectionRow> = sqlx::query_as(
        "SELECT personality_instance_id,
                wake_entry_id,
                trigger_kind,
                trigger_id, label, enabled,
                execution_mode,
                authored_by,
                probability_promille,
                goal_scope,
                instructions,
                model_tier,
                inference_target_ref, substrate_tool_palette,
                workspace_tool_palette, max_rounds,
                budgeter_personality_instance_id,
                budget_extension_rounds,
                budget_hard_cap_rounds,
                budget_progress_contract,
                disabled_reason
           FROM proxima_core.personality_wake_entries
           WHERE owner_principal_kind = $1
             AND owner_principal_id = $2
             AND owner_org_id = $3
             AND personality_instance_id = ANY($4::uuid[])
             AND tombstoned_at IS NULL
           ORDER BY label, wake_entry_id",
    )
    .bind(owner_kind as OwnerPrincipalKind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(&instance_ids[..])
    .fetch_all(pool)
    .await
    .map_err(map_err)?;

    let mut wake_by_instance: std::collections::HashMap<uuid::Uuid, Vec<WakeEntryRow>> =
        std::collections::HashMap::with_capacity(instance_ids.len());
    for row in wake_rows {
        let pid: uuid::Uuid = row.personality_instance_id;
        wake_by_instance.entry(pid).or_default().push(WakeEntryRow {
            wake_entry_id: row.wake_entry_id,
            trigger_kind: row.trigger_kind,
            trigger_id: row.trigger_id,
            label: row.label,
            enabled: row.enabled,
            execution_mode: row.execution_mode,
            authored_by: row.authored_by,
            probability_promille: u16::try_from(row.probability_promille).unwrap_or(0),
            goal_scope: row.goal_scope,
            instructions: row.instructions,
            model_tier: row.model_tier,
            inference_target_ref: row.inference_target_ref,
            substrate_tool_palette: row.substrate_tool_palette,
            workspace_tool_palette: row.workspace_tool_palette,
            max_rounds: u16::try_from(row.max_rounds).unwrap_or(1),
            budget_policy: budget_policy_from_parts(
                row.budgeter_personality_instance_id,
                row.budget_extension_rounds,
                row.budget_hard_cap_rounds,
                row.budget_progress_contract,
            ),
            disabled_reason: row.disabled_reason,
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

#[derive(sqlx::FromRow)]
struct WakeEntryProjectionRow {
    personality_instance_id: uuid::Uuid,
    wake_entry_id: uuid::Uuid,
    trigger_kind: WakeEntryTriggerKind,
    trigger_id: String,
    label: String,
    enabled: bool,
    execution_mode: WakeEntryExecutionMode,
    authored_by: WakeEntryAuthoredBy,
    probability_promille: i32,
    goal_scope: WakeEntryGoalScope,
    instructions: String,
    model_tier: ModelTier,
    inference_target_ref: Option<String>,
    substrate_tool_palette: Vec<String>,
    workspace_tool_palette: Vec<String>,
    max_rounds: i32,
    budgeter_personality_instance_id: Option<uuid::Uuid>,
    budget_extension_rounds: i32,
    budget_hard_cap_rounds: i32,
    budget_progress_contract: String,
    disabled_reason: Option<String>,
}

fn budget_policy_from_parts(
    budgeter_personality_instance_id: Option<uuid::Uuid>,
    budget_extension_rounds: i32,
    budget_hard_cap_rounds: i32,
    budget_progress_contract: String,
) -> Option<BudgetExhaustionPolicy> {
    budgeter_personality_instance_id.map(|budgeter_personality_instance_id| {
        BudgetExhaustionPolicy {
            budgeter_personality_instance_id,
            budget_extension_rounds: u16::try_from(budget_extension_rounds).unwrap_or(0),
            budget_hard_cap_rounds: u16::try_from(budget_hard_cap_rounds).unwrap_or(0),
            budget_progress_contract,
        }
    })
}

pub async fn instantiate_personality(
    pool: &PgPool,
    req: &InstantiatePersonalityRequest,
) -> Result<InstantiatePersonalityResponse, StorageError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_columns(&req.owner);
    let instance_id = uuid::Uuid::now_v7();
    let memory_id = uuid::Uuid::now_v7();
    let mut tx = pool.begin().await.map_err(map_err)?;

    sqlx::query!(
        r#"INSERT INTO proxima_core.memories
            (memory_id, owner_principal_kind, owner_principal_id, owner_org_id,
             schema_id, schema_version, kind, text, operator_kind, model_id,
             prompt_version, personality_instance_id, wake_chain_depth)
         VALUES ($1, $2, $3, $4, $5, 1, 'Perspective', $6, 'Wake', 'substrate',
                 'self-v1', $7, 0)"#,
        memory_id,
        owner_kind as OwnerPrincipalKind,
        owner_principal_id,
        owner_org_id,
        ROOT_PERSONALITY_PERSPECTIVE_SCHEMA_ID,
        &req.display_name,
        instance_id,
    )
    .execute(&mut *tx)
    .await
    .map_err(map_err)?;

    sqlx::query!(
        r#"INSERT INTO proxima_core.root_personality_perspective_v1
            (memory_id, display_name, purpose)
         VALUES ($1, $2, $3)"#,
        memory_id,
        &req.display_name,
        &req.purpose,
    )
    .execute(&mut *tx)
    .await
    .map_err(map_err)?;

    let change_seq = uuid::Uuid::now_v7();
    sqlx::query!(
        r#"INSERT INTO proxima_core.change_event
            (seq, owner_principal_kind, owner_principal_id, owner_org_id, kind,
             entity_kind, entity_memory_id, entity_schema_id, entity_schema_version,
             entity_personality_instance_id, wake_chain_depth)
         VALUES ($1, $2, $3, $4, 'EntityAppend', 'Perspective', $5, $6, 1, $7, 0)"#,
        change_seq,
        owner_kind as OwnerPrincipalKind,
        owner_principal_id,
        owner_org_id,
        memory_id,
        ROOT_PERSONALITY_PERSPECTIVE_SCHEMA_ID,
        instance_id,
    )
    .execute(&mut *tx)
    .await
    .map_err(map_err)?;

    sqlx::query(
        "INSERT INTO proxima_core.personality
            (owner_principal_kind, owner_principal_id, owner_org_id,
             personality_instance_id, current_root_perspective_memory_id,
             max_wake_chain_depth, status)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(instance_id)
    .bind(memory_id)
    .bind(i32::from(proxima_core::personality::MAX_WAKE_CHAIN_DEPTH))
    .bind(PersonalityStatus::Active)
    .execute(&mut *tx)
    .await
    .map_err(map_err)?;

    sqlx::query!(
        r#"INSERT INTO proxima_core.personality_wake_cursor
            (owner_principal_kind, owner_principal_id, owner_org_id,
             personality_instance_id, last_considered_seq)
         VALUES ($1, $2, $3, $4, $5)"#,
        owner_kind as OwnerPrincipalKind,
        owner_principal_id,
        owner_org_id,
        instance_id,
        uuid::Uuid::nil(),
    )
    .execute(&mut *tx)
    .await
    .map_err(map_err)?;

    tx.commit().await.map_err(map_err)?;
    Ok(InstantiatePersonalityResponse {
        instance_id: PersonalityInstanceId::new(instance_id),
    })
}
