//! Personality wake/decide/write storage helpers.

#![allow(
    clippy::missing_errors_doc,
    clippy::too_many_lines,
    clippy::type_complexity
)]

use proxima_core::personality::{
    AbstractionRow, ChangeEventForWake, FactRow, InstantiatePersonalityRequest,
    InstantiatePersonalityResponse, ListWakeInvocationsRequest, MemorySnapshot,
    PersonalityInstanceId, PersonalityInstanceRow, PersonalityRef, PersonalityWriteOutcome,
    PersonalityWriteRequest, ROOT_PERSONALITY_PERSPECTIVE_SCHEMA_ID, SetWakeEntriesRequest,
    SetWakeEntriesResponse, SidecarSpec, WakeChainDepth, WakeDispatchEntryRow, WakeEntryAuthoredBy,
    WakeEntryDraft, WakeEntryExecutionMode, WakeEntryGoalScope, WakeEntryRow, WakeEntryTriggerKind,
    WakeExecutionMode, WakeInvocationFinalize, WakeInvocationLogDraft, WakeInvocationLogRow,
    WakeInvocationRow, WakeInvocationStart, WakeInvocationStatus,
};
use proxima_core::{MemoryId, ModelTier, Owner, Principal, SchemaId, SchemaVersion, StorageError};
use sqlx::{PgPool, Row};

use crate::error::map_err;
use crate::outbox::hydrate_change_event;
use crate::pg_ident::PgIdent;
use crate::verbs::edge_append::{EdgeDraft, append_edge_in_tx};

fn owner_columns(owner: &Owner) -> (&'static str, uuid::Uuid, uuid::Uuid) {
    let (kind, principal_id) = match &owner.principal {
        Principal::User(u) => ("User", u.into_inner()),
        Principal::Group(g) => ("Group", g.into_inner()),
    };
    (kind, principal_id, owner.org_id.into_inner())
}

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

pub async fn list_change_events_after(
    pool: &PgPool,
    owner: &Owner,
    after: uuid::Uuid,
    limit: usize,
) -> Result<Vec<ChangeEventForWake>, StorageError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_columns(owner);
    let rows: Vec<(uuid::Uuid, Option<uuid::Uuid>, i16)> = sqlx::query_as(
        "SELECT seq, entity_personality_instance_id, wake_chain_depth
         FROM proxima_core.change_event
         WHERE owner_principal_kind = $1
           AND owner_principal_id = $2
           AND owner_org_id = $3
           AND seq > $4
         ORDER BY seq ASC
         LIMIT $5",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(after)
    .bind(i64::try_from(limit).unwrap_or(i64::MAX))
    .fetch_all(pool)
    .await
    .map_err(map_err)?;

    let mut out = Vec::with_capacity(rows.len());
    for (seq, instance_id, depth) in rows {
        if let Some(event) = hydrate_change_event(pool, seq).await? {
            out.push(ChangeEventForWake {
                event,
                authoring_personality_instance_id: instance_id
                    .filter(|id| !id.is_nil())
                    .map(PersonalityInstanceId::new),
                wake_chain_depth: WakeChainDepth::new(u16::try_from(depth).unwrap_or(0)),
            });
        }
    }
    Ok(out)
}

pub async fn list_change_events_for_replay(
    pool: &PgPool,
    owner: &Owner,
    after: uuid::Uuid,
    until: Option<uuid::Uuid>,
    limit: usize,
) -> Result<Vec<ChangeEventForWake>, StorageError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_columns(owner);
    let rows: Vec<(uuid::Uuid, Option<uuid::Uuid>, i16)> = sqlx::query_as(
        "SELECT seq, entity_personality_instance_id, wake_chain_depth
         FROM proxima_core.change_event
         WHERE owner_principal_kind = $1
           AND owner_principal_id = $2
           AND owner_org_id = $3
           AND seq > $4
           AND ($5::uuid IS NULL OR seq <= $5)
         ORDER BY seq ASC
         LIMIT $6",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(after)
    .bind(until)
    .bind(i64::try_from(limit).unwrap_or(i64::MAX))
    .fetch_all(pool)
    .await
    .map_err(map_err)?;

    let mut out = Vec::with_capacity(rows.len());
    for (seq, instance_id, depth) in rows {
        if let Some(event) = hydrate_change_event(pool, seq).await? {
            out.push(ChangeEventForWake {
                event,
                authoring_personality_instance_id: instance_id
                    .filter(|id| !id.is_nil())
                    .map(PersonalityInstanceId::new),
                wake_chain_depth: WakeChainDepth::new(u16::try_from(depth).unwrap_or(0)),
            });
        }
    }
    Ok(out)
}

