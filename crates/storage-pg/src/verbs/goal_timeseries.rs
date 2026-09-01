//! Goal timeseries write.
#![allow(
    clippy::missing_errors_doc,
    clippy::doc_markdown,
    clippy::too_many_lines
)]

use proxima_core::verbs::fact_ingest::{FactIngestOutcome, FactWriteCommand};
use proxima_core::verbs::goal_write::GoalState;
use proxima_core::{Owner, StorageError};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::error::map_err;
use crate::verbs::memory_timeseries::{ingest_fact_timeseries, ingest_unpinned_fact_at};
use crate::verbs::wake_timeseries::{WakeConfigDraft, insert_wake_config};

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
    /// Attach an already-minted write-act when `mint_write_act` is false.
    pub write_act_t: Option<Uuid>,
}

#[derive(Debug, Clone)]
pub struct GoalWriteOutcome {
    pub handle: Uuid,
    pub t: Uuid,
    pub write_act_t: Option<Uuid>,
    pub replay: bool,
}

/// An exact identity reserved for a lifecycle Fact. Reservation mints no
/// database row; persistence happens only after the complete Goal target union
/// has acquired its advisory locks.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ReservedFactIdentity {
    pub(crate) handle: Uuid,
    pub(crate) t: Uuid,
}

/// The wake rows a Goal write will either create or carry. A carried wake is
/// snapshotted before the lifecycle lock and re-read after it; this closes the
/// gap in which a target could be erased while a successor was still being
/// prepared.
#[derive(Debug, Clone)]
pub(crate) enum GoalWakePlan {
    None,
    Existing {
        wake_id: Uuid,
        trigger_t: Option<Uuid>,
        hard_memory_t: Vec<Uuid>,
    },
    New(WakeConfigDraft),
}

impl GoalWakePlan {
    fn target_ids(&self) -> impl Iterator<Item = Uuid> + '_ {
        self.trigger_t()
            .into_iter()
            .chain(self.hard_memory_t().iter().copied())
    }

    fn trigger_t(&self) -> Option<Uuid> {
        match self {
            Self::None => None,
            Self::Existing { trigger_t, .. } => *trigger_t,
            Self::New(draft) => draft.trigger_t,
        }
    }

    fn hard_memory_t(&self) -> &[Uuid] {
        match self {
            Self::None => &[],
            Self::Existing { hard_memory_t, .. } => hard_memory_t,
            Self::New(draft) => &draft.hard_memory_t,
        }
    }
}

/// A Goal write after replay detection and before its one lifecycle lock.
/// Decomposition prepares every child first, unions these target sets, then
/// persists them in order without extending an already-held advisory set.
#[derive(Debug)]
pub(crate) struct PreparedGoalWrite {
    owner: Owner,
    owner_id: Uuid,
    handle: Uuid,
    t: Uuid,
    /// The predecessor named by a Goal successor request. This is kept
    /// separate from the head snapshot so a successor prepared after its
    /// predecessor stopped being current cannot silently advance the series.
    expected_prior_t: Option<Uuid>,
    /// The head observed for `handle` before the lifecycle lock. The row is
    /// re-read under `FOR UPDATE` after that lock and must still be this t.
    expected_head_t: Option<Uuid>,
    command: GoalWriteCommand,
    wake: GoalWakePlan,
    targets: Vec<Uuid>,
    write_act_identity: Option<ReservedFactIdentity>,
    close_fact_identity: Option<ReservedFactIdentity>,
}

impl PreparedGoalWrite {
    #[cfg(test)]
    pub(crate) fn reserved_write_act_t(&self) -> Option<Uuid> {
        self.write_act_identity.map(|identity| identity.t)
    }

    #[cfg(test)]
    pub(crate) fn reserved_close_fact_t(&self) -> Option<Uuid> {
        self.close_fact_identity.map(|identity| identity.t)
    }
}

#[derive(Debug)]
pub(crate) enum GoalWritePreparation {
    Replay(GoalWriteOutcome),
    New(Box<PreparedGoalWrite>),
}

/// Existing Goal row for `(owner, request_id)`, if any.
#[derive(Debug, Clone, Copy)]
pub(crate) struct GoalRequestRow {
    pub handle: Uuid,
    pub t: Uuid,
    pub write_act_t: Option<Uuid>,
}

