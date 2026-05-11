use proxima_core::personality::{
    PersonalityInstanceId, SetWakeEntriesRequest, SetWakeEntriesResponse, WakeDispatchEntryRow,
    WakeEntryDraft,
};
use proxima_core::{MemoryId, Owner, StorageError};
use sqlx::{PgPool, Row};

use super::parse::{
    model_tier_str, owner_from_parts, parse_execution_mode, parse_goal_scope, parse_model_tier,
    parse_row_authored_by, parse_trigger_kind,
};
use super::rows::owner_columns;
use crate::error::map_err;

#[derive(sqlx::FromRow)]
struct WakeEntryJoinRow {
    owner_principal_kind: String,
    owner_principal_id: uuid::Uuid,
    owner_org_id: uuid::Uuid,
    personality_instance_id: uuid::Uuid,
    current_root_perspective_memory_id: uuid::Uuid,
    max_wake_chain_depth: i32,
    last_considered_seq: uuid::Uuid,
    wake_entry_id: uuid::Uuid,
    trigger_kind: String,
    trigger_id: String,
    label: String,
    enabled: bool,
    execution_mode: String,
    authored_by: String,
    probability_promille: i32,
    goal_scope: String,
    recipe_ref: String,
    model_tier: String,
    inference_target_ref: Option<String>,
    substrate_tool_palette: Vec<String>,
    workspace_tool_palette: Vec<String>,
    max_rounds: i32,
}

async fn replace_wake_entries_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    req: &SetWakeEntriesRequest,
) -> Result<SetWakeEntriesResponse, StorageError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_columns(&req.owner);
    let result = sqlx::query(
        "UPDATE proxima_core.personality_wake_entries
         SET tombstoned_at = now(), updated_at = now()
         WHERE owner_principal_kind = $1
           AND owner_principal_id = $2
           AND owner_org_id = $3
           AND personality_instance_id = $4
           AND tombstoned_at IS NULL",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(req.personality_instance_id.into_inner())
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    let _ = result;

    let active_parent = sqlx::query(
        "UPDATE proxima_core.personality
         SET updated_at = now()
         WHERE owner_principal_kind = $1
           AND owner_principal_id = $2
           AND owner_org_id = $3
           AND personality_instance_id = $4
           AND status <> 'tombstoned'",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(req.personality_instance_id.into_inner())
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    if active_parent.rows_affected() == 0 {
        return Err(StorageError::NotFound);
    }

    for entry in &req.entries {
        upsert_wake_entry(tx, &req.owner, entry).await?;
    }
    Ok(SetWakeEntriesResponse {
        active_entries: u32::try_from(req.entries.len()).unwrap_or(u32::MAX),
    })
}

pub async fn set_wake_entries(
    pool: &PgPool,
    req: &SetWakeEntriesRequest,
) -> Result<SetWakeEntriesResponse, StorageError> {
    let mut tx = pool.begin().await.map_err(map_err)?;
    let resp = replace_wake_entries_in_tx(&mut tx, req).await?;
    tx.commit().await.map_err(map_err)?;
    Ok(resp)
}

async fn read_wake_entries_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    owner: &Owner,
    pid: PersonalityInstanceId,
) -> Result<Vec<WakeEntryDraft>, StorageError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_columns(owner);
    let rows = sqlx::query(
        "SELECT wake_entry_id, trigger_kind, trigger_id, label, enabled,
                execution_mode, authored_by, probability_promille, goal_scope, recipe_ref,
                model_tier, inference_target_ref, substrate_tool_palette,
                workspace_tool_palette, max_rounds
         FROM proxima_core.personality_wake_entries
         WHERE owner_principal_kind = $1
           AND owner_principal_id = $2
           AND owner_org_id = $3
           AND personality_instance_id = $4
           AND tombstoned_at IS NULL
         ORDER BY label, wake_entry_id",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(pid.into_inner())
    .fetch_all(&mut **tx)
    .await
    .map_err(map_err)?;

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        out.push(WakeEntryDraft {
            wake_entry_id: row.get("wake_entry_id"),
            personality_instance_id: pid,
            trigger_kind: parse_trigger_kind(&row.get::<String, _>("trigger_kind")),
            trigger_id: row.get("trigger_id"),
            label: row.get("label"),
            enabled: row.get("enabled"),
            execution_mode: parse_execution_mode(&row.get::<String, _>("execution_mode")),
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
        });
    }
    Ok(out)
}

