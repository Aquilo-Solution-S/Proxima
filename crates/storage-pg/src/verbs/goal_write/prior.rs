use super::{
    ChildGoalDraft, GoalAuthorship, GoalDraft, GoalId, GoalPayloadWrite, GoalState, MemoryId,
    Owner, Postgres, SchemaId, SchemaVersion, StorageError, StoredGoal, StoredGoalRow, Transaction,
    map_err,
};

pub(super) async fn load_prior_goal(
    tx: &mut Transaction<'_, Postgres>,
    owner: &Owner,
    goal_id: GoalId,
) -> Result<StoredGoal, StorageError> {
    let (owner_kind, owner_id) = owner.columns();
    let row: Option<StoredGoalRow> = sqlx::query_as(
        "SELECT schema_id, schema_version, title, text, payload, state
           FROM proxima_core.goals
          WHERE goal_id = $1
            AND owner_kind = $2
            AND owner_id IS NOT DISTINCT FROM $3",
    )
    .bind(goal_id.into_inner())
    .bind(owner_kind)
    .bind(owner_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_err)?;
    let Some(row) = row else {
        return Err(StorageError::NotFound);
    };
    Ok(StoredGoal {
        schema_id: SchemaId::new(row.schema_id),
        schema_version: SchemaVersion::new(row.schema_version.cast_unsigned()),
        title: row.title,
        text: row.text,
        payload: row.payload,
        state: row.state,
        assignment: goal_assignment_target(tx, goal_id).await?,
        dependencies: dependency_goal_ids(tx, goal_id).await?,
    })
}

pub(super) async fn dependency_goal_ids(
    tx: &mut Transaction<'_, Postgres>,
    goal_id: GoalId,
) -> Result<Vec<GoalId>, StorageError> {
    let rows: Vec<(uuid::Uuid,)> = sqlx::query_as(
        "SELECT target_goal_id
           FROM proxima_core.edges
          WHERE source_goal_id = $1
            AND relation = 'core/depends-on'
            AND target_goal_id IS NOT NULL
          ORDER BY target_goal_id",
    )
    .bind(goal_id.into_inner())
    .fetch_all(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(rows.into_iter().map(|(id,)| GoalId::new(id)).collect())
}

async fn goal_assignment_target(
    tx: &mut Transaction<'_, Postgres>,
    goal_id: GoalId,
) -> Result<MemoryId, StorageError> {
    let row: Option<(uuid::Uuid,)> = sqlx::query_as(
        "SELECT target_memory_id
           FROM proxima_core.edges
          WHERE source_goal_id = $1
            AND relation = 'core/inspires'
            AND target_memory_id IS NOT NULL
          ORDER BY created_at ASC
          LIMIT 1",
    )
    .bind(goal_id.into_inner())
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_err)?;
    row.map(|(id,)| MemoryId::new(id))
        .ok_or_else(|| StorageError::ConstraintViolation("goal assignment edge missing".into()))
}

pub(super) async fn validate_active_head(
    tx: &mut Transaction<'_, Postgres>,
    owner: &Owner,
    goal_id: GoalId,
) -> Result<(), StorageError> {
    let prior = load_prior_goal(tx, owner, goal_id).await?;
    if prior.state != GoalState::Active {
        return Err(StorageError::ConstraintViolation(
            "parent_goal must be Active".into(),
        ));
    }
    let (owner_kind, owner_id) = owner.columns();
    let newer_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (
             SELECT 1
               FROM proxima_core.goals
              WHERE supersedes = $1
                AND owner_kind = $2
                AND owner_id IS NOT DISTINCT FROM $3
         )",
    )
    .bind(goal_id.into_inner())
    .bind(owner_kind)
    .bind(owner_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(map_err)?;
    if newer_exists {
        Err(StorageError::Conflict(
            "parent_goal is not current head".into(),
        ))
    } else {
        Ok(())
    }
}

pub(super) fn validate_goal_transition(
    prior: GoalState,
    next: GoalState,
) -> Result<(), StorageError> {
    match (prior, next) {
        (
            GoalState::Active,
            GoalState::Active | GoalState::Paused | GoalState::Achieved | GoalState::Abandoned,
        )
        | (GoalState::Paused, GoalState::Active) => Ok(()),
        _ => Err(StorageError::ConstraintViolation(format!(
            "invalid goal transition: {prior:?} -> {next:?}",
        ))),
    }
}

pub(super) fn draft_from_stored(
    owner: &Owner,
    stored: &StoredGoal,
    state: GoalState,
    supersedes: Option<GoalId>,
    authorship: GoalAuthorship,
    request_id: &str,
) -> GoalDraft {
    GoalDraft {
        owner: *owner,
        schema_id: stored.schema_id.clone(),
        schema_version: stored.schema_version,
        title: stored.title.clone(),
        text: stored.text.clone(),
        payload: stored.payload.clone(),
        sidecar_payload: None,
        state,
        topology: proxima_core::GoalTopologyWrite::new(
            proxima_core::GoalAssignmentTarget::perspective(stored.assignment),
            stored
                .dependencies
                .iter()
                .copied()
                .map(proxima_core::GoalDependencyRef::new)
                .collect(),
            Vec::new(),
        )
        .expect("stored topology has unique dependencies"),
        wake: None,
        supersedes_goal_id: supersedes,
        authorship,
        request_id: request_id.to_string(),
    }
}

pub(super) struct DraftFromPayload<'a> {
    pub(super) owner: &'a Owner,
    pub(super) payload: &'a GoalPayloadWrite,
    pub(super) state: GoalState,
    pub(super) assignment: MemoryId,
    pub(super) dependencies: Vec<GoalId>,
    pub(super) supersedes: Option<GoalId>,
    pub(super) authorship: GoalAuthorship,
    pub(super) request_id: &'a str,
}

pub(super) fn draft_from_payload(input: DraftFromPayload<'_>) -> GoalDraft {
    GoalDraft {
        owner: *input.owner,
        schema_id: input.payload.schema_id.clone(),
        schema_version: input.payload.schema_version,
        title: input.payload.title.clone(),
        text: input.payload.text.clone(),
        payload: input.payload.payload.clone(),
        sidecar_payload: input.payload.sidecar_payload.clone(),
        state: input.state,
        topology: proxima_core::GoalTopologyWrite::new(
            proxima_core::GoalAssignmentTarget::perspective(input.assignment),
            input
                .dependencies
                .into_iter()
                .map(proxima_core::GoalDependencyRef::new)
                .collect(),
            Vec::new(),
        )
        .expect("stored topology has unique dependencies"),
        wake: None,
        supersedes_goal_id: input.supersedes,
        authorship: input.authorship,
        request_id: input.request_id.to_string(),
    }
}

pub(super) fn child_draft(
    owner: &Owner,
    parent_goal_id: GoalId,
    topology: &proxima_core::GoalTopologyWrite,
    authorship: &GoalAuthorship,
    child: &ChildGoalDraft,
) -> Result<GoalDraft, StorageError> {
    let mut dependencies = topology.dependencies().to_vec();
    dependencies.push(proxima_core::GoalDependencyRef::new(parent_goal_id));
    let child_topology = proxima_core::GoalTopologyWrite::new(
        topology.assignment(),
        dependencies,
        child.evidence.clone(),
    )
    .map_err(|err| StorageError::ConstraintViolation(err.message))?;
    Ok(GoalDraft::active_from_payload_write(
        *owner,
        child.payload.clone(),
        child_topology,
        child.wake.clone(),
        authorship.clone(),
        child.request_id.clone(),
    ))
}