/// Replay lookup: same `(owner, request_id)` is one Goal version.
///
/// # Errors
///
/// `Internal` on query failure.
pub(crate) async fn load_goal_by_request_id(
    tx: &mut Transaction<'_, Postgres>,
    owner: &Owner,
    request_id: &str,
) -> Result<Option<GoalRequestRow>, StorageError> {
    let row = sqlx::query_as::<_, (Uuid, Uuid, Option<Uuid>)>(
        "SELECT handle, t, write_act_t FROM proxima_core.goal
          WHERE owner_id = $1 AND request_id = $2",
    )
    .bind(owner.stored_owner_id())
    .bind(request_id)
    .fetch_optional(tx.as_mut())
    .await
    .map_err(map_err)?;
    Ok(row.map(|(handle, t, write_act_t)| GoalRequestRow {
        handle,
        t,
        write_act_t,
    }))
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
    ingest_fact_timeseries(tx, owner, &draft, &[], &[], &[], None).await
}

pub(crate) async fn reserve_fact_identity(
    tx: &mut Transaction<'_, Postgres>,
) -> Result<ReservedFactIdentity, StorageError> {
    let t: Uuid = sqlx::query_scalar("SELECT uuidv7()")
        .fetch_one(tx.as_mut())
        .await
        .map_err(map_err)?;
    Ok(ReservedFactIdentity {
        handle: Uuid::now_v7(),
        t,
    })
}