pub async fn set_wake_entries_within(
    pool: &PgPool,
    owner: &Owner,
    personality_instance_id: PersonalityInstanceId,
    mutate: proxima_core::WakeEntriesMutator,
) -> Result<SetWakeEntriesResponse, StorageError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_columns(owner);
    let mut tx = pool.begin().await.map_err(map_err)?;

    // Lock the personality row to serialise concurrent granular ops.
    let locked: Option<uuid::Uuid> = sqlx::query_scalar(
        "SELECT personality_instance_id
         FROM proxima_core.personality
         WHERE owner_principal_kind = $1
           AND owner_principal_id = $2
           AND owner_org_id = $3
           AND personality_instance_id = $4
           AND status <> 'tombstoned'
         FOR UPDATE",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(personality_instance_id.into_inner())
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_err)?;
    if locked.is_none() {
        return Err(StorageError::NotFound);
    }

    let current = read_wake_entries_in_tx(&mut tx, owner, personality_instance_id).await?;

    let new_entries = mutate(&current).map_err(StorageError::Internal)?;

    let req = SetWakeEntriesRequest {
        owner: owner.clone(),
        personality_instance_id,
        entries: new_entries,
    };
    let resp = replace_wake_entries_in_tx(&mut tx, &req).await?;
    tx.commit().await.map_err(map_err)?;
    Ok(resp)
}

pub async fn tombstone_personality(
    pool: &PgPool,
    req: &proxima_core::TombstonePersonalityRequest,
) -> Result<proxima_core::TombstonePersonalityResponse, StorageError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_columns(&req.owner);
    let mut tx = pool.begin().await.map_err(map_err)?;

    let result = sqlx::query(
        "UPDATE proxima_core.personality
         SET status = 'tombstoned',
             tombstoned_at = now(),
             updated_at = now()
         WHERE owner_principal_kind = $1
           AND owner_principal_id = $2
           AND owner_org_id = $3
           AND personality_instance_id = $4
           AND status <> 'tombstoned'",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(req.personality_instance_id.into_inner())
    .execute(&mut *tx)
    .await
    .map_err(map_err)?;

    if result.rows_affected() == 1 {
        tx.commit().await.map_err(map_err)?;
        return Ok(proxima_core::TombstonePersonalityResponse {
            status: "tombstoned".into(),
            idempotent_replay: false,
        });
    }

    let existing: Option<(String,)> = sqlx::query_as(
        "SELECT status
         FROM proxima_core.personality
         WHERE owner_principal_kind = $1
           AND owner_principal_id = $2
           AND owner_org_id = $3
           AND personality_instance_id = $4",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(req.personality_instance_id.into_inner())
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_err)?;

    tx.commit().await.map_err(map_err)?;

    match existing {
        Some((status,)) if status == "tombstoned" => {
            Ok(proxima_core::TombstonePersonalityResponse {
                status: "tombstoned".into(),
                idempotent_replay: true,
            })
        }
        Some(_) => {
            unreachable!("UPDATE excluded only tombstoned rows; non-tombstoned must have hit")
        }
        None => Err(StorageError::NotFound),
    }
}