pub async fn advance_wake_cursor(
    pool: &PgPool,
    owner: &Owner,
    instance: PersonalityInstanceId,
    last_considered_seq: uuid::Uuid,
) -> Result<(), StorageError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_columns(owner);
    sqlx::query(
        "UPDATE proxima_core.personality_wake_cursor
         SET last_considered_seq = GREATEST(last_considered_seq, $1), updated_at = now()
         WHERE owner_principal_kind = $2
           AND owner_principal_id = $3
           AND owner_org_id = $4
           AND personality_instance_id = $5",
    )
    .bind(last_considered_seq)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(instance.into_inner())
    .execute(pool)
    .await
    .map_err(map_err)?;
    Ok(())
}

pub async fn try_begin_wake_invocation(
    pool: &PgPool,
    owner: &Owner,
    instance: PersonalityInstanceId,
    wake_entry_id: uuid::Uuid,
    change_event_seq: uuid::Uuid,
) -> Result<bool, StorageError> {
    start_wake_invocation(
        pool,
        &WakeInvocationStart {
            owner: owner.clone(),
            personality_instance_id: instance,
            wake_entry_id,
            change_event_seq,
            wake_token: uuid::Uuid::nil(),
            recipe_sha256: String::new(),
            resolved_inference_target_ref: String::new(),
        },
    )
    .await
}

