use super::{
    AchieveGoalAtomicRequest, ChildGoalDraft, CreateGoalAtomicRequest, DecomposeGoalAtomicRequest,
    DecomposeGoalOutcome, DecomposedGoalOutcome, GoalAuthorship, GoalDraft, GoalId,
    GoalPayloadWrite, GoalReplayOutcome, GoalReplayRequest, GoalWakeConfigWrite, GoalWriteOutcome,
    ModifyGoalAtomicRequest, Owner, PgPool, Postgres, StorageError, SystemOrigin, Transaction,
    TransitionGoalAtomicRequest, internal, map_err,
};

fn authorship_declaration(authorship: &GoalAuthorship) -> &'static str {
    // Replay identity records only the logical author category. The operator
    // fields and caller Self are host/session metadata, so including them
    // would turn a retry after a host rotation into an idempotency conflict.
    match authorship {
        GoalAuthorship::User => "user",
        GoalAuthorship::System(SystemOrigin::Operator { .. }) => "system_operator",
        GoalAuthorship::System(SystemOrigin::Tool { .. }) => "system_tool",
        GoalAuthorship::External => "external",
    }
}

fn sidecar_declaration(
    sidecar: Option<&proxima_core::SidecarPayload>,
) -> Result<serde_json::Value, StorageError> {
    let Some(sidecar) = sidecar else {
        return Ok(serde_json::Value::Null);
    };
    let payload = sidecar.to_protocol_json().map_err(internal)?;
    Ok(serde_json::json!({
        "kind": sidecar.kind,
        "schema_id": sidecar.schema_id.as_str(),
        "schema_version": sidecar.schema_version.into_inner(),
        "payload": payload,
    }))
}

fn payload_declaration(payload: &GoalPayloadWrite) -> Result<serde_json::Value, StorageError> {
    Ok(serde_json::json!({
        "schema_id": payload.schema_id.as_str(),
        "schema_version": payload.schema_version.into_inner(),
        "title": payload.title,
        "text": payload.text,
        "payload": payload.payload,
        "sidecar": sidecar_declaration(payload.sidecar_payload.as_ref())?,
    }))
}

fn draft_declaration(draft: &GoalDraft) -> Result<serde_json::Value, StorageError> {
    Ok(serde_json::json!({
        "schema_id": draft.schema_id.as_str(),
        "schema_version": draft.schema_version.into_inner(),
        "title": draft.title,
        "text": draft.text,
        "payload": draft.payload,
        "sidecar": sidecar_declaration(draft.sidecar_payload.as_ref())?,
        "state": draft.state,
        "topology": draft.topology,
        "wake": draft.wake,
        "supersedes_goal_id": draft.supersedes_goal_id.map(GoalId::into_inner),
        "authorship": authorship_declaration(&draft.authorship),
    }))
}

pub(super) fn create_replay_declaration(
    req: &CreateGoalAtomicRequest<'_>,
) -> Result<serde_json::Value, StorageError> {
    // `write_act_t` is an episode binding attempt, not Goal content. A replay
    // returns the stored row without rewriting it so the episode layer can
    // reject a bound replay with its protocol-specific error.
    Ok(serde_json::json!({
        "version": 1,
        "verb": "create",
        "draft": draft_declaration(&req.draft)?,
    }))
}

pub(super) fn transition_replay_declaration(
    req: &TransitionGoalAtomicRequest<'_>,
) -> serde_json::Value {
    serde_json::json!({
        "version": 1,
        "verb": "transition",
        "prior_goal_id": req.prior_goal_id.into_inner(),
        "next_state": req.next_state,
        "authorship": authorship_declaration(&req.authorship),
    })
}

pub(super) fn achieve_replay_declaration(req: &AchieveGoalAtomicRequest<'_>) -> serde_json::Value {
    serde_json::json!({
        "version": 1,
        "verb": "achieve",
        "prior_goal_id": req.prior_goal_id.into_inner(),
        "authorship": authorship_declaration(&req.authorship),
        "evidence": req.evidence,
    })
}

pub(super) fn modify_replay_declaration(
    req: &ModifyGoalAtomicRequest<'_>,
) -> Result<serde_json::Value, StorageError> {
    Ok(serde_json::json!({
        "version": 1,
        "verb": "modify",
        "prior_goal_id": req.prior_goal_id.into_inner(),
        "replacement": payload_declaration(&req.replacement)?,
        "wake": modify_wake_declaration(req.wake.as_ref()),
        "authorship": authorship_declaration(&req.authorship),
        "evidence": req.evidence,
    }))
}

