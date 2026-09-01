use super::wake::prepare_goal_wake_plan;
use super::{
    GoalAtomicContext, GoalDraft, GoalId, GoalState, GoalWritePreparation, InsertedGoal,
    PayloadKind, PgSidecarKey, PgSidecarRegistryFrozen, Postgres, StorageError, Transaction,
    WakeWrite, lock_prepared_goal_write, persist_prepared_goal_write, prepare_goal_write,
};
use crate::verbs::goal_timeseries::{
    GoalWriteCommand, attach_goal_close_fact_reservation, load_goal_by_request_id,
    reserve_fact_identity,
};

pub(super) struct PreparedGoalInsert<'a> {
    pub(super) draft: GoalDraft,
    pub(super) context: GoalAtomicContext<'a>,
    pub(super) expected_prior: Option<GoalId>,
    pub(super) preparation: GoalWritePreparation,
}

pub(super) async fn insert_or_replay_goal(
    tx: &mut Transaction<'_, Postgres>,
    sidecars: &PgSidecarRegistryFrozen,
    draft: &GoalDraft,
    expected_prior: Option<GoalId>,
    context: GoalAtomicContext<'_>,
    wake_write: WakeWrite<'_>,
    write_act_t: Option<uuid::Uuid>,
) -> Result<InsertedGoal, StorageError> {
    let prepared =
        prepare_goal_insert(tx, draft, expected_prior, context, wake_write, write_act_t).await?;
    if let GoalWritePreparation::New(prepared_write) = &prepared.preparation {
        lock_prepared_goal_write(tx, prepared_write).await?;
    }
    persist_prepared_goal_insert(tx, sidecars, &prepared).await
}

pub(super) async fn prepare_goal_insert<'a>(
    tx: &mut Transaction<'_, Postgres>,
    draft: &GoalDraft,
    expected_prior: Option<GoalId>,
    context: GoalAtomicContext<'a>,
    wake_write: WakeWrite<'_>,
    write_act_t: Option<uuid::Uuid>,
) -> Result<PreparedGoalInsert<'a>, StorageError> {
    let owner = draft.owner();

    // Replay detection precedes prior-head and wake lookups: a replay must
    // not become dependent on a target that a later erase removed.
    if let Some(existing) = load_goal_by_request_id(tx, &owner, &draft.request_id).await? {
        return Ok(PreparedGoalInsert {
            draft: draft.clone(),
            context,
            expected_prior,
            preparation: GoalWritePreparation::Replay(
                crate::verbs::goal_timeseries::GoalWriteOutcome {
                    handle: existing.handle,
                    t: existing.t,
                    write_act_t: existing.write_act_t,
                    replay: true,
                },
            ),
        });
    }
    validate_goal_schema(context, draft)?;

    let handle = if let Some(prior) = expected_prior {
        let handle: Option<uuid::Uuid> = sqlx::query_scalar(
            "SELECT handle FROM proxima_core.goal WHERE t = $1 AND owner_id = $2",
        )
        .bind(prior.into_inner())
        .bind(owner.stored_owner_id())
        .fetch_optional(&mut **tx)
        .await
        .map_err(crate::error::map_err)?;
        Some(handle.ok_or(StorageError::NotFound)?)
    } else {
        None
    };

    let command = GoalWriteCommand {
        handle,
        schema_id: draft.schema_id.as_str().to_string(),
        title: draft.title.clone(),
        state: draft.state,
        request_id: draft.request_id.clone(),
        close_fact_t: None,
        assignment_t: Some(draft.topology.assignment().perspective_id().into_inner()),
        dependency_t: draft
            .topology
            .dependencies()
            .iter()
            .map(|dependency| dependency.goal_id().into_inner())
            .collect(),
        evidence_t: draft
            .topology
            .evidence()
            .iter()
            .map(|item| item.memory_id().into_inner())
            .collect(),
        wake_id: None,
        mint_write_act: false,
        write_act_t,
    };
    let wake = prepare_goal_wake_plan(tx, context, &owner, wake_write).await?;
    let preparation = prepare_goal_write(
        tx,
        &owner,
        &command,
        wake,
        expected_prior.map(GoalId::into_inner),
    )
    .await?;
    let close_fact = match &preparation {
        GoalWritePreparation::New(_)
            if matches!(draft.state, GoalState::Achieved | GoalState::Abandoned) =>
        {
            Some(reserve_fact_identity(tx).await?)
        }
        GoalWritePreparation::Replay(_) | GoalWritePreparation::New(_) => None,
    };
    let preparation = attach_goal_close_fact_reservation(preparation, close_fact);
    Ok(PreparedGoalInsert {
        draft: draft.clone(),
        context,
        expected_prior,
        preparation,
    })
}