pub async fn start_wake_invocation(
    pool: &PgPool,
    start: &WakeInvocationStart,
) -> Result<bool, StorageError> {
    let owner = &start.owner;
    let (owner_kind, owner_principal_id, owner_org_id) = owner_columns(owner);
    let inserted: Option<(uuid::Uuid,)> = sqlx::query_as(
        "INSERT INTO proxima_core.personality_wake_invocations
            (owner_principal_kind, owner_principal_id, owner_org_id,
             personality_instance_id, wake_entry_id, change_event_seq,
             status, started_at, wake_token, recipe_sha256,
             resolved_inference_target_ref)
         VALUES ($1, $2, $3, $4, $5, $6, 'running', now(), $7, $8, $9)
         ON CONFLICT (owner_principal_kind, owner_principal_id, owner_org_id,
                      personality_instance_id, wake_entry_id, change_event_seq)
         DO NOTHING
         RETURNING change_event_seq",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(start.personality_instance_id.into_inner())
    .bind(start.wake_entry_id)
    .bind(start.change_event_seq)
    .bind(start.wake_token)
    .bind(&start.recipe_sha256)
    .bind(&start.resolved_inference_target_ref)
    .fetch_optional(pool)
    .await
    .map_err(map_err)?;
    Ok(inserted.is_some())
}

#[allow(clippy::too_many_arguments)]
pub async fn finish_wake_invocation(
    pool: &PgPool,
    owner: &Owner,
    instance: PersonalityInstanceId,
    wake_entry_id: uuid::Uuid,
    change_event_seq: uuid::Uuid,
    status: WakeInvocationStatus,
    turn_count: u16,
    cost_usd: f64,
) -> Result<(), StorageError> {
    finalize_wake_invocation(
        pool,
        &WakeInvocationFinalize {
            owner: owner.clone(),
            personality_instance_id: instance,
            wake_entry_id,
            change_event_seq,
            status,
            turn_count: Some(turn_count),
            cost_usd: Some(cost_usd),
            failure_reason: None,
            exit_code: None,
            duration_ms: None,
            stdout_tail: None,
            stderr_tail: None,
            stdout_truncated: false,
            stderr_truncated: false,
        },
    )
    .await
}

pub async fn finalize_wake_invocation(
    pool: &PgPool,
    finalize: &WakeInvocationFinalize,
) -> Result<(), StorageError> {
    let owner = &finalize.owner;
    let (owner_kind, owner_principal_id, owner_org_id) = owner_columns(owner);
    sqlx::query(
        "UPDATE proxima_core.personality_wake_invocations
         SET status = $1,
             finished_at = now(),
             turn_count = COALESCE($2, turn_count),
             cost_usd = COALESCE($3, cost_usd),
             failure_reason = $4,
             exit_code = $5,
             duration_ms = $6,
             stdout_tail = $7,
             stderr_tail = $8,
             stdout_truncated = $9,
             stderr_truncated = $10
         WHERE owner_principal_kind = $11
           AND owner_principal_id = $12
           AND owner_org_id = $13
           AND personality_instance_id = $14
           AND wake_entry_id = $15
           AND change_event_seq = $16",
    )
    .bind(finalize.status.as_str())
    .bind(finalize.turn_count.map(i32::from))
    .bind(finalize.cost_usd)
    .bind(&finalize.failure_reason)
    .bind(finalize.exit_code)
    .bind(finalize.duration_ms.and_then(|v| i64::try_from(v).ok()))
    .bind(&finalize.stdout_tail)
    .bind(&finalize.stderr_tail)
    .bind(finalize.stdout_truncated)
    .bind(finalize.stderr_truncated)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(finalize.personality_instance_id.into_inner())
    .bind(finalize.wake_entry_id)
    .bind(finalize.change_event_seq)
    .execute(pool)
    .await
    .map_err(map_err)?;
    Ok(())
}

pub async fn append_wake_invocation_log(
    pool: &PgPool,
    log: &WakeInvocationLogDraft,
) -> Result<(), StorageError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_columns(&log.owner);
    sqlx::query(
        "INSERT INTO proxima_core.personality_wake_invocation_logs
            (owner_principal_kind, owner_principal_id, owner_org_id,
             personality_instance_id, wake_entry_id, change_event_seq,
             phase, tool_id, status, duration_ms, message_tail)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(log.personality_instance_id.into_inner())
    .bind(log.wake_entry_id)
    .bind(log.change_event_seq)
    .bind(&log.phase)
    .bind(&log.tool_id)
    .bind(&log.status)
    .bind(log.duration_ms.and_then(|v| i64::try_from(v).ok()))
    .bind(&log.message_tail)
    .execute(pool)
    .await
    .map_err(map_err)?;
    Ok(())
}

#[derive(sqlx::FromRow)]
struct WakeInvocationRowDb {
    owner_principal_kind: String,
    owner_principal_id: uuid::Uuid,
    owner_org_id: uuid::Uuid,
    personality_instance_id: uuid::Uuid,
    wake_entry_id: uuid::Uuid,
    wake_entry_label: String,
    change_event_seq: uuid::Uuid,
    status: String,
    started_at: time::OffsetDateTime,
    finished_at: Option<time::OffsetDateTime>,
    turn_count: i32,
    cost_usd: f64,
    recipe_sha256: Option<String>,
    resolved_inference_target_ref: Option<String>,
    failure_reason: Option<String>,
    exit_code: Option<i32>,
    duration_ms: Option<i64>,
    stdout_tail: Option<String>,
    stderr_tail: Option<String>,
    stdout_truncated: bool,
    stderr_truncated: bool,
}

#[derive(sqlx::FromRow)]
struct WakeInvocationLogRowDb {
    log_seq: i32,
    at: time::OffsetDateTime,
    phase: String,
    tool_id: Option<String>,
    status: String,
    duration_ms: Option<i64>,
    message_tail: Option<String>,
}

pub async fn list_wake_invocations(
    pool: &PgPool,
    req: &ListWakeInvocationsRequest,
) -> Result<Vec<WakeInvocationRow>, StorageError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_columns(&req.owner);
    let limit = i64::from(req.limit.clamp(1, 100));
    let rows: Vec<WakeInvocationRowDb> = sqlx::query_as(
        "SELECT i.owner_principal_kind, i.owner_principal_id, i.owner_org_id,
                i.personality_instance_id, i.wake_entry_id, e.label AS wake_entry_label,
                i.change_event_seq, i.status, i.started_at, i.finished_at,
                i.turn_count, i.cost_usd::float8 AS cost_usd, i.recipe_sha256,
                i.resolved_inference_target_ref, i.failure_reason,
                i.exit_code, i.duration_ms, i.stdout_tail, i.stderr_tail,
                i.stdout_truncated, i.stderr_truncated
         FROM proxima_core.personality_wake_invocations i
         JOIN proxima_core.personality_wake_entries e
           ON e.owner_principal_kind = i.owner_principal_kind
          AND e.owner_principal_id = i.owner_principal_id
          AND e.owner_org_id = i.owner_org_id
          AND e.personality_instance_id = i.personality_instance_id
          AND e.wake_entry_id = i.wake_entry_id
         WHERE i.owner_principal_kind = $1
           AND i.owner_principal_id = $2
           AND i.owner_org_id = $3
           AND i.personality_instance_id = $4
           AND ($5::uuid IS NULL OR i.wake_entry_id = $5)
         ORDER BY i.started_at DESC
         LIMIT $6",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(req.personality_instance_id.into_inner())
    .bind(req.wake_entry_id)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(map_err)?;

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let logs: Vec<WakeInvocationLogRowDb> = sqlx::query_as(
            "SELECT log_seq, at, phase, tool_id, status, duration_ms, message_tail
             FROM proxima_core.personality_wake_invocation_logs
             WHERE owner_principal_kind = $1
               AND owner_principal_id = $2
               AND owner_org_id = $3
               AND personality_instance_id = $4
               AND wake_entry_id = $5
               AND change_event_seq = $6
             ORDER BY log_seq ASC",
        )
        .bind(owner_kind)
        .bind(owner_principal_id)
        .bind(owner_org_id)
        .bind(row.personality_instance_id)
        .bind(row.wake_entry_id)
        .bind(row.change_event_seq)
        .fetch_all(pool)
        .await
        .map_err(map_err)?;
        out.push(WakeInvocationRow {
            owner: owner_from_parts(
                &row.owner_principal_kind,
                row.owner_principal_id,
                row.owner_org_id,
            ),
            personality_instance_id: PersonalityInstanceId::new(row.personality_instance_id),
            wake_entry_id: row.wake_entry_id,
            wake_entry_label: row.wake_entry_label,
            change_event_seq: row.change_event_seq,
            status: parse_wake_invocation_status(&row.status),
            started_at: row.started_at,
            finished_at: row.finished_at,
            turn_count: u16::try_from(row.turn_count).unwrap_or(0),
            cost_usd: row.cost_usd,
            recipe_sha256: row.recipe_sha256,
            resolved_inference_target_ref: row.resolved_inference_target_ref,
            failure_reason: row.failure_reason,
            exit_code: row.exit_code,
            duration_ms: row.duration_ms.and_then(|v| u64::try_from(v).ok()),
            stdout_tail: row.stdout_tail,
            stderr_tail: row.stderr_tail,
            stdout_truncated: row.stdout_truncated,
            stderr_truncated: row.stderr_truncated,
            logs: logs
                .into_iter()
                .map(|log| WakeInvocationLogRow {
                    log_seq: i64::from(log.log_seq),
                    at: log.at,
                    phase: log.phase,
                    tool_id: log.tool_id,
                    status: log.status,
                    duration_ms: log.duration_ms.and_then(|v| u64::try_from(v).ok()),
                    message_tail: log.message_tail,
                })
                .collect(),
        });
    }
    Ok(out)
}

