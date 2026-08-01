use super::wake::write_goal_wake_config;
use super::{
    ExistingGoalRow, GoalAtomicContext, GoalDraft, GoalId, GoalState, InsertedGoal, PayloadKind,
    PgSidecarKey, PgSidecarRegistryFrozen, Postgres, StorageError, Transaction, WakeWrite,
    authorship_columns, authorship_matches, existing_goal_body_matches, idempotency_conflict,
    map_err, validate_active_head,
};

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

    if let Some(inserted) = replay_existing_goal(tx, draft, expected_prior, wake_write).await? {
        return Ok(inserted);
    }

    // Two concurrent same-key goal creates both miss the replay lookup
    // above; the loser collides on `goals_idempotency_key`. Guard the insert
    // with a SAVEPOINT so the mid-tx unique violation does not poison the
    // whole transaction — then roll back and replay the winner's committed
    // goal instead of surfacing a spurious ConstraintViolation.
    sqlx::query("SAVEPOINT proxima_goal_insert")
        .execute(&mut **tx)
        .await
        .map_err(map_err)?;
    match insert_new_goal(tx, sidecars, draft, expected_prior, context, wake_write).await {
        Ok(inserted) => {
            sqlx::query("RELEASE SAVEPOINT proxima_goal_insert")
                .execute(&mut **tx)
                .await
                .map_err(map_err)?;
            Ok(inserted)
        }
        Err(err) if is_goal_idempotency_race(&err) => {
            sqlx::query("ROLLBACK TO SAVEPOINT proxima_goal_insert")
                .execute(&mut **tx)
                .await
                .map_err(map_err)?;
            match replay_existing_goal(tx, draft, expected_prior, wake_write).await? {
                Some(inserted) => Ok(inserted),
                None => Err(err),
            }
        }
        Err(err) => Err(err),
    }
}

/// Look up an already-committed goal for this idempotency key and, if the
/// stored body/authorship match the draft, return it as an idempotent replay.
/// A key match with a different body is an [`idempotency_conflict`].
async fn replay_existing_goal(
    tx: &mut Transaction<'_, Postgres>,
    draft: &GoalDraft,
    expected_prior: Option<GoalId>,
    wake_write: WakeWrite<'_>,
) -> Result<Option<InsertedGoal>, StorageError> {
    let owner = draft.owner();
    let (owner_kind, owner_id) = owner.columns();
    let existing: Option<ExistingGoalRow> = sqlx::query_as(
        "SELECT g.goal_id, ce.seq
           FROM proxima_core.goals g
           JOIN proxima_core.change_event ce ON ce.entity_goal_id = g.goal_id
          WHERE g.idempotency_key = md5($1::text || ':' || $2::text || ':' || $3)
          ORDER BY ce.seq ASC
          LIMIT 1",
    )
    .bind(owner_kind)
    .bind(owner_id)
    .bind(&draft.request_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_err)?;

    let Some(existing) = existing else {
        return Ok(None);
    };
    let body_matches =
        existing_goal_body_matches(tx, existing.goal_id, draft, expected_prior, wake_write).await?;
    if body_matches && authorship_matches(tx, existing.goal_id, &draft.authorship).await? {
        return Ok(Some(InsertedGoal {
            goal_id: GoalId::new(existing.goal_id),
            change_event_seq: existing.seq,
            idempotent_replay: true,
        }));
    }
    Err(idempotency_conflict(&draft.request_id))
}

async fn insert_new_goal(
    tx: &mut Transaction<'_, Postgres>,
    sidecars: &PgSidecarRegistryFrozen,
    draft: &GoalDraft,
    expected_prior: Option<GoalId>,
    context: GoalAtomicContext<'_>,
    wake_write: WakeWrite<'_>,
) -> Result<InsertedGoal, StorageError> {
    let goal_id = uuid::Uuid::now_v7();
    let change_seq = uuid::Uuid::now_v7();
    validate_goal_dependencies(tx, draft).await?;
    insert_goal_row(tx, draft, goal_id, expected_prior).await?;
    if let Some(prior) = expected_prior {
        point_prior_goal_head_forward(tx, prior, goal_id).await?;
    }
    insert_goal_sidecar(
        tx,
        sidecars,
        context,
        draft,
        GoalId::new(goal_id),
        expected_prior,
    )
    .await?;
    write_goal_wake_config(tx, context, GoalId::new(goal_id), wake_write).await?;
    insert_goal_change_event(tx, draft, goal_id, change_seq, expected_prior).await?;
    Ok(InsertedGoal {
        goal_id: GoalId::new(goal_id),
        change_event_seq: change_seq,
        idempotent_replay: false,
    })
}

