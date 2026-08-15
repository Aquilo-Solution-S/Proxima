//! Goal timeseries write (v0.0.8). UML §3 / §5b / §8.
#![allow(
    clippy::missing_errors_doc,
    clippy::doc_markdown,
    clippy::too_many_lines
)]

use proxima_core::verbs::fact_ingest::{FactIngestOutcome, FactWriteCommand};
use proxima_core::verbs::goal_write::GoalState;
use proxima_core::{MemoryId, Owner, OwnerRefKind, StorageError};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::error::map_err;
use crate::verbs::memory_timeseries::ingest_fact_timeseries;

pub const WRITE_ACT_SCHEMA: &str = "core/write-act-v1";

#[derive(Debug, Clone)]
pub struct GoalWriteCommand {
    pub handle: Option<Uuid>,
    pub schema_id: String,
    pub title: String,
    pub state: GoalState,
    pub request_id: String,
    pub close_fact_t: Option<Uuid>,
    pub assignment_t: Option<Uuid>,
    pub dependency_t: Vec<Uuid>,
    pub evidence_t: Vec<Uuid>,
    pub wake_id: Option<Uuid>,
    pub mint_write_act: bool,
}

#[derive(Debug, Clone)]
pub struct GoalWriteOutcome {
    pub handle: Uuid,
    pub t: Uuid,
    pub write_act_t: Option<Uuid>,
    pub replay: bool,
}

pub async fn ingest_write_act(
    tx: &mut Transaction<'_, Postgres>,
    owner: &Owner,
) -> Result<FactIngestOutcome, StorageError> {
    let draft = FactWriteCommand {
        schema_id: proxima_core::SchemaId::new(WRITE_ACT_SCHEMA.to_string()),
        schema_version: proxima_core::SchemaVersion::new(1),
        handle: None,
        source_id: None,
        ingest_key: None,
        payload: Vec::new(),
        rendered_text: None,
        lexical_language: None,
        receipt: None,
        citation: None,
        derived_from: Vec::new(),
        refs: Vec::new(),
        blob_id: None,
        kind: "fact".into(),
    };
    ingest_fact_timeseries(tx, owner, &draft).await
}

pub async fn write_goal(
    tx: &mut Transaction<'_, Postgres>,
    owner: &Owner,
    draft: &GoalWriteCommand,
) -> Result<GoalWriteOutcome, StorageError> {
    crate::access::owner_columns::reject_world_write_owner(owner)?;
    let owner_id = owner.stored_owner_id();
    let owner_kind = OwnerRefKind::of(owner).as_str();
    sqlx::query(
        "INSERT INTO proxima_core.owners (owner_id, kind)
         VALUES ($1, $2::proxima_core.owner_kind)
         ON CONFLICT (owner_id) DO NOTHING",
    )
    .bind(owner_id)
    .bind(owner_kind)
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;

    if let Some((handle, t, write_act_t)) = sqlx::query_as::<_, (Uuid, Uuid, Option<Uuid>)>(
        "SELECT handle, t, write_act_t FROM proxima_core.goal
          WHERE owner_id = $1 AND request_id = $2",
    )
    .bind(owner_id)
    .bind(&draft.request_id)
    .fetch_optional(tx.as_mut())
    .await
    .map_err(map_err)?
    {
        return Ok(GoalWriteOutcome {
            handle,
            t,
            write_act_t,
            replay: true,
        });
    }

    let write_act = if draft.mint_write_act {
        Some(ingest_write_act(tx, owner).await?)
    } else {
        None
    };
    let write_act_t = write_act.as_ref().map(|o| o.memory_id.into_inner());

    let handle = draft.handle.unwrap_or_else(Uuid::now_v7);
    let t: Uuid = sqlx::query_scalar("SELECT uuidv7()")
        .fetch_one(tx.as_mut())
        .await
        .map_err(map_err)?;

    let head = sqlx::query(
        "INSERT INTO proxima_core.goal_head (handle, schema_id, owner_id, t)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT (handle) DO UPDATE SET t = EXCLUDED.t
         WHERE proxima_core.goal_head.schema_id = EXCLUDED.schema_id
           AND proxima_core.goal_head.owner_id = EXCLUDED.owner_id
         RETURNING handle",
    )
    .bind(handle)
    .bind(&draft.schema_id)
    .bind(owner_id)
    .bind(t)
    .fetch_optional(tx.as_mut())
    .await
    .map_err(map_err)?;
    if head.is_none() {
        return Err(StorageError::ConstraintViolation(
            "goal_head schema/owner mismatch".into(),
        ));
    }

    sqlx::query(
        "INSERT INTO proxima_core.goal
            (handle, t, owner_id, title, state, request_id, close_fact_t,
             assignment_t, dependency_t, evidence_t, wake_id, write_act_t)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
    )
    .bind(handle)
    .bind(t)
    .bind(owner_id)
    .bind(&draft.title)
    .bind(draft.state)
    .bind(&draft.request_id)
    .bind(draft.close_fact_t)
    .bind(draft.assignment_t)
    .bind(&draft.dependency_t)
    .bind(&draft.evidence_t)
    .bind(draft.wake_id)
    .bind(write_act_t)
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;

    sqlx::query(
        "INSERT INTO proxima_core.announce (owner_id, op, entity, handle, t)
         VALUES ($1, 'append', 'goal', $2, $3)",
    )
    .bind(owner_id)
    .bind(handle)
    .bind(t)
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;

    let _ = MemoryId::new(t);
    Ok(GoalWriteOutcome {
        handle,
        t,
        write_act_t,
        replay: false,
    })
}
