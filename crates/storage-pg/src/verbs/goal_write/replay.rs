use super::{
    AuthorshipColumns, AuthorshipRow, CORE_AUTHORED_RELATION, CORE_MOTIVATED_BY_RELATION,
    EdgeAuthorshipKind, EntityKind, EvidenceTarget, GoalAuthorship, GoalBodyRow, GoalDraft, GoalId,
    GoalLifecycleFact, HashSet, MemoryId, Postgres, StorageError, Transaction, WakeWrite,
    authorship_columns, dependency_goal_ids, goal_wake_matches, lifecycle_memory_for_goal, map_err,
};

pub(super) struct CreateGoalReplayExpectation<'a> {
    pub(super) goal_id: GoalId,
    pub(super) target_self_perspective_id: MemoryId,
    pub(super) evidence: &'a [EvidenceTarget],
    pub(super) evidence_authorship_kind: EdgeAuthorshipKind,
    pub(super) author_self_perspective_id: Option<MemoryId>,
    pub(super) wake_write: WakeWrite<'a>,
    pub(super) expected_prior: Option<GoalId>,
    pub(super) request_id: &'a str,
}

pub(super) async fn ensure_create_goal_replay_side_effects_match(
    tx: &mut Transaction<'_, Postgres>,
    expected: CreateGoalReplayExpectation<'_>,
) -> Result<(), StorageError> {
    if !goal_self_assignment_matches(tx, expected.goal_id, expected.target_self_perspective_id)
        .await?
    {
        return Err(idempotency_conflict(expected.request_id));
    }
    if !goal_evidence_edges_match(
        tx,
        expected.goal_id,
        expected.evidence,
        expected.evidence_authorship_kind,
    )
    .await?
    {
        return Err(idempotency_conflict(expected.request_id));
    }
    let Some(lifecycle_memory_id) =
        lifecycle_memory_for_goal(tx, expected.goal_id, GoalLifecycleFact::Activated).await?
    else {
        return Err(idempotency_conflict(expected.request_id));
    };
    if !lifecycle_author_edge_matches(tx, lifecycle_memory_id, expected.author_self_perspective_id)
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
    let rows: Vec<(Option<uuid::Uuid>,)> = sqlx::query_as(
        "SELECT target_memory_id
           FROM proxima_core.edges
          WHERE source_goal_id = $1
            AND relation = $2
          ORDER BY edge_id ASC",
    )
    .bind(goal_id.into_inner())
    .bind(proxima_core::relation::CORE_INSPIRES_RELATION)
    .fetch_all(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(rows.len() == 1 && rows[0].0 == Some(target_self_perspective_id.into_inner()))
}

pub(super) async fn goal_evidence_edges_match(
    tx: &mut Transaction<'_, Postgres>,
    goal_id: GoalId,
    evidence: &[EvidenceTarget],
    authorship_kind: EdgeAuthorshipKind,
) -> Result<bool, StorageError> {
    type EvidenceEdgeTuple = (
        String,
        String,
        Option<uuid::Uuid>,
        EntityKind,
        Option<uuid::Uuid>,
        EdgeAuthorshipKind,
    );

    let rows: Vec<EvidenceEdgeTuple> = sqlx::query_as(
        "SELECT relation, relation_class::text, source_goal_id, target_kind,
                target_memory_id, authorship_kind
           FROM proxima_core.edges
          WHERE source_goal_id = $1
            AND relation = $2
          ORDER BY edge_id ASC",
    )
    .bind(goal_id.into_inner())
    .bind(CORE_MOTIVATED_BY_RELATION)
    .fetch_all(&mut **tx)
    .await
    .map_err(map_err)?;
    let stored = rows.into_iter().collect::<HashSet<_>>();
    let requested = evidence
        .iter()
        .map(|target| {
            (
                CORE_MOTIVATED_BY_RELATION.to_string(),
                "Structural".to_string(),
                Some(goal_id.into_inner()),
                target.kind,
                Some(target.memory_id.into_inner()),
                authorship_kind,
            )
        })
        .collect::<HashSet<_>>();
    Ok(stored == requested)
}

async fn lifecycle_author_edge_matches(
    tx: &mut Transaction<'_, Postgres>,
    lifecycle_memory_id: MemoryId,
    author_self_perspective_id: Option<MemoryId>,
) -> Result<bool, StorageError> {
    let rows: Vec<(Option<uuid::Uuid>,)> = sqlx::query_as(
        "SELECT source_memory_id
           FROM proxima_core.edges
          WHERE target_memory_id = $1
            AND relation = $2
          ORDER BY edge_id ASC",
    )
    .bind(lifecycle_memory_id.into_inner())
    .bind(CORE_AUTHORED_RELATION)
    .fetch_all(&mut **tx)
    .await
    .map_err(map_err)?;
    let expected = author_self_perspective_id.map(MemoryId::into_inner);
    match expected {
        Some(expected) => Ok(rows.len() == 1 && rows[0].0 == Some(expected)),
        None => Ok(rows.is_empty()),
    }
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
                state, supersedes
           FROM proxima_core.goals
          WHERE goal_id = $1",
    )
    .bind(existing_goal_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(map_err)?;
    let dependencies = dependency_goal_ids(tx, GoalId::new(existing_goal_id)).await?;
    let existing_dependencies: HashSet<GoalId> = dependencies.into_iter().collect();
    let draft_dependencies: HashSet<GoalId> = draft
        .topology
        .dependencies()
        .iter()
        .map(|dependency| dependency.goal_id())
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