pub async fn load_memory_batch_facts(
    pool: &PgPool,
    owner: &Owner,
    memory_id: MemoryId,
    sidecars: &[SidecarSpec],
) -> Result<Vec<FactRow>, StorageError> {
    let batch_id: Option<uuid::Uuid> = sqlx::query_scalar(
        "SELECT e.source_batch_id
         FROM proxima_core.memories m
         JOIN proxima_core.events e ON e.event_id = m.event_id
         WHERE m.memory_id = $1",
    )
    .bind(memory_id.into_inner())
    .fetch_optional(pool)
    .await
    .map_err(map_err)?;
    let Some(batch_id) = batch_id else {
        return Ok(Vec::new());
    };
    load_batch_facts_by_id(pool, owner, batch_id, sidecars).await
}

async fn load_batch_facts_by_id(
    pool: &PgPool,
    owner: &Owner,
    batch_id: uuid::Uuid,
    sidecars: &[SidecarSpec],
) -> Result<Vec<FactRow>, StorageError> {
    let (owner_kind, owner_principal_id, _owner_org_id) = owner_columns(owner);
    let mut out = Vec::new();
    for spec in sidecars {
        let sidecar = PgIdent::table(&spec.sidecar_table)?;
        let sql = format!(
            "SELECT m.memory_id, e.schema_version, row_to_json(s.*) AS payload, m.wake_chain_depth
             FROM proxima_core.memories m
             JOIN proxima_core.events e ON m.event_id = e.event_id
             JOIN {sidecar} s ON s.memory_id = m.memory_id
             WHERE e.source_batch_id = $1
               AND m.owner_principal_kind = $2
               AND m.owner_principal_id = $3
               AND m.schema_id = $4",
            sidecar = sidecar.as_str(),
        );
        let rows: Vec<(uuid::Uuid, i32, serde_json::Value, i16)> = sqlx::query_as(&sql)
            .bind(batch_id)
            .bind(owner_kind)
            .bind(owner_principal_id)
            .bind(spec.schema_id.as_str())
            .fetch_all(pool)
            .await
            .map_err(map_err)?;
        out.extend(
            rows.into_iter()
                .map(|(memory_id, schema_version, payload_json, depth)| FactRow {
                    memory_id: MemoryId::new(memory_id),
                    schema_id: spec.schema_id.clone(),
                    schema_version: SchemaVersion::new(u32::try_from(schema_version).unwrap_or(1)),
                    payload_json,
                    wake_chain_depth: WakeChainDepth::new(u16::try_from(depth).unwrap_or(0)),
                }),
        );
    }
    Ok(out)
}