pub(crate) async fn ingest_write_act_at(
    tx: &mut Transaction<'_, Postgres>,
    owner: &Owner,
    identity: ReservedFactIdentity,
) -> Result<FactIngestOutcome, StorageError> {
    let draft = FactWriteCommand {
        schema_id: proxima_core::SchemaId::new(WRITE_ACT_SCHEMA.to_string()),
        schema_version: proxima_core::SchemaVersion::new(1),
        handle: Some(identity.handle),
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
    ingest_unpinned_fact_at(tx, owner, &draft, (identity.handle, identity.t)).await
}

pub async fn write_goal(
    tx: &mut Transaction<'_, Postgres>,
    owner: &Owner,
    draft: &GoalWriteCommand,
) -> Result<GoalWriteOutcome, StorageError> {
    if let Some(existing) = load_goal_by_request_id(tx, owner, &draft.request_id).await? {
        return Ok(GoalWriteOutcome {
            handle: existing.handle,
            t: existing.t,
            write_act_t: existing.write_act_t,
            replay: true,
        });
    }
    let wake = match draft.wake_id {
        Some(wake_id) => load_goal_wake_plan(tx, wake_id).await?,
        None => GoalWakePlan::None,
    };
    match prepare_goal_write(tx, owner, draft, wake, None).await? {
        GoalWritePreparation::Replay(outcome) => Ok(outcome),
        GoalWritePreparation::New(prepared) => {
            lock_prepared_goal_write(tx, &prepared).await?;
            persist_prepared_goal_write(tx, &prepared).await
        }
    }
}

/// Prepare one low-level Goal write. Replay is checked before owner creation,
/// UUID minting, wake insertion, or head mutation so a replay has no side
/// effects to roll back.
pub(crate) async fn prepare_goal_write(
    tx: &mut Transaction<'_, Postgres>,
    owner: &Owner,
    draft: &GoalWriteCommand,
    wake: GoalWakePlan,
    expected_prior_t: Option<Uuid>,
) -> Result<GoalWritePreparation, StorageError> {
    if let Some(existing) = load_goal_by_request_id(tx, owner, &draft.request_id).await? {
        return Ok(GoalWritePreparation::Replay(GoalWriteOutcome {
            handle: existing.handle,
            t: existing.t,
            write_act_t: existing.write_act_t,
            replay: true,
        }));
    }

    let owner_id = crate::access::owner_columns::ensure_owner_row(tx.as_mut(), owner).await?;
    let write_act_identity = if draft.mint_write_act {
        Some(reserve_fact_identity(tx).await?)
    } else {
        None
    };
    let write_act_t = write_act_identity
        .map(|identity| identity.t)
        .or(draft.write_act_t);
    let handle = draft.handle.unwrap_or_else(Uuid::now_v7);
    let t: Uuid = sqlx::query_scalar("SELECT uuidv7()")
        .fetch_one(tx.as_mut())
        .await
        .map_err(map_err)?;
    let mut command = draft.clone();
    command.mint_write_act = false;
    command.write_act_t = write_act_t;
    let expected_head_t: Option<Uuid> =
        sqlx::query_scalar("SELECT t FROM proxima_core.goal_head WHERE handle = $1")
            .bind(handle)
            .fetch_optional(tx.as_mut())
            .await
            .map_err(map_err)?;
    let mut targets = Vec::with_capacity(
        1 + usize::from(command.assignment_t.is_some())
            + command.dependency_t.len()
            + command.evidence_t.len()
            + usize::from(command.close_fact_t.is_some())
            + usize::from(command.write_act_t.is_some())
            + usize::from(expected_prior_t.is_some())
            + usize::from(expected_head_t.is_some())
            + wake.target_ids().count(),
    );
    targets.push(t);
    targets.extend(expected_prior_t);
    targets.extend(expected_head_t);
    targets.extend(command.assignment_t);
    targets.extend(command.dependency_t.iter().copied());
    targets.extend(command.evidence_t.iter().copied());
    targets.extend(command.close_fact_t);
    targets.extend(command.write_act_t);
    targets.extend(wake.target_ids());
    Ok(GoalWritePreparation::New(Box::new(PreparedGoalWrite {
        owner: *owner,
        owner_id,
        handle,
        t,
        expected_prior_t,
        expected_head_t,
        command,
        wake,
        targets,
        write_act_identity,
        close_fact_identity: None,
    })))
}

pub(crate) async fn lock_prepared_goal_write(
    tx: &mut Transaction<'_, Postgres>,
    prepared: &PreparedGoalWrite,
) -> Result<(), StorageError> {
    lock_and_validate_prepared_goal_write(tx, prepared).await
}

/// Hold the complete lifecycle footprint before touching the Goal head. The
/// head row is deliberately the second lock: owner -> lifecycle -> head is
/// shared with Memory admission and transfer. A named predecessor that is no
/// longer current is a caller-fixable conflict; an unnamed snapshot drift is
/// retryable before any write-act, close Fact, wake, sidecar, Goal, sketch,
/// or announce row is persisted.
async fn lock_and_validate_prepared_goal_write(
    tx: &mut Transaction<'_, Postgres>,
    prepared: &PreparedGoalWrite,
) -> Result<(), StorageError> {
    crate::verbs::forget::lock_lifecycle_targets_tx(tx, &prepared.targets).await?;
    let current_head: Option<Uuid> =
        sqlx::query_scalar("SELECT t FROM proxima_core.goal_head WHERE handle = $1 FOR UPDATE")
            .bind(prepared.handle)
            .fetch_optional(tx.as_mut())
            .await
            .map_err(map_err)?;
    if let Some(expected_prior_t) = prepared.expected_prior_t
        && current_head != Some(expected_prior_t)
    {
        return Err(StorageError::Conflict(
            "goal successor prior is no longer current".into(),
        ));
    }
    if current_head != prepared.expected_head_t {
        return Err(StorageError::Retryable(
            "goal series head changed while preparing successor".into(),
        ));
    }
    Ok(())
}

pub(crate) fn attach_goal_close_fact_reservation(
    preparation: GoalWritePreparation,
    close_fact: Option<ReservedFactIdentity>,
) -> GoalWritePreparation {
    match preparation {
        GoalWritePreparation::Replay(outcome) => GoalWritePreparation::Replay(outcome),
        GoalWritePreparation::New(mut prepared) => {
            if let Some(close_fact) = close_fact {
                prepared.command.close_fact_t = Some(close_fact.t);
                prepared.targets.push(close_fact.t);
                prepared.close_fact_identity = Some(close_fact);
            }
            GoalWritePreparation::New(prepared)
        }
    }
}

pub(crate) async fn lock_prepared_goal_writes(
    tx: &mut Transaction<'_, Postgres>,
    prepared: &[&GoalWritePreparation],
) -> Result<(), StorageError> {
    let mut targets = Vec::new();
    for item in prepared {
        if let GoalWritePreparation::New(item) = item {
            targets.extend(item.targets.iter().copied());
        }
    }
    crate::verbs::forget::lock_lifecycle_targets_tx(tx, &targets).await
}

pub(crate) async fn persist_prepared_goal_write(
    tx: &mut Transaction<'_, Postgres>,
    prepared: &PreparedGoalWrite,
) -> Result<GoalWriteOutcome, StorageError> {
    // This helper is also called directly by decomposition and crate tests;
    // re-enter the advisory set and re-check the head so persistence remains
    // safe even when a caller already performed the union lock.
    lock_and_validate_prepared_goal_write(tx, prepared).await?;
    let reserved_lifecycle = [
        prepared.write_act_identity.map(|identity| identity.t),
        prepared.close_fact_identity.map(|identity| identity.t),
    ];
    validate_goal_targets_live(tx, &prepared.command, &reserved_lifecycle).await?;
    if let GoalWakePlan::Existing {
        wake_id,
        trigger_t,
        hard_memory_t,
    } = &prepared.wake
    {
        revalidate_wake_plan(tx, *wake_id, *trigger_t, hard_memory_t).await?;
    }
    // Lifecycle Facts use the exact reservations included in `targets`.
    // Persist them only after the caller has acquired the complete union, so
    // a blocked Goal preparation cannot leave a write-act or close Fact.
    if let Some(identity) = prepared.write_act_identity {
        ingest_write_act_at(tx, &prepared.owner, identity).await?;
    }
    if let Some(identity) = prepared.close_fact_identity {
        ingest_write_act_at(tx, &prepared.owner, identity).await?;
    }
    let wake_id = match &prepared.wake {
        GoalWakePlan::None => None,
        GoalWakePlan::New(draft) => Some(insert_wake_config(tx, &prepared.owner, draft).await?),
        GoalWakePlan::Existing { wake_id, .. } => Some(*wake_id),
    };
    let head = sqlx::query(
        "INSERT INTO proxima_core.goal_head (handle, schema_id, owner_id, t)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT (handle) DO UPDATE SET t = EXCLUDED.t
         WHERE proxima_core.goal_head.schema_id = EXCLUDED.schema_id
           AND proxima_core.goal_head.owner_id = EXCLUDED.owner_id
           AND ($5::uuid IS NOT NULL AND proxima_core.goal_head.t = $5)
         RETURNING handle",
    )
    .bind(prepared.handle)
    .bind(&prepared.command.schema_id)
    .bind(prepared.owner_id)
    .bind(prepared.t)
    .bind(prepared.expected_head_t)
    .fetch_optional(tx.as_mut())
    .await
    .map_err(map_err)?;
    if head.is_none() {
        let current: Option<(String, Uuid, Uuid)> = sqlx::query_as(
            "SELECT schema_id, owner_id, t FROM proxima_core.goal_head WHERE handle = $1",
        )
        .bind(prepared.handle)
        .fetch_optional(tx.as_mut())
        .await
        .map_err(map_err)?;
        match current {
            Some((schema_id, owner_id, _))
                if schema_id != prepared.command.schema_id || owner_id != prepared.owner_id =>
            {
                return Err(StorageError::ConstraintViolation(
                    "goal_head schema/owner mismatch".into(),
                ));
            }
            Some((_, _, current_t))
                if prepared
                    .expected_prior_t
                    .is_some_and(|expected_prior_t| current_t != expected_prior_t) =>
            {
                return Err(StorageError::Conflict(
                    "goal successor prior is no longer current".into(),
                ));
            }
            Some(_) => {
                return Err(StorageError::Retryable(
                    "goal series head changed while persisting Goal".into(),
                ));
            }
            None if prepared.expected_prior_t.is_some() => {
                return Err(StorageError::Conflict(
                    "goal successor prior is no longer current".into(),
                ));
            }
            None => {
                return Err(StorageError::Retryable(
                    "goal series head disappeared while persisting Goal".into(),
                ));
            }
        }
    }

    sqlx::query(
        "INSERT INTO proxima_core.goal
            (handle, t, owner_id, title, state, request_id, close_fact_t,
             assignment_t, dependency_t, evidence_t, wake_id, write_act_t)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
    )
    .bind(prepared.handle)
    .bind(prepared.t)
    .bind(prepared.owner_id)
    .bind(&prepared.command.title)
    .bind(prepared.command.state)
    .bind(&prepared.command.request_id)
    .bind(prepared.command.close_fact_t)
    .bind(prepared.command.assignment_t)
    .bind(&prepared.command.dependency_t)
    .bind(&prepared.command.evidence_t)
    .bind(wake_id)
    .bind(prepared.command.write_act_t)
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;

    crate::verbs::sketch::upsert_sketch(
        tx,
        prepared.owner_id,
        prepared.t,
        "goal",
        &prepared.command.title,
    )
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.announce (owner_id, op, entity, handle, t)
         VALUES ($1, 'append', 'goal', $2, $3)",
    )
    .bind(prepared.owner_id)
    .bind(prepared.handle)
    .bind(prepared.t)
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;
    Ok(GoalWriteOutcome {
        handle: prepared.handle,
        t: prepared.t,
        write_act_t: prepared.command.write_act_t,
        replay: false,
    })
}