/// True when a goal insert failed because another transaction already claimed
/// the same `goals_idempotency_key` (the idempotent-race sentinel from
/// [`map_goal_insert_err`]).
fn is_goal_idempotency_race(err: &StorageError) -> bool {
    matches!(err, StorageError::Conflict(message) if message == "goals_idempotency_key")
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

async fn insert_goal_row(
    tx: &mut Transaction<'_, Postgres>,
    draft: &GoalDraft,
    goal_id: uuid::Uuid,
    supersedes: Option<GoalId>,
) -> Result<(), StorageError> {
    if supersedes.is_none() && draft.state != GoalState::Active {
        return Err(StorageError::ConstraintViolation(
            "root goal rows must be Active".into(),
        ));
    }
    let owner = draft.owner();
    let (owner_kind, owner_id) = owner.columns();
    let authorship = authorship_columns(&draft.authorship);
    // The topology columns travel with the row: the Goal is the home of the
    // statement that it inspires this Perspective, waits on these Goals and
    // rests on this evidence. The reference rows in `edges` are derived from
    // them afterwards, in this same transaction.
    sqlx::query(
        "INSERT INTO proxima_core.goals
            (goal_id, schema_id, schema_version, owner_kind, owner_id,
             title, text, payload, state, supersedes,
             authorship_kind, authorship_origin, authorship_operator_id,
             authorship_tool_id, operator_kind, input_contract_id, model_id, prompt_version,
             request_id, idempotency_key,
             assignment_perspective_id, dependency_goal_ids, evidence_memory_ids)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10,
                 $11, $12, $13, $14, $15, $16, $17, $18, $19,
                 md5($4::text || ':' || $5::text || ':' || $19),
                 $20, $21, $22)",
    )
    .bind(goal_id)
    .bind(draft.schema_id.as_str())
    .bind(draft.schema_version.into_inner().cast_signed())
    .bind(owner_kind)
    .bind(owner_id)
    .bind(&draft.title)
    .bind(&draft.text)
    .bind(&draft.payload)
    .bind(draft.state)
    .bind(supersedes.map(GoalId::into_inner))
    .bind(authorship.authorship_kind)
    .bind(authorship.authorship_origin)
    .bind(authorship.authorship_operator_id)
    .bind(authorship.authorship_tool_id)
    .bind(authorship.operator_kind)
    .bind(authorship.input_contract_id)
    .bind(authorship.model_id)
    .bind(authorship.prompt_version)
    .bind(&draft.request_id)
    .bind(draft.topology.assignment().perspective_id().into_inner())
    .bind(
        draft
            .topology
            .dependencies()
            .iter()
            .map(|dependency| dependency.goal_id().into_inner())
            .collect::<Vec<_>>(),
    )
    .bind(
        draft
            .topology
            .evidence()
            .iter()
            .map(|item| item.memory_id().into_inner())
            .collect::<Vec<_>>(),
    )
    .execute(&mut **tx)
    .await
    .map_err(map_goal_insert_err)?;
    Ok(())
}

/// Close the lineage pointer on the Goal this write revises. Same shape as
/// the memory-side pointer: the successor already carries `supersedes`, and
/// this stamps the predecessor.
async fn point_prior_goal_head_forward(
    tx: &mut Transaction<'_, Postgres>,
    prior: GoalId,
    successor: uuid::Uuid,
) -> Result<(), StorageError> {
    let updated = sqlx::query(
        "UPDATE proxima_core.goals
            SET superseded_by = $2
          WHERE goal_id = $1
            AND superseded_by IS NULL",
    )
    .bind(prior.into_inner())
    .bind(successor)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    if updated.rows_affected() == 1 {
        Ok(())
    } else {
        Err(StorageError::Conflict("stale goal head".into()))
    }
}

fn map_goal_insert_err(err: sqlx::Error) -> StorageError {
    if let sqlx::Error::Database(db) = &err
        && db.is_unique_violation()
    {
        match db.constraint() {
            Some("goals_supersedes_unique") => {
                return StorageError::Conflict("stale goal head".into());
            }
            // Idempotent-race sentinel: the caller rolls back to its
            // SAVEPOINT and replays the winner's committed goal.
            Some("goals_idempotency_key") => {
                return StorageError::Conflict("goals_idempotency_key".into());
            }
            _ => {}
        }
    }
    map_err(err)
}

/// A declared dependency must be a live head at write time. The reference
/// row that follows is derived from the column, so this is the only place the
/// dependency is checked.
async fn validate_goal_dependencies(
    tx: &mut Transaction<'_, Postgres>,
    draft: &GoalDraft,
) -> Result<(), StorageError> {
    let owner = draft.owner();
    for dependency in draft.topology.dependencies() {
        validate_active_head(tx, &owner, dependency.goal_id()).await?;
    }
    Ok(())
}

async fn insert_goal_change_event(
    tx: &mut Transaction<'_, Postgres>,
    draft: &GoalDraft,
    goal_id: uuid::Uuid,
    change_seq: uuid::Uuid,
    supersedes_goal_id: Option<GoalId>,
) -> Result<(), StorageError> {
    let owner = draft.owner();
    let (owner_kind, owner_id) = owner.columns();
    sqlx::query(
        "INSERT INTO proxima_core.change_event
            (seq, owner_kind, owner_id,
             kind, entity_kind, entity_goal_id, entity_schema_id,
             entity_schema_version, supersedes_goal_id)
         VALUES ($1, $2, $3, 'EntityAppend', 'Goal', $4, $5, $6, $7)",
    )
    .bind(change_seq)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(goal_id)
    .bind(draft.schema_id.as_str())
    .bind(draft.schema_version.into_inner().cast_signed())
    .bind(supersedes_goal_id.map(GoalId::into_inner))
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(())
}