fn modify_wake_declaration(wake: Option<&Option<GoalWakeConfigWrite>>) -> serde_json::Value {
    match wake {
        None => serde_json::json!({"mode": "carry"}),
        Some(None) => serde_json::json!({"mode": "clear"}),
        Some(Some(config)) => serde_json::json!({"mode": "replace", "config": config}),
    }
}

type ReplayProbeRow = (i64, Option<uuid::Uuid>, Option<i32>, Option<bool>);

pub(super) fn decompose_child_replay_declaration(
    req: &DecomposeGoalAtomicRequest<'_>,
    child: &ChildGoalDraft,
    child_index: usize,
) -> Result<serde_json::Value, StorageError> {
    Ok(serde_json::json!({
        "version": 1,
        "verb": "decompose_child",
        "parent_goal_id": req.parent_goal_id.into_inner(),
        "authorship": authorship_declaration(&req.authorship),
        "topology": req.topology,
        "child_index": child_index,
        "payload": payload_declaration(&child.payload)?,
        "evidence": child.evidence,
        "wake": child.wake,
    }))
}

pub(super) fn decompose_replay_declarations(
    req: &DecomposeGoalAtomicRequest<'_>,
) -> Result<Vec<serde_json::Value>, StorageError> {
    req.children
        .iter()
        .enumerate()
        .map(|(index, child)| decompose_child_replay_declaration(req, child, index))
        .collect()
}

pub(super) async fn resolve_decompose_replay_set(
    tx: &mut Transaction<'_, Postgres>,
    req: &DecomposeGoalAtomicRequest<'_>,
    declarations: &[serde_json::Value],
) -> Result<Option<DecomposeGoalOutcome>, StorageError> {
    let request_ids = req
        .children
        .iter()
        .map(|child| child.request_id.as_str().to_owned())
        .collect::<Vec<_>>();
    let declaration_json = declarations
        .iter()
        .map(serde_json::Value::to_string)
        .collect::<Vec<_>>();
    let rows: Vec<ReplayProbeRow> = sqlx::query_as(
        "SELECT r.ordinality::bigint, g.t, d.edge_count,
                d.declaration = r.declaration::jsonb
           FROM unnest($2::text[], $3::text[]) WITH ORDINALITY
                  AS r(request_id, declaration, ordinality)
           LEFT JOIN proxima_core.goal g
             ON g.owner_id = $1 AND g.request_id = r.request_id
           LEFT JOIN proxima_core.goal_replay_declaration d
             ON d.goal_t = g.t
          ORDER BY r.ordinality",
    )
    .bind(req.owner.stored_owner_id())
    .bind(&request_ids)
    .bind(&declaration_json)
    .fetch_all(tx.as_mut())
    .await
    .map_err(map_err)?;
    if rows.len() != req.children.len() {
        return Err(internal(
            "Goal decomposition replay probe returned the wrong row count",
        ));
    }
    let mut replayed = Vec::with_capacity(rows.len());
    for (child, (_, goal_t, edge_count, matches)) in req.children.iter().zip(rows) {
        let Some(goal_t) = goal_t else {
            replayed.push(None);
            continue;
        };
        if matches != Some(true) {
            return Err(idempotency_conflict(child.request_id.as_str()));
        }
        replayed.push(Some(replay_outcome(goal_t, edge_count)?));
    }
    let replay_count = replayed.iter().filter(|outcome| outcome.is_some()).count();
    if replay_count == 0 {
        return Ok(None);
    }
    if replay_count != req.children.len() {
        let reused = req
            .children
            .iter()
            .zip(&replayed)
            .find_map(|(child, outcome)| outcome.as_ref().map(|_| child.request_id.as_str()))
            .expect("a partial replay contains one reused request id");
        return Err(idempotency_conflict(reused));
    }
    let children = replayed
        .into_iter()
        .map(|outcome| DecomposedGoalOutcome {
            outcome: outcome.expect("every child replay was resolved"),
        })
        .collect();
    Ok(Some(DecomposeGoalOutcome {
        children,
        idempotent_replay: true,
    }))
}

/// Pool-scoped public-boundary replay probe. It is read-only, but uses one
/// transaction so a decomposed child set is observed from one snapshot.
pub(crate) async fn resolve_goal_command_replay(
    pool: &PgPool,
    req: GoalReplayRequest<'_, '_>,
) -> Result<Option<GoalReplayOutcome>, StorageError> {
    let mut tx = pool.begin().await.map_err(internal)?;
    let outcome = resolve_goal_command_replay_in_tx(&mut tx, req).await?;
    tx.commit().await.map_err(map_err)?;
    Ok(outcome)
}

