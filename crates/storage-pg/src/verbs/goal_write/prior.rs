use super::{
    ChildGoalDraft, EvidenceTarget, GoalAuthorship, GoalDraft, GoalId, GoalPayloadWrite, GoalState,
    MemoryId, OwnerId, Postgres, SchemaId, SchemaVersion, StorageError, StoredGoal, Transaction,
    map_err,
};

type PriorRow = (
    String,
    String,
    Option<String>,
    GoalState,
    Option<uuid::Uuid>,
    Vec<uuid::Uuid>,
);

pub(super) async fn load_prior_goal(
    tx: &mut Transaction<'_, Postgres>,
    owner: &OwnerId,
    goal_id: GoalId,
) -> Result<StoredGoal, StorageError> {
    let owner_id = owner.into_inner();
    let row: Option<PriorRow> = sqlx::query_as(
        "SELECT h.schema_id, g.title, g.text, g.state, g.assignment_t, g.dependency_t
               FROM proxima_core.goal g
               JOIN proxima_core.goal_head h ON h.handle = g.handle
              WHERE g.t = $1 AND g.owner_id = $2",
    )
    .bind(goal_id.into_inner())
    .bind(owner_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_err)?;
    let Some((schema_id, title, text, state, assignment, dependency_t)) = row else {
        return Err(StorageError::NotFound);
    };
    Ok(StoredGoal {
        schema_id: SchemaId::new(schema_id),
        schema_version: SchemaVersion::new(1),
        title,
        text,
        payload: Vec::new(),
        state,
        assignment: assignment.map(MemoryId::new),
        dependencies: dependency_t.into_iter().map(GoalId::new).collect(),
    })
}

pub(super) async fn validate_active_head(
    tx: &mut Transaction<'_, Postgres>,
    owner: &OwnerId,
    goal_id: GoalId,
) -> Result<(), StorageError> {
    let prior = load_prior_goal(tx, owner, goal_id).await?;
    if prior.state != GoalState::Active {
        return Err(StorageError::ConstraintViolation(
            "parent_goal must be Active".into(),
        ));
    }
    let owner_id = owner.into_inner();
    let is_head: bool = sqlx::query_scalar(
        "SELECT EXISTS (
             SELECT 1
               FROM proxima_core.goal g
               JOIN proxima_core.goal_head h ON h.handle = g.handle AND h.t = g.t
              WHERE g.t = $1 AND g.owner_id = $2
         )",
    )
    .bind(goal_id.into_inner())
    .bind(owner_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(map_err)?;
    if is_head {
        Ok(())
    } else {
        Err(StorageError::Conflict(
            "parent_goal is not current head".into(),
        ))
    }
}

pub(super) fn validate_goal_transition(
    prior: GoalState,
    next: GoalState,
) -> Result<(), StorageError> {
    if prior.may_transition_to(next) {
        Ok(())
    } else {
        Err(StorageError::ConstraintViolation(format!(
            "invalid goal transition: {prior:?} -> {next:?}",
        )))
    }
}

/// The parent's own state. The child rule (#514: no open child, at least one
/// Achieved) is enforced after the lifecycle lock, through
/// `PostLockGuard::ChildrenClosed` in `insert_or_replay_goal`, so it sees a
/// child a racing decompose committed first.
pub(super) fn validate_goal_achievement(prior: GoalState) -> Result<(), StorageError> {
    if prior.may_achieve() {
        Ok(())
    } else {
        Err(StorageError::ConstraintViolation(format!(
            "invalid goal transition: {prior:?} -> {:?}",
            GoalState::Achieved,
        )))
    }
}

/// The evidence a successor Goal rests on, in the shape the topology takes.
///
/// The evidence column is the statement and the reference rows are its
/// consequence, so a successor that omits it from the column would leave rows
/// nothing could rebuild.
fn evidence_refs(evidence: &[EvidenceTarget]) -> Vec<proxima_core::GoalEvidenceRef> {
    evidence
        .iter()
        .map(|target| proxima_core::GoalEvidenceRef::new(target.memory_id))
        .collect()
}

pub(super) fn draft_from_stored(
    owner: &OwnerId,
    stored: &StoredGoal,
    state: GoalState,
    supersedes: Option<GoalId>,
    authorship: GoalAuthorship,
    request_id: &str,
    evidence: &[EvidenceTarget],
) -> GoalDraft {
    GoalDraft {
        owner: *owner,
        schema_id: stored.schema_id.clone(),
        schema_version: stored.schema_version,
        title: stored.title.clone(),
        // Not the row's text: a lifecycle write passes the stored text to the
        // insert as its `RowColumns`, and nothing reads this field for it (its
        // replay declaration is built from the request).
        text: String::new(),
        payload: stored.payload.clone(),
        sidecar_payload: None,
        state,
        topology: proxima_core::GoalTopologyWrite::new(
            stored
                .assignment
                .map(proxima_core::GoalAssignmentTarget::perspective),
            stored
                .dependencies
                .iter()
                .copied()
                .map(proxima_core::GoalDependencyRef::new)
                .collect(),
            evidence_refs(evidence),
        )
        .expect("stored topology has unique dependencies"),
        supersedes_goal_id: supersedes,
        authorship,
        request_id: request_id.to_string(),
    }
}

pub(super) struct DraftFromPayload<'a> {
    pub(super) owner: &'a OwnerId,
    pub(super) payload: &'a GoalPayloadWrite,
    pub(super) state: GoalState,
    pub(super) assignment: Option<MemoryId>,
    pub(super) dependencies: Vec<GoalId>,
    pub(super) supersedes: Option<GoalId>,
    pub(super) authorship: GoalAuthorship,
    pub(super) request_id: &'a str,
    pub(super) evidence: &'a [EvidenceTarget],
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
            input
                .assignment
                .map(proxima_core::GoalAssignmentTarget::perspective),
            input
                .dependencies
                .into_iter()
                .map(proxima_core::GoalDependencyRef::new)
                .collect(),
            evidence_refs(input.evidence),
        )
        .expect("stored topology has unique dependencies"),
        supersedes_goal_id: input.supersedes,
        authorship: input.authorship,
        request_id: input.request_id.to_string(),
    }
}

pub(super) fn child_draft(
    owner: &OwnerId,
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
        authorship.clone(),
        child.request_id.clone(),
    ))
}