async fn validate_goal_targets_live(
    tx: &mut Transaction<'_, Postgres>,
    draft: &GoalWriteCommand,
    reserved_lifecycle: &[Option<Uuid>; 2],
) -> Result<(), StorageError> {
    // Retained Goal rows may keep cooled or witnessed pins for historical
    // validity; a new successor is fresh admission and must reauthorize hot,
    // readable assignment, evidence, and lifecycle targets. Hydrate first.
    if let Some(assignment) = draft.assignment_t {
        ensure_live_memory_kind(
            tx,
            assignment,
            "goal assignment perspective",
            &["perspective"],
        )
        .await?;
    }
    for dependency in &draft.dependency_t {
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM proxima_core.goal WHERE t = $1)")
                .bind(dependency)
                .fetch_one(tx.as_mut())
                .await
                .map_err(map_err)?;
        if !exists {
            return Err(StorageError::ConstraintViolation(
                "goal dependency does not exist".into(),
            ));
        }
    }
    for evidence in &draft.evidence_t {
        ensure_live_memory_kind(tx, *evidence, "goal evidence", &["fact", "abstraction"]).await?;
    }
    for target in [draft.close_fact_t, draft.write_act_t]
        .into_iter()
        .flatten()
    {
        if reserved_lifecycle.contains(&Some(target)) {
            continue;
        }
        ensure_live_memory_kind(tx, target, "goal lifecycle Fact", &["fact"]).await?;
    }
    Ok(())
}