pub async fn load_abstraction_heads(
    pool: &PgPool,
    owner: &Owner,
    sidecars: &[SidecarSpec],
    limit: usize,
) -> Result<Vec<AbstractionRow>, StorageError> {
    let (owner_kind, owner_principal_id, _owner_org_id) = owner_columns(owner);
    let mut rows_all = Vec::new();
    for spec in sidecars {
        let sidecar = PgIdent::table(&spec.sidecar_table)?;
        let sql = format!(
            "SELECT m.memory_id, m.schema_version, m.text, row_to_json(s.*) AS payload,
                    m.created_at, m.wake_chain_depth
             FROM proxima_core.memories m
             JOIN {sidecar} s ON s.memory_id = m.memory_id
             WHERE m.owner_principal_kind = $1
               AND m.owner_principal_id = $2
               AND m.kind = 'Abstraction'
               AND m.schema_id = $3
               AND NOT EXISTS (
                    SELECT 1 FROM proxima_core.memories newer
                    WHERE newer.supersedes = m.memory_id
               )
             ORDER BY m.created_at DESC, m.memory_id DESC
             LIMIT $4",
            sidecar = sidecar.as_str(),
        );
        let rows: Vec<(
            uuid::Uuid,
            i32,
            String,
            serde_json::Value,
            time::OffsetDateTime,
            i16,
        )> = sqlx::query_as(&sql)
            .bind(owner_kind)
            .bind(owner_principal_id)
            .bind(spec.schema_id.as_str())
            .bind(i64::try_from(limit).unwrap_or(i64::MAX))
            .fetch_all(pool)
            .await
            .map_err(map_err)?;
        for (memory_id, schema_version, text, payload_json, created_at, depth) in rows {
            rows_all.push((
                created_at,
                memory_id,
                AbstractionRow {
                    memory_id: MemoryId::new(memory_id),
                    schema_id: spec.schema_id.clone(),
                    schema_version: SchemaVersion::new(u32::try_from(schema_version).unwrap_or(1)),
                    text,
                    payload_json,
                    wake_chain_depth: WakeChainDepth::new(u16::try_from(depth).unwrap_or(0)),
                },
            ));
        }
    }
    rows_all.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    Ok(rows_all
        .into_iter()
        .take(limit)
        .map(|(_, _, row)| row)
        .collect())
}