pub(crate) async fn resolve_goal_command_replay_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    req: GoalReplayRequest<'_, '_>,
) -> Result<Option<GoalReplayOutcome>, StorageError> {
    match req {
        GoalReplayRequest::Create(req) => {
            let declaration = create_replay_declaration(req)?;
            resolve_goal_replay(tx, &req.draft.owner(), &req.draft.request_id, &declaration)
                .await
                .map(|outcome| outcome.map(GoalReplayOutcome::Goal))
        }
        GoalReplayRequest::Transition(req) => {
            let declaration = transition_replay_declaration(req);
            resolve_goal_replay(tx, &req.owner, req.request_id.as_str(), &declaration)
                .await
                .map(|outcome| outcome.map(GoalReplayOutcome::Goal))
        }
        GoalReplayRequest::Achieve(req) => {
            let declaration = achieve_replay_declaration(req);
            resolve_goal_replay(tx, &req.owner, req.request_id.as_str(), &declaration)
                .await
                .map(|outcome| outcome.map(GoalReplayOutcome::Goal))
        }
        GoalReplayRequest::Modify(req) => {
            let declaration = modify_replay_declaration(req)?;
            resolve_goal_replay(tx, &req.owner, req.request_id.as_str(), &declaration)
                .await
                .map(|outcome| outcome.map(GoalReplayOutcome::Goal))
        }
        GoalReplayRequest::Decompose(req) => {
            let declarations = decompose_replay_declarations(req)?;
            resolve_decompose_replay_set(tx, req, &declarations)
                .await
                .map(|outcome| outcome.map(GoalReplayOutcome::Decompose))
        }
    }
}

/// Resolve one exact command replay from its persisted declaration before any
/// live assignment, evidence, prior-head, or wake probe.
pub(super) async fn resolve_goal_replay(
    tx: &mut Transaction<'_, Postgres>,
    owner: &Owner,
    request_id: &str,
    declaration: &serde_json::Value,
) -> Result<Option<GoalWriteOutcome>, StorageError> {
    let row: Option<(uuid::Uuid, Option<i32>, Option<bool>)> = sqlx::query_as(
        "SELECT g.t, d.edge_count, d.declaration = $3::jsonb
           FROM proxima_core.goal g
           LEFT JOIN proxima_core.goal_replay_declaration d
             ON d.goal_t = g.t
          WHERE g.owner_id = $1 AND g.request_id = $2",
    )
    .bind(owner.stored_owner_id())
    .bind(request_id)
    .bind(declaration)
    .fetch_optional(tx.as_mut())
    .await
    .map_err(map_err)?;
    let Some((goal_t, edge_count, matches)) = row else {
        return Ok(None);
    };
    if matches != Some(true) {
        return Err(idempotency_conflict(request_id));
    }
    replay_outcome(goal_t, edge_count).map(Some)
}

fn replay_outcome(
    goal_t: uuid::Uuid,
    edge_count: Option<i32>,
) -> Result<GoalWriteOutcome, StorageError> {
    let edge_count = edge_count
        .and_then(|count| usize::try_from(count).ok())
        .ok_or_else(|| internal("Goal replay declaration has invalid edge_count"))?;
    Ok(GoalWriteOutcome {
        goal_id: GoalId::new(goal_t),
        change_event_seq: goal_t,
        lifecycle_memory_id: None,
        edge_count,
        idempotent_replay: true,
    })
}

pub(super) async fn require_goal_replay(
    tx: &mut Transaction<'_, Postgres>,
    owner: &Owner,
    request_id: &str,
    declaration: &serde_json::Value,
) -> Result<GoalWriteOutcome, StorageError> {
    resolve_goal_replay(tx, owner, request_id, declaration)
        .await?
        .ok_or_else(|| internal("Goal replay row disappeared after request-id match"))
}

pub(super) async fn record_goal_replay_declaration(
    tx: &mut Transaction<'_, Postgres>,
    declaration: &serde_json::Value,
    outcome: &GoalWriteOutcome,
) -> Result<(), StorageError> {
    let edge_count = i32::try_from(outcome.edge_count)
        .map_err(|_| internal("Goal replay edge_count exceeds PostgreSQL integer"))?;
    sqlx::query(
        "INSERT INTO proxima_core.goal_replay_declaration
             (goal_t, declaration, edge_count)
         VALUES ($1, $2, $3)",
    )
    .bind(outcome.goal_id.into_inner())
    .bind(declaration)
    .bind(edge_count)
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;
    Ok(())
}

pub(super) fn idempotency_conflict(request_id: &str) -> StorageError {
    // The `Display` form is load-bearing: storage-level callers match on the
    // message `idempotency_conflict:{request_id}`. The engine matches the variant.
    StorageError::IdempotencyConflict {
        request_id: request_id.to_string(),
    }
}