pub async fn list_active_wake_entries(
    pool: &PgPool,
) -> Result<Vec<WakeDispatchEntryRow>, StorageError> {
    let rows: Vec<WakeEntryJoinRow> = sqlx::query_as(
        "SELECT p.owner_principal_kind,
                p.owner_principal_id,
                p.owner_org_id,
                p.personality_instance_id,
                p.current_root_perspective_memory_id,
                p.max_wake_chain_depth,
                cur.last_considered_seq,
                e.wake_entry_id,
                e.trigger_kind,
                e.trigger_id,
                e.label,
                e.enabled,
                e.execution_mode,
                e.authored_by,
                e.probability_promille,
                e.goal_scope,
                e.recipe_ref,
                e.model_tier,
                e.inference_target_ref,
                e.substrate_tool_palette,
                e.workspace_tool_palette,
                e.max_rounds
         FROM proxima_core.personality p
         JOIN proxima_core.personality_wake_cursor cur
           ON cur.owner_principal_kind = p.owner_principal_kind
          AND cur.owner_principal_id = p.owner_principal_id
          AND cur.owner_org_id = p.owner_org_id
          AND cur.personality_instance_id = p.personality_instance_id
         JOIN proxima_core.personality_wake_entries e
           ON e.owner_principal_kind = p.owner_principal_kind
          AND e.owner_principal_id = p.owner_principal_id
          AND e.owner_org_id = p.owner_org_id
          AND e.personality_instance_id = p.personality_instance_id
         WHERE p.status = 'active'
           AND e.enabled
           AND e.tombstoned_at IS NULL
         ORDER BY p.owner_principal_kind, p.owner_principal_id, p.personality_instance_id, e.created_at",
    )
    .fetch_all(pool)
    .await
    .map_err(map_err)?;

    Ok(rows
        .into_iter()
        .map(|row| WakeDispatchEntryRow {
            owner: owner_from_parts(
                &row.owner_principal_kind,
                row.owner_principal_id,
                row.owner_org_id,
            ),
            personality_instance_id: PersonalityInstanceId::new(row.personality_instance_id),
            current_root_perspective_memory_id: MemoryId::new(
                row.current_root_perspective_memory_id,
            ),
            max_wake_chain_depth: u16::try_from(row.max_wake_chain_depth).unwrap_or(0),
            last_considered_seq: row.last_considered_seq,
            wake_entry: WakeEntryDraft {
                wake_entry_id: row.wake_entry_id,
                personality_instance_id: PersonalityInstanceId::new(row.personality_instance_id),
                trigger_kind: parse_trigger_kind(&row.trigger_kind),
                trigger_id: row.trigger_id,
                label: row.label,
                enabled: row.enabled,
                execution_mode: parse_execution_mode(&row.execution_mode),
                authored_by: parse_row_authored_by(&row.authored_by),
                probability_promille: u16::try_from(row.probability_promille).unwrap_or(0),
                goal_scope: parse_goal_scope(&row.goal_scope),
                recipe_ref: row.recipe_ref,
                model_tier: parse_model_tier(&row.model_tier),
                inference_target_ref: row.inference_target_ref,
                substrate_tool_palette: row.substrate_tool_palette,
                workspace_tool_palette: row.workspace_tool_palette,
                max_rounds: u16::try_from(row.max_rounds).unwrap_or(1),
            },
        })
        .collect())
}

async fn upsert_wake_entry(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    owner: &Owner,
    entry: &WakeEntryDraft,
) -> Result<(), StorageError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_columns(owner);
    sqlx::query(
        "INSERT INTO proxima_core.personality_wake_entries
            (owner_principal_kind, owner_principal_id, owner_org_id,
             personality_instance_id, wake_entry_id, trigger_kind, trigger_id,
             label, enabled, execution_mode, authored_by, probability_promille,
             goal_scope, recipe_ref, model_tier, inference_target_ref, substrate_tool_palette,
             workspace_tool_palette, max_rounds)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11,
                 $12, $13, $14, $15, $16, $17, $18, $19)
         ON CONFLICT (
             owner_principal_kind,
             owner_principal_id,
             owner_org_id,
             personality_instance_id,
             wake_entry_id
         ) DO UPDATE SET
             trigger_kind = EXCLUDED.trigger_kind,
             trigger_id = EXCLUDED.trigger_id,
             label = EXCLUDED.label,
             enabled = EXCLUDED.enabled,
             execution_mode = EXCLUDED.execution_mode,
             authored_by = EXCLUDED.authored_by,
             probability_promille = EXCLUDED.probability_promille,
             goal_scope = EXCLUDED.goal_scope,
             recipe_ref = EXCLUDED.recipe_ref,
             model_tier = EXCLUDED.model_tier,
             inference_target_ref = EXCLUDED.inference_target_ref,
             substrate_tool_palette = EXCLUDED.substrate_tool_palette,
             workspace_tool_palette = EXCLUDED.workspace_tool_palette,
             max_rounds = EXCLUDED.max_rounds,
             disabled_reason = NULL,
             tombstoned_at = NULL,
             updated_at = now()",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(entry.personality_instance_id.into_inner())
    .bind(entry.wake_entry_id)
    .bind(entry.trigger_kind.as_str())
    .bind(&entry.trigger_id)
    .bind(&entry.label)
    .bind(entry.enabled)
    .bind(entry.execution_mode.as_str())
    .bind(entry.authored_by.as_str())
    .bind(i32::from(entry.probability_promille))
    .bind(entry.goal_scope.as_str())
    .bind(&entry.recipe_ref)
    .bind(model_tier_str(entry.model_tier))
    .bind(&entry.inference_target_ref)
    .bind(&entry.substrate_tool_palette)
    .bind(&entry.workspace_tool_palette)
    .bind(i32::from(entry.max_rounds))
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(())
}