pub async fn load_memory_by_id(
    pool: &PgPool,
    owner: &Owner,
    memory_id: MemoryId,
    sidecars: &[SidecarSpec],
) -> Result<Option<MemorySnapshot>, StorageError> {
    let (owner_kind, owner_principal_id, _owner_org_id) = owner_columns(owner);
    let head: Option<(Option<String>, String, i32, Option<String>, i16)> = sqlx::query_as(
        "SELECT kind, schema_id, schema_version, text, wake_chain_depth
         FROM proxima_core.memories
         WHERE memory_id = $1
           AND owner_principal_kind = $2
           AND owner_principal_id = $3",
    )
    .bind(memory_id.into_inner())
    .bind(owner_kind)
    .bind(owner_principal_id)
    .fetch_optional(pool)
    .await
    .map_err(map_err)?;
    let Some((kind, schema_id, schema_version, text, depth)) = head else {
        return Ok(None);
    };
    let kind_str = kind.unwrap_or_else(|| "Fact".to_string());
    let payload_json =
        if let Some(spec) = sidecars.iter().find(|s| s.schema_id.as_str() == schema_id) {
            let sidecar = PgIdent::table(&spec.sidecar_table)?;
            let sql = format!(
                "SELECT row_to_json(s.*) AS payload FROM {sidecar} s WHERE s.memory_id = $1",
                sidecar = sidecar.as_str(),
            );
            let row: Option<(serde_json::Value,)> = sqlx::query_as(&sql)
                .bind(memory_id.into_inner())
                .fetch_optional(pool)
                .await
                .map_err(map_err)?;
            row.map_or(serde_json::Value::Null, |(p,)| p)
        } else {
            serde_json::Value::Null
        };
    Ok(Some(MemorySnapshot {
        memory_id,
        kind: kind_str,
        schema_id: SchemaId::new(schema_id),
        schema_version: SchemaVersion::new(u32::try_from(schema_version).unwrap_or(1)),
        text,
        wake_chain_depth: WakeChainDepth::new(u16::try_from(depth).unwrap_or(0)),
        payload_json,
    }))
}