pub(super) async fn persist_prepared_goal_insert(
    tx: &mut Transaction<'_, Postgres>,
    sidecars: &PgSidecarRegistryFrozen,
    prepared: &PreparedGoalInsert<'_>,
) -> Result<InsertedGoal, StorageError> {
    let out = match &prepared.preparation {
        GoalWritePreparation::Replay(outcome) => outcome.clone(),
        GoalWritePreparation::New(prepared) => persist_prepared_goal_write(tx, prepared).await?,
    };
    if !out.replay {
        insert_goal_sidecar(
            tx,
            sidecars,
            prepared.context,
            &prepared.draft,
            GoalId::new(out.t),
            prepared.expected_prior,
        )
        .await?;
    }

    Ok(InsertedGoal {
        goal_id: GoalId::new(out.t),
        change_event_seq: out.t,
        idempotent_replay: out.replay,
    })
}

fn validate_goal_schema(
    context: GoalAtomicContext<'_>,
    draft: &GoalDraft,
) -> Result<(), StorageError> {
    context
        .registry
        .lookup_payload(&draft.schema_id, draft.schema_version, PayloadKind::Goal)
        .ok_or_else(|| {
            StorageError::ConstraintViolation(format!(
                "unregistered GoalPayload schema {} v{}",
                draft.schema_id.as_str(),
                draft.schema_version.into_inner(),
            ))
        })?;
    Ok(())
}

async fn insert_goal_sidecar(
    tx: &mut Transaction<'_, Postgres>,
    sidecars: &PgSidecarRegistryFrozen,
    context: GoalAtomicContext<'_>,
    draft: &GoalDraft,
    goal_id: GoalId,
    source_goal_id: Option<GoalId>,
) -> Result<(), StorageError> {
    let Some(sidecar_table) = context
        .registry
        .lookup_payload(&draft.schema_id, draft.schema_version, PayloadKind::Goal)
        .and_then(|schema| schema.sidecar_table.as_deref())
    else {
        return Ok(());
    };
    if let Some(payload) = &draft.sidecar_payload {
        if payload.kind != PayloadKind::Goal
            || payload.schema_id != draft.schema_id
            || payload.schema_version != draft.schema_version
        {
            return Err(StorageError::ConstraintViolation(format!(
                "Goal sidecar payload drift for {} v{} table {sidecar_table}",
                draft.schema_id.as_str(),
                draft.schema_version.into_inner(),
            )));
        }
        sidecars.insert_goal_sidecar(tx, goal_id, payload).await?;
        return Ok(());
    }

    if let Some(source_goal_id) = source_goal_id {
        let key = PgSidecarKey::new(
            PayloadKind::Goal,
            draft.schema_id.clone(),
            draft.schema_version,
        );
        sidecars
            .copy_goal_sidecar(tx, key, goal_id, source_goal_id)
            .await?;
        return Ok(());
    }

    Err(StorageError::ConstraintViolation(format!(
        "missing typed Goal sidecar payload for {} v{} table {sidecar_table}",
        draft.schema_id.as_str(),
        draft.schema_version.into_inner(),
    )))
}
