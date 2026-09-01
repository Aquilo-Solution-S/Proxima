use super::{
    AchieveGoalAtomicRequest, ChildGoalDraft, CreateGoalAtomicRequest, DecomposeGoalAtomicRequest,
    DecomposeGoalOutcome, DecomposedGoalOutcome, GoalAuthorship, GoalDraft, GoalId,
    GoalPayloadWrite, GoalReplayOutcome, GoalReplayRequest, GoalWakeConfigWrite, GoalWriteOutcome,
    ModifyGoalAtomicRequest, Owner, PgConnection, PgPool, StorageError, SystemOrigin,
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

/// Every Goal command declaration carries the same envelope: the format
/// version and the verb that authored it. Only the body differs, so a sixth
/// verb cannot forget a field of the envelope.
fn command_declaration<const N: usize>(
    verb: &'static str,
    body: [(&'static str, serde_json::Value); N],
) -> serde_json::Value {
    let mut declaration = serde_json::Map::with_capacity(N + 2);
    declaration.insert("version".to_owned(), serde_json::json!(1));
    declaration.insert("verb".to_owned(), serde_json::json!(verb));
    declaration.extend(body.into_iter().map(|(key, value)| (key.to_owned(), value)));
    serde_json::Value::Object(declaration)
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
    Ok(command_declaration(
        "create",
        [("draft", draft_declaration(&req.draft)?)],
    ))
}

pub(super) fn transition_replay_declaration(
    req: &TransitionGoalAtomicRequest<'_>,
) -> serde_json::Value {
    let prior = req.prior_goal_id.into_inner();
    command_declaration(
        "transition",
        [
            ("prior_goal_id", serde_json::json!(prior)),
            ("next_state", serde_json::json!(req.next_state)),
            (
                "authorship",
                serde_json::json!(authorship_declaration(&req.authorship)),
            ),
        ],
    )
}

pub(super) fn achieve_replay_declaration(req: &AchieveGoalAtomicRequest<'_>) -> serde_json::Value {
    let prior = req.prior_goal_id.into_inner();
    command_declaration(
        "achieve",
        [
            ("prior_goal_id", serde_json::json!(prior)),
            (
                "authorship",
                serde_json::json!(authorship_declaration(&req.authorship)),
            ),
            ("evidence", serde_json::json!(req.evidence)),
        ],
    )
}

pub(super) fn modify_replay_declaration(
    req: &ModifyGoalAtomicRequest<'_>,
) -> Result<serde_json::Value, StorageError> {
    let prior = req.prior_goal_id.into_inner();
    Ok(command_declaration(
        "modify",
        [
            ("prior_goal_id", serde_json::json!(prior)),
            ("replacement", payload_declaration(&req.replacement)?),
            ("wake", modify_wake_declaration(req.wake.as_ref())),
            (
                "authorship",
                serde_json::json!(authorship_declaration(&req.authorship)),
            ),
            ("evidence", serde_json::json!(req.evidence)),
        ],
    ))
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
    let parent = req.parent_goal_id.into_inner();
    Ok(command_declaration(
        "decompose_child",
        [
            ("parent_goal_id", serde_json::json!(parent)),
            (
                "authorship",
                serde_json::json!(authorship_declaration(&req.authorship)),
            ),
            ("topology", serde_json::json!(req.topology)),
            ("child_index", serde_json::json!(child_index)),
            ("payload", payload_declaration(&child.payload)?),
            ("evidence", serde_json::json!(child.evidence)),
            ("wake", serde_json::json!(child.wake)),
        ],
    ))
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
    conn: &mut PgConnection,
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
    .fetch_all(&mut *conn)
    .await
    .map_err(map_err)?;
    if rows.len() != req.children.len() {
        return Err(internal(
            "Goal decomposition replay probe returned the wrong row count",
        ));
    }
    let mut replayed = Vec::with_capacity(rows.len());
    let mut reused: Option<&str> = None;
    for (child, (_, goal_t, edge_count, matches)) in req.children.iter().zip(rows) {
        let Some(goal_t) = goal_t else {
            replayed.push(None);
            continue;
        };
        if matches != Some(true) {
            return Err(idempotency_conflict(child.request_id.as_str()));
        }
        reused.get_or_insert(child.request_id.as_str());
        replayed.push(Some(replay_outcome(goal_t, edge_count)?));
    }
    // Nothing was reused, so the whole set is fresh.
    let Some(reused) = reused else {
        return Ok(None);
    };
    // A decomposition is one idempotency unit. A request-id set that is only
    // partly present resolves as neither a fresh write nor an exact replay.
    let mut children = Vec::with_capacity(replayed.len());
    for outcome in replayed {
        let Some(outcome) = outcome else {
            return Err(idempotency_conflict(reused));
        };
        children.push(DecomposedGoalOutcome { outcome });
    }
    Ok(Some(DecomposeGoalOutcome {
        children,
        idempotent_replay: true,
    }))
}

/// Pool-scoped public-boundary replay probe. Every arm below is exactly one
/// statement, and one statement already observes one snapshot — including the
/// decomposed child set — so this needs a connection, not a transaction.
pub(crate) async fn resolve_goal_command_replay(
    pool: &PgPool,
    req: GoalReplayRequest<'_, '_>,
) -> Result<Option<GoalReplayOutcome>, StorageError> {
    let mut conn = pool.acquire().await.map_err(internal)?;
    resolve_goal_command_replay_on(&mut conn, req).await
}

pub(crate) async fn resolve_goal_command_replay_on(
    conn: &mut PgConnection,
    req: GoalReplayRequest<'_, '_>,
) -> Result<Option<GoalReplayOutcome>, StorageError> {
    match req {
        GoalReplayRequest::Create(req) => {
            let declaration = create_replay_declaration(req)?;
            resolve_goal_replay(
                conn,
                &req.draft.owner(),
                &req.draft.request_id,
                &declaration,
            )
            .await
            .map(|outcome| outcome.map(GoalReplayOutcome::Goal))
        }
        GoalReplayRequest::Transition(req) => {
            let declaration = transition_replay_declaration(req);
            resolve_goal_replay(conn, &req.owner, req.request_id.as_str(), &declaration)
                .await
                .map(|outcome| outcome.map(GoalReplayOutcome::Goal))
        }
        GoalReplayRequest::Achieve(req) => {
            let declaration = achieve_replay_declaration(req);
            resolve_goal_replay(conn, &req.owner, req.request_id.as_str(), &declaration)
                .await
                .map(|outcome| outcome.map(GoalReplayOutcome::Goal))
        }
        GoalReplayRequest::Modify(req) => {
            let declaration = modify_replay_declaration(req)?;
            resolve_goal_replay(conn, &req.owner, req.request_id.as_str(), &declaration)
                .await
                .map(|outcome| outcome.map(GoalReplayOutcome::Goal))
        }
        GoalReplayRequest::Decompose(req) => {
            let declarations = decompose_replay_declarations(req)?;
            resolve_decompose_replay_set(conn, req, &declarations)
                .await
                .map(|outcome| outcome.map(GoalReplayOutcome::Decompose))
        }
    }
}

/// Resolve one exact command replay from its persisted declaration before any
/// live assignment, evidence, prior-head, or wake probe.
pub(super) async fn resolve_goal_replay(
    conn: &mut PgConnection,
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
    .fetch_optional(&mut *conn)
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
    conn: &mut PgConnection,
    owner: &Owner,
    request_id: &str,
    declaration: &serde_json::Value,
) -> Result<GoalWriteOutcome, StorageError> {
    resolve_goal_replay(conn, owner, request_id, declaration)
        .await?
        .ok_or_else(|| internal("Goal replay row disappeared after request-id match"))
}

pub(super) async fn record_goal_replay_declaration(
    conn: &mut PgConnection,
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
    .execute(&mut *conn)
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