pub async fn lookup_prior_personality_head(
    pool: &PgPool,
    owner: &Owner,
    instance: &PersonalityRef,
    schema_id: &SchemaId,
) -> Result<Option<MemoryId>, StorageError> {
    let (owner_kind, owner_principal_id, _owner_org_id) = owner_columns(owner);
    let row: Option<(uuid::Uuid,)> = sqlx::query_as(
        "SELECT memory_id
         FROM proxima_core.memories
         WHERE owner_principal_kind = $1
           AND owner_principal_id = $2
           AND schema_id = $3
           AND personality_instance_id = $4
           AND kind = 'Perspective'
           AND NOT EXISTS (
                SELECT 1 FROM proxima_core.memories newer
                WHERE newer.supersedes = memories.memory_id
           )
         ORDER BY created_at DESC
         LIMIT 1",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(schema_id.as_str())
    .bind(instance.personality_instance_id.into_inner())
    .fetch_optional(pool)
    .await
    .map_err(map_err)?;
    Ok(row.map(|(id,)| MemoryId::new(id)))
}

#[allow(clippy::too_many_lines)]
pub async fn append_personality_memories(
    pool: &PgPool,
    req: &PersonalityWriteRequest<'_>,
) -> Result<PersonalityWriteOutcome, StorageError> {
    if req.memories.is_empty() {
        return Ok(PersonalityWriteOutcome {
            memory_ids: Vec::new(),
        });
    }
    let output_sidecar_table = PgIdent::table(req.sidecar_table)?;
    let (owner_kind, owner_principal_id, owner_org_id) = owner_columns(&req.owner);
    let mut tx = pool.begin().await.map_err(map_err)?;
    let mut memory_ids = Vec::with_capacity(req.memories.len());

    for memory in req.memories {
        let memory_id = uuid::Uuid::now_v7();
        let prior_head = if memory.kind == proxima_core::PersonalityMemoryKind::Perspective {
            lookup_prior_personality_head(pool, &req.owner, &req.instance, &memory.schema_id)
                .await?
                .map(MemoryId::into_inner)
        } else {
            None
        };
        memory_ids.push(MemoryId::new(memory_id));
        sqlx::query(
            "INSERT INTO proxima_core.memories
                (memory_id, owner_principal_kind, owner_principal_id, owner_org_id,
                 schema_id, schema_version, kind, text, operator_kind, model_id,
                 prompt_version, personality_instance_id,
                 wake_chain_depth, supersedes)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'Wake', $9, $10, $11, $12, $13)",
        )
        .bind(memory_id)
        .bind(owner_kind)
        .bind(owner_principal_id)
        .bind(owner_org_id)
        .bind(memory.schema_id.as_str())
        .bind(i32::try_from(memory.schema_version.into_inner()).unwrap_or(1))
        .bind(memory.kind.as_str())
        .bind(&memory.text)
        .bind(req.model_id)
        .bind(req.prompt_version)
        .bind(req.instance.personality_instance_id.into_inner())
        .bind(i16::try_from(req.wake_chain_depth.into_inner()).unwrap_or(i16::MAX))
        .bind(prior_head)
        .execute(&mut *tx)
        .await
        .map_err(map_err)?;

        let sidecar_sql = format!(
            "INSERT INTO {sidecar}
             SELECT * FROM jsonb_populate_record(
                 NULL::{sidecar},
                 ($1::jsonb || jsonb_build_object('memory_id', $2::uuid))
             )",
            sidecar = output_sidecar_table.as_str(),
        );
        sqlx::query(&sidecar_sql)
            .bind(&memory.typed_payload)
            .bind(memory_id)
            .execute(&mut *tx)
            .await
            .map_err(map_err)?;

        let change_seq = uuid::Uuid::now_v7();
        sqlx::query(
            "INSERT INTO proxima_core.change_event
                (seq, owner_principal_kind, owner_principal_id, owner_org_id, kind,
                 entity_kind, entity_memory_id, entity_schema_id, entity_schema_version,
                 entity_personality_instance_id,
                 wake_chain_depth, supersedes_memory_id)
             VALUES ($1, $2, $3, $4, 'EntityAppend', $5, $6, $7, $8, $9, $10, $11)",
        )
        .bind(change_seq)
        .bind(owner_kind)
        .bind(owner_principal_id)
        .bind(owner_org_id)
        .bind(memory.kind.as_str())
        .bind(memory_id)
        .bind(memory.schema_id.as_str())
        .bind(i32::try_from(memory.schema_version.into_inner()).unwrap_or(1))
        .bind(req.instance.personality_instance_id.into_inner())
        .bind(i16::try_from(req.wake_chain_depth.into_inner()).unwrap_or(i16::MAX))
        .bind(prior_head)
        .execute(&mut *tx)
        .await
        .map_err(map_err)?;

        for prov_id in &memory.provenance {
            let target_kind = memory_kind_for_provenance(&mut tx, *prov_id).await?;
            let authorship_kind = provenance_edge_authorship_kind(memory.kind);
            let draft = EdgeDraft {
                edge_id: uuid::Uuid::now_v7(),
                relation: req.provenance_relation,
                source_kind: memory.kind.as_str(),
                source_memory_id: Some(memory_id),
                source_goal_id: None,
                target_kind,
                target_memory_id: Some(prov_id.into_inner()),
                target_goal_id: None,
                authorship_kind,
                authorship_owner_memory_id: Some(memory_id),
                owner: &req.owner,
            };
            append_edge_in_tx(&mut tx, &draft, None).await?;
        }

        if let Some(prior_head) = prior_head {
            let draft = EdgeDraft {
                edge_id: uuid::Uuid::now_v7(),
                relation: req.supersedes_relation,
                source_kind: memory.kind.as_str(),
                source_memory_id: Some(memory_id),
                source_goal_id: None,
                target_kind: memory.kind.as_str(),
                target_memory_id: Some(prior_head),
                target_goal_id: None,
                authorship_kind: "Engine",
                authorship_owner_memory_id: None,
                owner: &req.owner,
            };
            append_edge_in_tx(&mut tx, &draft, None).await?;
        }

        let authored = EdgeDraft {
            edge_id: uuid::Uuid::now_v7(),
            relation: req.authored_relation,
            source_kind: "Perspective",
            source_memory_id: Some(req.current_root_perspective_memory_id.into_inner()),
            source_goal_id: None,
            target_kind: memory.kind.as_str(),
            target_memory_id: Some(memory_id),
            target_goal_id: None,
            authorship_kind: "Engine",
            authorship_owner_memory_id: None,
            owner: &req.owner,
        };
        append_edge_in_tx(&mut tx, &authored, None).await?;

        let dim = i32::try_from(memory.embedding.len())
            .map_err(|_| StorageError::ConstraintViolation("embedding dim too large".into()))?;
        sqlx::query(
            "INSERT INTO proxima_core.embeddings
                (entity_kind, entity_id, embedding_version, model_id, vec, dim,
                 owner_principal_kind, owner_principal_id, owner_org_id)
             VALUES ($1, $2, 1, $3, $4, $5, $6, $7, $8)",
        )
        .bind(memory.kind.as_str())
        .bind(memory_id)
        .bind(&memory.embedding_model_id)
        .bind(&memory.embedding)
        .bind(dim)
        .bind(owner_kind)
        .bind(owner_principal_id)
        .bind(owner_org_id)
        .execute(&mut *tx)
        .await
        .map_err(map_err)?;
    }

    tx.commit().await.map_err(map_err)?;
    Ok(PersonalityWriteOutcome { memory_ids })
}