async fn ensure_live_memory(
    tx: &mut Transaction<'_, Postgres>,
    t: Uuid,
    label: &str,
) -> Result<(), StorageError> {
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM proxima_core.memory WHERE t = $1)")
            .bind(t)
            .fetch_one(tx.as_mut())
            .await
            .map_err(map_err)?;
    if exists {
        Ok(())
    } else {
        Err(StorageError::ConstraintViolation(format!(
            "{label} does not exist"
        )))
    }
}

async fn ensure_live_memory_kind(
    tx: &mut Transaction<'_, Postgres>,
    t: Uuid,
    label: &str,
    allowed: &[&str],
) -> Result<(), StorageError> {
    let kind: Option<String> =
        sqlx::query_scalar("SELECT kind::text FROM proxima_core.memory WHERE t = $1")
            .bind(t)
            .fetch_optional(tx.as_mut())
            .await
            .map_err(map_err)?;
    let Some(kind) = kind else {
        return Err(StorageError::ConstraintViolation(format!(
            "{label} does not exist"
        )));
    };
    if allowed.contains(&kind.as_str()) {
        Ok(())
    } else {
        Err(StorageError::ConstraintViolation(format!(
            "{label} must be {}",
            allowed.join(" or ")
        )))
    }
}

pub(crate) async fn load_goal_wake_plan(
    tx: &mut Transaction<'_, Postgres>,
    wake_id: Uuid,
) -> Result<GoalWakePlan, StorageError> {
    let row: Option<(Option<Uuid>, Vec<Uuid>)> = sqlx::query_as(
        "SELECT trigger_t, hard_memory_t FROM proxima_core.wake_config WHERE wake_id = $1",
    )
    .bind(wake_id)
    .fetch_optional(tx.as_mut())
    .await
    .map_err(map_err)?;
    let Some((trigger_t, hard_memory_t)) = row else {
        return Err(StorageError::ConstraintViolation(
            "wake configuration does not exist".into(),
        ));
    };
    Ok(GoalWakePlan::Existing {
        wake_id,
        trigger_t,
        hard_memory_t,
    })
}

async fn revalidate_wake_plan(
    tx: &mut Transaction<'_, Postgres>,
    wake_id: Uuid,
    trigger_t: Option<Uuid>,
    hard_memory_t: &[Uuid],
) -> Result<(), StorageError> {
    // A retained Goal may carry a cooled wake config, but carrying it into a
    // successor is fresh admission: trigger and hard context must be hot and
    // readable after the config snapshot is revalidated. Hydrate first.
    let current = load_goal_wake_plan(tx, wake_id).await?;
    let GoalWakePlan::Existing {
        trigger_t: current_trigger,
        hard_memory_t: current_hard,
        ..
    } = current
    else {
        unreachable!()
    };
    if current_trigger != trigger_t || current_hard != hard_memory_t {
        return Err(StorageError::Retryable(
            "wake targets changed while Goal was prepared".into(),
        ));
    }
    if let Some(trigger_t) = trigger_t {
        ensure_live_memory_kind(tx, trigger_t, "wake trigger memory", &["fact"]).await?;
    }
    for target in hard_memory_t {
        ensure_live_memory(tx, *target, "wake hard context memory").await?;
    }
    Ok(())
}
