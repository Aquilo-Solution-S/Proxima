use super::wake::write_goal_wake_config;
use super::{
    GoalAtomicContext, GoalDraft, GoalId, GoalState, InsertedGoal, PayloadKind,
    PgSidecarKey, PgSidecarRegistryFrozen, Postgres, StorageError, Transaction, WakeWrite,
};
use crate::verbs::goal_timeseries::{GoalWriteCommand, ingest_write_act, write_goal};

pub(super) async fn insert_or_replay_goal(
    tx: &mut Transaction<'_, Postgres>,
    sidecars: &PgSidecarRegistryFrozen,
    draft: &GoalDraft,
    expected_prior: Option<GoalId>,
    context: GoalAtomicContext<'_>,
    wake_write: WakeWrite<'_>,
) -> Result<InsertedGoal, StorageError> {
    validate_goal_schema(context, draft)?;
    let owner = draft.owner();
    crate::access::owner_columns::reject_world_write_owner(&owner)?;

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

    let wake_id = write_goal_wake_config(tx, context, &owner, wake_write).await?;
    let terminal = matches!(draft.state, GoalState::Achieved | GoalState::Abandoned);
    let close_fact_t = if terminal {
        Some(ingest_write_act(tx, &owner).await?.memory_id.into_inner())
    } else {
        None
    };
    let out = write_goal(
        tx,
        &owner,
        &GoalWriteCommand {
            handle,
            schema_id: draft.schema_id.as_str().to_string(),
            title: draft.title.clone(),
            state: draft.state,
            request_id: draft.request_id.clone(),
            close_fact_t,
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
            wake_id,
            mint_write_act: false,
        },
    )
    .await?;

    if !out.replay {
        insert_goal_sidecar(
            tx,
            sidecars,
            context,
            draft,
            GoalId::new(out.t),
            expected_prior,
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