fn provenance_edge_authorship_kind(kind: proxima_core::PersonalityMemoryKind) -> &'static str {
    match kind {
        proxima_core::PersonalityMemoryKind::Abstraction => "OperatorFtoA",
        proxima_core::PersonalityMemoryKind::Perspective => "OperatorAtoP",
    }
}

async fn memory_kind_for_provenance(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    memory_id: MemoryId,
) -> Result<&'static str, StorageError> {
    let row: Option<(Option<String>,)> =
        sqlx::query_as("SELECT kind FROM proxima_core.memories WHERE memory_id = $1")
            .bind(memory_id.into_inner())
            .fetch_optional(&mut **tx)
            .await
            .map_err(map_err)?;
    match row.and_then(|(kind,)| kind) {
        Some(kind) if kind == "Abstraction" => Ok("Abstraction"),
        Some(kind) if kind == "Perspective" => Ok("Perspective"),
        Some(other) => Err(StorageError::Internal(format!(
            "unsupported provenance memory kind: {other}"
        ))),
        None => Ok("Fact"),
    }
}

fn owner_from_parts(kind: &str, principal_id: uuid::Uuid, org_id: uuid::Uuid) -> Owner {
    Owner {
        principal: match kind {
            "User" => Principal::User(proxima_core::UserId::new(principal_id)),
            _ => Principal::Group(proxima_core::GroupId::new(principal_id)),
        },
        org_id: proxima_core::OrgId::new(org_id),
    }
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

fn parse_trigger_kind(value: &str) -> WakeEntryTriggerKind {
    match value {
        "on_edge" => WakeEntryTriggerKind::OnEdge,
        _ => WakeEntryTriggerKind::OnMemory,
    }
}

fn parse_execution_mode(value: &str) -> WakeExecutionMode {
    match value {
        "workspace" => WakeExecutionMode::Workspace,
        _ => WakeExecutionMode::SubstrateOnly,
    }
}

fn parse_row_execution_mode(value: &str) -> WakeEntryExecutionMode {
    match value {
        "workspace" => WakeEntryExecutionMode::Workspace,
        _ => WakeEntryExecutionMode::SubstrateOnly,
    }
}

fn parse_goal_scope(value: &str) -> WakeEntryGoalScope {
    match value {
        "trigger_goal_assigned" => WakeEntryGoalScope::TriggerGoalAssigned,
        _ => WakeEntryGoalScope::None,
    }
}

fn parse_row_authored_by(value: &str) -> WakeEntryAuthoredBy {
    match value {
        "self" => WakeEntryAuthoredBy::SelfAuthor,
        "other" => WakeEntryAuthoredBy::Other,
        _ => WakeEntryAuthoredBy::Any,
    }
}

fn parse_model_tier(value: &str) -> ModelTier {
    match value {
        "fast" => ModelTier::Fast,
        "deep" => ModelTier::Deep,
        _ => ModelTier::Standard,
    }
}

fn parse_wake_invocation_status(value: &str) -> WakeInvocationStatus {
    match value {
        "running" => WakeInvocationStatus::Running,
        "truncated" => WakeInvocationStatus::Truncated,
        "failed" => WakeInvocationStatus::Failed,
        _ => WakeInvocationStatus::Succeeded,
    }
}

fn model_tier_str(tier: ModelTier) -> &'static str {
    match tier {
        ModelTier::Fast => "fast",
        ModelTier::Standard => "standard",
        ModelTier::Deep => "deep",
    }
}
