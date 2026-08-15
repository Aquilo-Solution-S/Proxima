//! WakeConfig + fire (one write-act per match). UML §3b / §5b.

use proxima_core::{Owner, OwnerRefKind, StorageError};
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::error::map_err;
use crate::verbs::goal_timeseries::{ingest_write_act, GoalWriteCommand, write_goal};
use proxima_core::verbs::goal_write::GoalState;

#[derive(Debug, Clone, Copy, sqlx::Type)]
#[sqlx(type_name = "proxima_core.wake_trigger_kind", rename_all = "snake_case")]
pub enum WakeTriggerKind {
    FactSchema,
    FactMemory,
}

#[derive(Debug, Clone)]
pub struct WakeConfigDraft {
    pub trigger_kind: WakeTriggerKind,
    pub trigger_schema_id: Option<String>,
    pub trigger_t: Option<Uuid>,
    pub tool_ids: Vec<String>,
    pub prompt: String,
    pub hard_memory_t: Vec<Uuid>,
}

pub async fn insert_wake_config(
    tx: &mut Transaction<'_, Postgres>,
    owner: &Owner,
    draft: &WakeConfigDraft,
) -> Result<Uuid, StorageError> {
    crate::access::owner_columns::reject_world_write_owner(owner)?;
    let owner_id = owner.stored_owner_id();
    sqlx::query(
        "INSERT INTO proxima_core.owners (owner_id, kind)
         VALUES ($1, $2::proxima_core.owner_kind)
         ON CONFLICT (owner_id) DO NOTHING",
    )
    .bind(owner_id)
    .bind(OwnerRefKind::of(owner).as_str())
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;

    sqlx::query_scalar(
        "INSERT INTO proxima_core.wake_config
            (owner_id, trigger_kind, trigger_schema_id, trigger_t, tool_ids, prompt, hard_memory_t)
         VALUES ($1, $2, $3, $4, $5, $6, $7)
         RETURNING wake_id",
    )
    .bind(owner_id)
    .bind(draft.trigger_kind)
    .bind(draft.trigger_schema_id.as_deref())
    .bind(draft.trigger_t)
    .bind(&draft.tool_ids)
    .bind(&draft.prompt)
    .bind(&draft.hard_memory_t)
    .fetch_one(tx.as_mut())
    .await
    .map_err(map_err)
}

pub async fn update_wake_prompt(
    pool: &PgPool,
    wake_id: Uuid,
    prompt: &str,
) -> Result<(), StorageError> {
    let n = sqlx::query("UPDATE proxima_core.wake_config SET prompt = $2 WHERE wake_id = $1")
        .bind(wake_id)
        .bind(prompt)
        .execute(pool)
        .await
        .map_err(map_err)?
        .rows_affected();
    if n == 0 {
        return Err(StorageError::NotFound);
    }
    Ok(())
}

/// Armed heads whose trigger matches this incoming Fact `t` (schema or pin).
pub async fn matching_wake_ids(pool: &PgPool, incoming_t: Uuid) -> Result<Vec<Uuid>, StorageError> {
    sqlx::query_scalar(
        "SELECT DISTINCT g.wake_id
           FROM proxima_core.goal_head h
           JOIN proxima_core.goal g ON g.handle = h.handle AND g.t = h.t
           JOIN proxima_core.wake_config w ON w.wake_id = g.wake_id
           JOIN proxima_core.memory_head mh
             ON mh.t = $1
          WHERE g.wake_id IS NOT NULL
            AND g.state = 'Active'
            AND (
                (w.trigger_kind = 'fact_schema' AND w.trigger_schema_id = mh.schema_id)
                OR (w.trigger_kind = 'fact_memory' AND w.trigger_t = $1)
            )",
    )
    .bind(incoming_t)
    .fetch_all(pool)
    .await
    .map_err(map_err)
}

/// One write-act Fact for the fire (not per Memory).
pub async fn fire_wake(
    tx: &mut Transaction<'_, Postgres>,
    owner: &Owner,
) -> Result<Uuid, StorageError> {
    let outcome = ingest_write_act(tx, owner).await?;
    Ok(outcome.memory_id.into_inner())
}

/// Helper so a Goal can be created already armed (used by tests).
pub async fn write_armed_goal(
    tx: &mut Transaction<'_, Postgres>,
    owner: &Owner,
    title: &str,
    request_id: &str,
    wake_id: Uuid,
) -> Result<Uuid, StorageError> {
    let out = write_goal(
        tx,
        owner,
        &GoalWriteCommand {
            handle: None,
            schema_id: "core/task-v1".into(),
            title: title.into(),
            state: GoalState::Active,
            request_id: request_id.into(),
            close_fact_t: None,
            assignment_t: None,
            dependency_t: vec![],
            evidence_t: vec![],
            wake_id: Some(wake_id),
            mint_write_act: false,
        },
    )
    .await?;
    Ok(out.handle)
}
