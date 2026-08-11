use super::{
    AuthorshipColumns, AuthorshipRow, EvidenceTarget, GoalAuthorship, GoalBodyRow, GoalDraft,
    GoalId, GoalLifecycleFact, HashSet, MemoryId, Postgres, StorageError, Transaction, WakeWrite,
    authorship_columns, goal_wake_matches, lifecycle_memory_for_goal, map_err,
};

pub(super) struct CreateGoalReplayExpectation<'a> {
    pub(super) goal_id: GoalId,
    pub(super) target_self_perspective_id: MemoryId,
    pub(super) author_self_perspective_id: Option<MemoryId>,
    pub(super) wake_write: WakeWrite<'a>,
    pub(super) expected_prior: Option<GoalId>,
    pub(super) request_id: &'a str,
}

/// The create-only half of replay verification: the rows a create writes that
/// no other Goal verb does. Evidence equality is checked for every verb by
/// the shared lifecycle tail, so it is deliberately not restated here.
pub(super) async fn ensure_create_goal_replay_side_effects_match(
    tx: &mut Transaction<'_, Postgres>,
    expected: CreateGoalReplayExpectation<'_>,
) -> Result<(), StorageError> {
    if !goal_self_assignment_matches(tx, expected.goal_id, expected.target_self_perspective_id)
        .await?
    {
        return Err(idempotency_conflict(expected.request_id));
    }
    let Some(lifecycle_memory_id) =
        lifecycle_memory_for_goal(tx, expected.goal_id, GoalLifecycleFact::Activated).await?
    else {
        return Err(idempotency_conflict(expected.request_id));
    };
    if !lifecycle_author_matches(tx, lifecycle_memory_id, expected.author_self_perspective_id)
        .await?
    {
        return Err(idempotency_conflict(expected.request_id));
    }
    if !goal_wake_matches(
        tx,
        expected.goal_id,
        expected.wake_write,
        expected.expected_prior,
    )
    .await?
    {
        return Err(idempotency_conflict(expected.request_id));
    }
    Ok(())
}

async fn goal_self_assignment_matches(
    tx: &mut Transaction<'_, Postgres>,
    goal_id: GoalId,
    target_self_perspective_id: MemoryId,
) -> Result<bool, StorageError> {
    let stored: Option<Option<uuid::Uuid>> = sqlx::query_scalar(
        "SELECT assignment_perspective_id FROM proxima_core.goals WHERE goal_id = $1",
    )
    .bind(goal_id.into_inner())
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(stored.flatten() == Some(target_self_perspective_id.into_inner()))
}

/// Evidence equality on the Goal's own column, not on the index. The column
/// is the statement; the index rows are its consequence, so comparing the
/// column is comparing what the write actually claimed.
pub(super) async fn goal_evidence_matches(
    tx: &mut Transaction<'_, Postgres>,
    goal_id: GoalId,
    evidence: &[EvidenceTarget],
) -> Result<bool, StorageError> {
    let stored: Option<Vec<uuid::Uuid>> =
        sqlx::query_scalar("SELECT evidence_memory_ids FROM proxima_core.goals WHERE goal_id = $1")
            .bind(goal_id.into_inner())
            .fetch_optional(&mut **tx)
            .await
            .map_err(map_err)?;
    let stored = stored
        .unwrap_or_default()
        .into_iter()
        .collect::<HashSet<_>>();
    let requested = evidence
        .iter()
        .map(|target| target.memory_id.into_inner())
        .collect::<HashSet<_>>();
    Ok(stored == requested)
}

/// Authorship of the lifecycle Fact is a column on the Fact, so the replay
/// check reads the column that the write stamped.
async fn lifecycle_author_matches(
    tx: &mut Transaction<'_, Postgres>,
    lifecycle_memory_id: MemoryId,
    author_self_perspective_id: Option<MemoryId>,
) -> Result<bool, StorageError> {
    let stored: Option<Option<uuid::Uuid>> = sqlx::query_scalar(
        "SELECT authoring_perspective_id FROM proxima_core.memories WHERE memory_id = $1",
    )
    .bind(lifecycle_memory_id.into_inner())
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(stored.flatten() == author_self_perspective_id.map(MemoryId::into_inner))
}

pub(super) fn idempotency_conflict(request_id: &str) -> StorageError {
    // Typed variant (was a stringly `ConstraintViolation`). Its `Display`
    // stays `idempotency_conflict:{request_id}` so storage-level callers that
    // match on the message keep working; the engine matches the variant.
    StorageError::IdempotencyConflict {
        request_id: request_id.to_string(),
    }
}

pub(super) async fn existing_goal_body_matches(
    tx: &mut Transaction<'_, Postgres>,
    existing_goal_id: uuid::Uuid,
    draft: &GoalDraft,
    expected_prior: Option<GoalId>,
    wake_write: WakeWrite<'_>,
) -> Result<bool, StorageError> {
    let row: GoalBodyRow = sqlx::query_as(
        "SELECT schema_id, schema_version, title, text, payload,
                state, supersedes, dependency_goal_ids
           FROM proxima_core.goals
          WHERE goal_id = $1",
    )
    .bind(existing_goal_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(map_err)?;
    let existing_dependencies: HashSet<uuid::Uuid> = row.dependency_goal_ids.into_iter().collect();
    let draft_dependencies: HashSet<uuid::Uuid> = draft
        .topology
        .dependencies()
        .iter()
        .map(|dependency| dependency.goal_id().into_inner())
        .collect();
    Ok(row.schema_id == draft.schema_id.as_str()
        && row.schema_version == draft.schema_version.into_inner().cast_signed()
        && row.title == draft.title
        && row.text == draft.text
        && row.payload == draft.payload
        && row.state == draft.state
        && row.supersedes == expected_prior.map(GoalId::into_inner)
        && existing_dependencies == draft_dependencies
        && goal_wake_matches(
            tx,
            GoalId::new(existing_goal_id),
            wake_write,
            expected_prior,
        )
        .await?)
}

pub(super) async fn authorship_matches(
    tx: &mut Transaction<'_, Postgres>,
    existing_goal_id: uuid::Uuid,
    authorship: &GoalAuthorship,
) -> Result<bool, StorageError> {
    let row: AuthorshipRow = sqlx::query_as(
        "SELECT authorship_kind, authorship_origin, authorship_operator_id,
                authorship_tool_id, operator_kind, input_contract_id, model_id, prompt_version
           FROM proxima_core.goals
          WHERE goal_id = $1",
    )
    .bind(existing_goal_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(map_err)?;
    let existing = AuthorshipColumns {
        authorship_kind: row.authorship_kind,
        authorship_origin: row.authorship_origin,
        authorship_operator_id: row.authorship_operator_id,
        authorship_tool_id: row.authorship_tool_id,
        operator_kind: row.operator_kind,
        input_contract_id: row.input_contract_id,
        model_id: row.model_id,
        prompt_version: row.prompt_version,
    };
    Ok(existing == authorship_columns(authorship))
}
