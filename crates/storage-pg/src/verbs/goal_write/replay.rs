use super::{
    AchieveGoalAtomicRequest, ChildGoalDraft, CreateGoalAtomicRequest, DecomposeGoalAtomicRequest,
    DecomposeGoalOutcome, DecomposedGoalOutcome, GoalAuthorship, GoalDraft, GoalId,
    GoalPayloadWrite, GoalReplayOutcome, GoalReplayRequest, GoalWriteOutcome,
    ModifyGoalAtomicRequest, OwnerId, PgConnection, StorageError, SystemOrigin,
    TransitionGoalAtomicRequest, internal, map_err,
};
use proxiima_core::verbs::goal_write::GoalTransitionKind;
use proxiima_core::verbs::idempotency::{IdempotencyKey, KeyVerdict, judge};
use serde::{Deserialize, Serialize, Serializer};
use serde_json::Value;

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

#[derive(Debug, Serialize)]
struct ReplaySidecar {
    kind: ReplayPayloadKind,
    schema_id: String,
    schema_version: u32,
    payload: proxiima_core::DurablePayload,
}

#[derive(Debug, Serialize)]
struct ReplayPayload {
    schema_id: String,
    schema_version: u32,
    title: String,
    text: String,
    payload: Vec<u8>,
    sidecar: Option<ReplaySidecar>,
}

#[derive(Debug, Serialize)]
struct CreateBody {
    draft: ReplayDraft,
}

#[derive(Debug, Serialize)]
struct ReplayDraft {
    schema_id: String,
    schema_version: u32,
    title: String,
    text: String,
    payload: Vec<u8>,
    sidecar: Option<ReplaySidecar>,
    state: ReplayGoalState,
    topology: ReplayTopology,
    supersedes_goal_id: Option<uuid::Uuid>,
    authorship: &'static str,
}

#[derive(Debug, Serialize)]
struct TransitionBody {
    prior_goal_id: uuid::Uuid,
    next_state: ReplayGoalState,
    authorship: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pending: Option<ReplayPendingWork>,
}

#[derive(Debug, Serialize)]
struct PriorAuthorshipBody {
    prior_goal_id: uuid::Uuid,
    authorship: &'static str,
    evidence: Option<Vec<ReplayEvidenceRef>>,
}

#[derive(Debug, Serialize)]
struct ModifyBody {
    prior_goal_id: uuid::Uuid,
    replacement: ReplayPayload,
    authorship: &'static str,
    evidence: Option<Vec<ReplayEvidenceRef>>,
}

#[derive(Debug, Serialize)]
struct DecomposeChildBody {
    parent_goal_id: uuid::Uuid,
    authorship: &'static str,
    topology: ReplayTopology,
    child_index: usize,
    payload: ReplayPayload,
    evidence: Vec<ReplayEvidenceRef>,
}

#[derive(Debug, Serialize)]
struct ReplayPayloadKind(&'static str);

impl From<proxiima_core::verbs::schema::PayloadKind> for ReplayPayloadKind {
    fn from(kind: proxiima_core::verbs::schema::PayloadKind) -> Self {
        use proxiima_core::verbs::schema::PayloadKind;
        Self(match kind {
            PayloadKind::Fact => "Fact",
            PayloadKind::Abstraction => "Abstraction",
            PayloadKind::Perspective => "Perspective",
            PayloadKind::Goal => "Goal",
            PayloadKind::CitedObject => "CitedObject",
        })
    }
}

#[derive(Debug)]
struct ReplayGoalState(proxiima_core::verbs::goal_write::GoalState);

impl Serialize for ReplayGoalState {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        use proxiima_core::verbs::goal_write::GoalState;
        serializer.serialize_str(match self.0 {
            GoalState::Active => "Active",
            GoalState::Paused => "Paused",
            GoalState::Achieved => "Achieved",
            GoalState::Abandoned => "Abandoned",
        })
    }
}

#[derive(Debug)]
struct ReplayPendingWork(proxiima_core::PendingWork);

impl Serialize for ReplayPendingWork {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(match self.0 {
            proxiima_core::PendingWork::Keep => "keep",
            proxiima_core::PendingWork::Discard => "discard",
        })
    }
}

#[derive(Debug, Serialize)]
struct ReplayMotivationRef {
    perspective_id: uuid::Uuid,
}

#[derive(Debug, Serialize)]
struct ReplayDependencyRef {
    goal_id: uuid::Uuid,
}

#[derive(Debug, Serialize)]
struct ReplayEvidenceRef {
    memory_id: uuid::Uuid,
}

#[derive(Debug, Serialize)]
struct ReplayTopology {
    motivated_by: Vec<ReplayMotivationRef>,
    dependencies: Vec<ReplayDependencyRef>,
    evidence: Vec<ReplayEvidenceRef>,
}

impl From<&proxiima_core::GoalTopologyWrite> for ReplayTopology {
    fn from(topology: &proxiima_core::GoalTopologyWrite) -> Self {
        Self {
            motivated_by: topology
                .motivated_by()
                .iter()
                .map(|reference| ReplayMotivationRef {
                    perspective_id: reference.perspective_id().into_inner(),
                })
                .collect(),
            dependencies: topology
                .dependencies()
                .iter()
                .map(|reference| ReplayDependencyRef {
                    goal_id: reference.goal_id().into_inner(),
                })
                .collect(),
            evidence: topology
                .evidence()
                .iter()
                .map(|reference| ReplayEvidenceRef {
                    memory_id: reference.memory_id().into_inner(),
                })
                .collect(),
        }
    }
}

impl From<proxiima_core::GoalEvidenceRef> for ReplayEvidenceRef {
    fn from(reference: proxiima_core::GoalEvidenceRef) -> Self {
        Self {
            memory_id: reference.memory_id().into_inner(),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(tag = "verb")]
enum ReplayCommand {
    #[serde(rename = "create")]
    Create(CreateBody),
    #[serde(rename = "transition")]
    Transition(TransitionBody),
    #[serde(rename = "achieve")]
    Achieve(PriorAuthorshipBody),
    #[serde(rename = "modify")]
    Modify(ModifyBody),
    #[serde(rename = "decompose_child")]
    DecomposeChild(DecomposeChildBody),
}

/// Every Goal command declaration carries the same format version. The
/// closed command enum owns the verb spelling, so a declaration cannot pair
/// one verb with another verb's body.
#[derive(Debug, Serialize)]
pub(super) struct ReplayDeclaration {
    version: u8,
    #[serde(flatten)]
    command: ReplayCommand,
}

#[derive(Debug, Deserialize, Serialize)]
struct StoredReplayDeclaration {
    version: u8,
    #[serde(flatten)]
    command: StoredReplayCommand,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "verb", deny_unknown_fields)]
enum StoredReplayCommand {
    #[serde(rename = "create")]
    Create(StoredCreateBody),
    #[serde(rename = "transition")]
    Transition(StoredTransitionBody),
    #[serde(rename = "achieve")]
    Achieve(StoredPriorAuthorshipBody),
    #[serde(rename = "modify")]
    Modify(StoredModifyBody),
    #[serde(rename = "decompose_child")]
    DecomposeChild(StoredDecomposeChildBody),
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredCreateBody {
    draft: StoredDraft,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredDraft {
    schema_id: String,
    schema_version: u32,
    title: String,
    text: String,
    payload: Vec<u8>,
    sidecar: Option<StoredSidecar>,
    state: StoredGoalState,
    topology: StoredTopology,
    supersedes_goal_id: Option<uuid::Uuid>,
    authorship: StoredAuthorship,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredPayload {
    schema_id: String,
    schema_version: u32,
    title: String,
    text: String,
    payload: Vec<u8>,
    sidecar: Option<StoredSidecar>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredSidecar {
    kind: StoredPayloadKind,
    schema_id: String,
    schema_version: u32,
    payload: Value,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "PascalCase")]
enum StoredPayloadKind {
    Fact,
    Abstraction,
    Perspective,
    Goal,
    CitedObject,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "PascalCase")]
enum StoredGoalState {
    Active,
    Paused,
    Achieved,
    Abandoned,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum StoredAuthorship {
    User,
    SystemOperator,
    SystemTool,
    External,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum StoredPendingWork {
    Keep,
    Discard,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredTransitionBody {
    prior_goal_id: uuid::Uuid,
    next_state: StoredGoalState,
    authorship: StoredAuthorship,
    #[serde(skip_serializing_if = "Option::is_none")]
    pending: Option<StoredPendingWork>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredPriorAuthorshipBody {
    prior_goal_id: uuid::Uuid,
    authorship: StoredAuthorship,
    evidence: Option<Vec<StoredEvidenceRef>>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredModifyBody {
    prior_goal_id: uuid::Uuid,
    replacement: StoredPayload,
    authorship: StoredAuthorship,
    evidence: Option<Vec<StoredEvidenceRef>>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredDecomposeChildBody {
    parent_goal_id: uuid::Uuid,
    authorship: StoredAuthorship,
    topology: StoredTopology,
    child_index: usize,
    payload: StoredPayload,
    evidence: Vec<StoredEvidenceRef>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredTopology {
    motivated_by: Vec<StoredMotivationRef>,
    dependencies: Vec<StoredDependencyRef>,
    evidence: Vec<StoredEvidenceRef>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredMotivationRef {
    perspective_id: uuid::Uuid,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredDependencyRef {
    goal_id: uuid::Uuid,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredEvidenceRef {
    memory_id: uuid::Uuid,
}

fn rewrite_stored_topology(
    topology: &mut StoredTopology,
    map_memory: &dyn Fn(uuid::Uuid) -> Result<uuid::Uuid, StorageError>,
    map_goal: &dyn Fn(uuid::Uuid) -> Result<uuid::Uuid, StorageError>,
) -> Result<(), StorageError> {
    for reference in &mut topology.motivated_by {
        reference.perspective_id = map_memory(reference.perspective_id)?;
    }
    for reference in &mut topology.dependencies {
        reference.goal_id = map_goal(reference.goal_id)?;
    }
    for reference in &mut topology.evidence {
        reference.memory_id = map_memory(reference.memory_id)?;
    }
    Ok(())
}

fn rewrite_stored_evidence(
    evidence: &mut [StoredEvidenceRef],
    map_memory: &dyn Fn(uuid::Uuid) -> Result<uuid::Uuid, StorageError>,
) -> Result<(), StorageError> {
    for reference in evidence {
        reference.memory_id = map_memory(reference.memory_id)?;
    }
    Ok(())
}

fn replacement_payload_value(
    replacement: &GoalPayloadWrite,
) -> Result<StoredPayload, StorageError> {
    let value = serde_json::to_value(payload_declaration(replacement)?)
        .map_err(|error| internal(format!("serialize replacement Goal payload: {error}")))?;
    serde_json::from_value(value)
        .map_err(|error| internal(format!("encode replacement Goal payload: {error}")))
}

/// Rewrite one persisted v1 declaration at the database boundary. The stored
/// sidecar remains opaque JSON; only typed replay structure and IDs are parsed.
pub(crate) fn rewrite_goal_replay(
    raw: Value,
    replacement: &GoalPayloadWrite,
    map_memory: &dyn Fn(uuid::Uuid) -> Result<uuid::Uuid, StorageError>,
    map_goal: &dyn Fn(uuid::Uuid) -> Result<uuid::Uuid, StorageError>,
) -> Result<Value, StorageError> {
    let mut declaration: StoredReplayDeclaration = serde_json::from_value(raw)
        .map_err(|error| internal(format!("invalid Goal replay declaration: {error}")))?;
    if declaration.version != 1 {
        return Err(internal(format!(
            "unsupported Goal replay declaration version {}",
            declaration.version
        )));
    }
    let replacement = replacement_payload_value(replacement)?;
    match &mut declaration.command {
        StoredReplayCommand::Create(body) => {
            body.draft.payload.clone_from(&replacement.payload);
            body.draft.schema_id.clone_from(&replacement.schema_id);
            body.draft.schema_version = replacement.schema_version;
            body.draft.title.clone_from(&replacement.title);
            body.draft.text.clone_from(&replacement.text);
            body.draft.sidecar.clone_from(&replacement.sidecar);
            body.draft.supersedes_goal_id =
                body.draft.supersedes_goal_id.map(map_goal).transpose()?;
            rewrite_stored_topology(&mut body.draft.topology, map_memory, map_goal)?;
        }
        StoredReplayCommand::Transition(body) => {
            body.prior_goal_id = map_goal(body.prior_goal_id)?;
        }
        StoredReplayCommand::Achieve(body) => {
            body.prior_goal_id = map_goal(body.prior_goal_id)?;
            if let Some(evidence) = &mut body.evidence {
                rewrite_stored_evidence(evidence, map_memory)?;
            }
        }
        StoredReplayCommand::Modify(body) => {
            body.prior_goal_id = map_goal(body.prior_goal_id)?;
            body.replacement.clone_from(&replacement);
            if let Some(evidence) = &mut body.evidence {
                rewrite_stored_evidence(evidence, map_memory)?;
            }
        }
        StoredReplayCommand::DecomposeChild(body) => {
            body.parent_goal_id = map_goal(body.parent_goal_id)?;
            body.payload.clone_from(&replacement);
            rewrite_stored_topology(&mut body.topology, map_memory, map_goal)?;
            rewrite_stored_evidence(&mut body.evidence, map_memory)?;
        }
    }
    serde_json::to_value(declaration).map_err(|error| {
        internal(format!(
            "serialize rewritten Goal replay declaration: {error}"
        ))
    })
}

fn replay_declaration(command: ReplayCommand) -> ReplayDeclaration {
    ReplayDeclaration {
        version: 1,
        command,
    }
}

fn sidecar_declaration(
    sidecar: Option<&proxiima_core::SidecarPayload>,
) -> Result<Option<ReplaySidecar>, StorageError> {
    let Some(sidecar) = sidecar else {
        return Ok(None);
    };
    let payload = sidecar.durable().map_err(internal)?;
    Ok(Some(ReplaySidecar {
        kind: sidecar.kind.into(),
        schema_id: sidecar.schema_id.as_str().to_owned(),
        schema_version: sidecar.schema_version.into_inner(),
        payload,
    }))
}

fn payload_declaration(payload: &GoalPayloadWrite) -> Result<ReplayPayload, StorageError> {
    Ok(ReplayPayload {
        schema_id: payload.schema_id.as_str().to_owned(),
        schema_version: payload.schema_version.into_inner(),
        title: payload.title.clone(),
        text: payload.text.clone(),
        payload: payload.payload.clone(),
        sidecar: sidecar_declaration(payload.sidecar_payload.as_ref())?,
    })
}

fn draft_declaration(draft: &GoalDraft) -> Result<ReplayDraft, StorageError> {
    Ok(ReplayDraft {
        schema_id: draft.schema_id.as_str().to_owned(),
        schema_version: draft.schema_version.into_inner(),
        title: draft.title.clone(),
        text: draft.text.clone(),
        payload: draft.payload.clone(),
        sidecar: sidecar_declaration(draft.sidecar_payload.as_ref())?,
        state: ReplayGoalState(draft.state),
        topology: (&draft.topology).into(),
        supersedes_goal_id: draft.supersedes_goal_id.map(GoalId::into_inner),
        authorship: authorship_declaration(&draft.authorship),
    })
}

pub(super) fn create_replay_declaration(
    req: &CreateGoalAtomicRequest<'_>,
) -> Result<ReplayDeclaration, StorageError> {
    // `write_act_t` is an episode binding attempt, not Goal content. A replay
    // returns the stored row without rewriting it so the episode layer can
    // reject a bound replay with its protocol-specific error.
    Ok(replay_declaration(ReplayCommand::Create(CreateBody {
        draft: draft_declaration(&req.draft)?,
    })))
}

pub(super) fn transition_replay_declaration(
    req: &TransitionGoalAtomicRequest<'_>,
) -> ReplayDeclaration {
    let prior = req.prior_goal_id.into_inner();
    let pending = match req.transition {
        GoalTransitionKind::Pause { pending } => Some(pending),
        GoalTransitionKind::Resume | GoalTransitionKind::Abandon => None,
    };
    // A pause also declares what happens to pending work, so the same request
    // id with the other choice is a conflict, not a replay. Resume and
    // abandon declare nothing new, and a declaration stored before `pending`
    // existed still matches them.
    replay_declaration(ReplayCommand::Transition(TransitionBody {
        prior_goal_id: prior,
        next_state: ReplayGoalState(req.transition.next_state()),
        authorship: authorship_declaration(&req.authorship),
        pending: pending.map(ReplayPendingWork),
    }))
}

pub(super) fn achieve_replay_declaration(req: &AchieveGoalAtomicRequest<'_>) -> ReplayDeclaration {
    let prior = req.prior_goal_id.into_inner();
    replay_declaration(ReplayCommand::Achieve(PriorAuthorshipBody {
        prior_goal_id: prior,
        authorship: authorship_declaration(&req.authorship),
        evidence: Some(req.evidence.iter().copied().map(Into::into).collect()),
    }))
}

pub(super) fn modify_replay_declaration(
    req: &ModifyGoalAtomicRequest<'_>,
) -> Result<ReplayDeclaration, StorageError> {
    let prior = req.prior_goal_id.into_inner();
    Ok(replay_declaration(ReplayCommand::Modify(ModifyBody {
        prior_goal_id: prior,
        replacement: payload_declaration(&req.replacement)?,
        authorship: authorship_declaration(&req.authorship),
        evidence: req
            .evidence
            .as_ref()
            .map(|evidence| evidence.iter().copied().map(Into::into).collect()),
    })))
}

type ReplayProbeRow = (
    i64,
    Option<uuid::Uuid>,
    Option<uuid::Uuid>,
    Option<i32>,
    Option<bool>,
);

pub(super) fn decompose_child_replay_declaration(
    req: &DecomposeGoalAtomicRequest<'_>,
    child: &ChildGoalDraft,
    child_index: usize,
) -> Result<ReplayDeclaration, StorageError> {
    let parent = req.parent_goal_id.into_inner();
    Ok(replay_declaration(ReplayCommand::DecomposeChild(
        DecomposeChildBody {
            parent_goal_id: parent,
            authorship: authorship_declaration(&req.authorship),
            topology: (&req.topology).into(),
            child_index,
            payload: payload_declaration(&child.payload)?,
            evidence: child.evidence.iter().copied().map(Into::into).collect(),
        },
    )))
}

pub(super) fn decompose_replay_declarations(
    req: &DecomposeGoalAtomicRequest<'_>,
) -> Result<Vec<ReplayDeclaration>, StorageError> {
    req.children
        .iter()
        .enumerate()
        .map(|(index, child)| decompose_child_replay_declaration(req, child, index))
        .collect()
}

pub(super) async fn resolve_decompose_replay_set(
    conn: &mut PgConnection,
    req: &DecomposeGoalAtomicRequest<'_>,
    declarations: &[ReplayDeclaration],
) -> Result<Option<DecomposeGoalOutcome>, StorageError> {
    let request_ids = req
        .children
        .iter()
        .map(|child| child.request_id.as_str().to_owned())
        .collect::<Vec<_>>();
    let declaration_json = declarations
        .iter()
        .map(serde_json::to_string)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| internal(format!("serialize Goal replay declaration: {error}")))?;
    let rows: Vec<ReplayProbeRow> = sqlx::query_as(
        "SELECT r.ordinality::bigint, g.t, g.handle, d.edge_count,
                d.declaration = r.declaration::jsonb
           FROM unnest($2::text[], $3::text[]) WITH ORDINALITY
                  AS r(request_id, declaration, ordinality)
           LEFT JOIN proxiima_core.goal_replay_declaration d
             ON d.owner_id = $1 AND d.request_id = r.request_id
           LEFT JOIN proxiima_core.goal admitted
             ON admitted.owner_id = $1 AND admitted.request_id = r.request_id
           LEFT JOIN proxiima_core.goal g
             ON g.t = COALESCE(d.goal_t, admitted.t)
          ORDER BY r.ordinality",
    )
    .bind(req.owner.into_inner())
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
    for (child, (_, goal_t, goal_handle, edge_count, matches)) in req.children.iter().zip(rows) {
        let Some(goal_t) = goal_t else {
            replayed.push(None);
            continue;
        };
        let key = &child.request_id;
        match judge(Some((goal_t, matches == Some(true))), key) {
            Ok(KeyVerdict::Replay(t)) => {
                reused.get_or_insert(child.request_id.as_str());
                let goal_handle =
                    goal_handle.ok_or_else(|| internal("Goal replay row missing stable handle"))?;
                replayed.push(Some(replay_outcome(t, goal_handle, edge_count)?));
            }
            Ok(KeyVerdict::Fresh) => replayed.push(None),
            Err(conflict) => {
                return Err(idempotency_conflict(&conflict.key, Some(conflict.existing)));
            }
        }
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
            return Err(idempotency_conflict(reused, None));
        };
        children.push(DecomposedGoalOutcome { outcome });
    }
    Ok(Some(DecomposeGoalOutcome {
        children,
        idempotent_replay: true,
    }))
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
/// live assignment, evidence, or prior-head probe.
pub(super) async fn resolve_goal_replay(
    conn: &mut PgConnection,
    owner: &OwnerId,
    request_id: &str,
    declaration: &ReplayDeclaration,
) -> Result<Option<GoalWriteOutcome>, StorageError> {
    let row: Option<(uuid::Uuid, uuid::Uuid, Option<i32>, Option<bool>)> = sqlx::query_as(
        "SELECT g.t, g.handle, d.edge_count, d.declaration = $3::jsonb
           FROM (VALUES ($1::uuid, $2::text)) request(owner_id, request_id)
           LEFT JOIN proxiima_core.goal_replay_declaration d
             ON d.owner_id = request.owner_id AND d.request_id = request.request_id
           LEFT JOIN proxiima_core.goal admitted
             ON admitted.owner_id = request.owner_id AND admitted.request_id = request.request_id
           JOIN proxiima_core.goal g ON g.t = COALESCE(d.goal_t, admitted.t)",
    )
    .bind(owner.into_inner())
    .bind(request_id)
    .bind(sqlx::types::Json(declaration))
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_err)?;
    let Some((goal_t, goal_handle, edge_count, matches)) = row else {
        return Ok(None);
    };
    let key =
        IdempotencyKey::new(request_id.to_string()).map_err(StorageError::ConstraintViolation)?;
    match judge(Some((goal_t, matches == Some(true))), &key) {
        Ok(KeyVerdict::Replay(t)) => replay_outcome(t, goal_handle, edge_count).map(Some),
        Ok(KeyVerdict::Fresh) => Ok(None),
        Err(conflict) => Err(idempotency_conflict(&conflict.key, Some(conflict.existing))),
    }
}

fn replay_outcome(
    goal_t: uuid::Uuid,
    goal_handle: uuid::Uuid,
    edge_count: Option<i32>,
) -> Result<GoalWriteOutcome, StorageError> {
    let edge_count = edge_count
        .and_then(|count| usize::try_from(count).ok())
        .ok_or_else(|| internal("Goal replay declaration has invalid edge_count"))?;
    Ok(GoalWriteOutcome {
        goal_id: GoalId::new(goal_t),
        goal_handle: proxiima_core::goal_collaboration::GoalHandle::new(goal_handle),
        edge_count,
        idempotent_replay: true,
    })
}

pub(super) async fn require_goal_replay(
    conn: &mut PgConnection,
    owner: &OwnerId,
    request_id: &str,
    declaration: &ReplayDeclaration,
) -> Result<GoalWriteOutcome, StorageError> {
    resolve_goal_replay(conn, owner, request_id, declaration)
        .await?
        .ok_or_else(|| internal("Goal replay row disappeared after request-id match"))
}

pub(super) async fn record_goal_replay_declaration(
    conn: &mut PgConnection,
    declaration: &ReplayDeclaration,
    outcome: &GoalWriteOutcome,
) -> Result<(), StorageError> {
    let (owner_id, request_id): (uuid::Uuid, String) =
        sqlx::query_as("SELECT owner_id, request_id FROM proxiima_core.goal WHERE t = $1")
            .bind(outcome.goal_id.into_inner())
            .fetch_one(&mut *conn)
            .await
            .map_err(map_err)?;
    let edge_count = i32::try_from(outcome.edge_count)
        .map_err(|_| internal("Goal replay edge_count exceeds PostgreSQL integer"))?;
    sqlx::query(
        "INSERT INTO proxiima_core.goal_replay_declaration
             (owner_id, request_id, goal_t, declaration, edge_count)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(owner_id)
    .bind(request_id)
    .bind(outcome.goal_id.into_inner())
    .bind(sqlx::types::Json(declaration))
    .bind(edge_count)
    .execute(&mut *conn)
    .await
    .map_err(map_err)?;
    Ok(())
}

pub(super) fn idempotency_conflict(request_id: &str, existing: Option<uuid::Uuid>) -> StorageError {
    // The `Display` form is load-bearing: storage-level callers match on the
    // message `idempotency_conflict:{request_id}`. The engine matches the variant.
    StorageError::IdempotencyConflict {
        request_id: request_id.to_string(),
        existing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proxiima_core::GoalTopologyWrite;
    use proxiima_core::verbs::goal_write::GoalState;
    use serde_json::{Value, json};

    fn topology() -> GoalTopologyWrite {
        GoalTopologyWrite::new(Vec::new(), Vec::new(), Vec::new()).expect("empty topology")
    }

    fn payload() -> ReplayPayload {
        ReplayPayload {
            schema_id: "test/schema-v1".to_owned(),
            schema_version: 1,
            title: "title".to_owned(),
            text: "text".to_owned(),
            payload: vec![1, 2, 3],
            sidecar: None,
        }
    }

    fn serialized<T: Serialize>(value: &T) -> Value {
        serde_json::to_value(value).expect("replay declaration serializes")
    }

    fn replacement() -> GoalPayloadWrite {
        GoalPayloadWrite::from_payload(
            "replacement title",
            "replacement text",
            proxiima_core::TaskGoalV1 {
                due_at: Some(time::OffsetDateTime::UNIX_EPOCH),
                priority: Some(proxiima_core::TaskPriority::High),
            },
        )
        .expect("replacement payload")
    }

    fn rewrite_identity(raw: Value) -> Value {
        rewrite_goal_replay(raw, &replacement(), &|id| Ok(id), &|id| Ok(id)).expect("rewrite")
    }

    #[test]
    fn rewrite_roundtrips_all_goal_replay_verbs_and_replaces_payload_slots() {
        let id = uuid::Uuid::from_u128(1);
        let declarations = [
            serialized(&ReplayDeclaration {
                version: 1,
                command: ReplayCommand::Create(CreateBody {
                    draft: ReplayDraft {
                        schema_id: "old/schema".into(),
                        schema_version: 1,
                        title: "old".into(),
                        text: "old".into(),
                        payload: vec![0],
                        sidecar: None,
                        state: ReplayGoalState(GoalState::Active),
                        topology: ReplayTopology {
                            motivated_by: vec![ReplayMotivationRef { perspective_id: id }],
                            dependencies: vec![ReplayDependencyRef { goal_id: id }],
                            evidence: vec![ReplayEvidenceRef { memory_id: id }],
                        },
                        supersedes_goal_id: Some(id),
                        authorship: "user",
                    },
                }),
            }),
            serialized(&ReplayDeclaration {
                version: 1,
                command: ReplayCommand::Transition(TransitionBody {
                    prior_goal_id: id,
                    next_state: ReplayGoalState(GoalState::Paused),
                    authorship: "user",
                    pending: Some(ReplayPendingWork(proxiima_core::PendingWork::Keep)),
                }),
            }),
            serialized(&ReplayDeclaration {
                version: 1,
                command: ReplayCommand::Achieve(PriorAuthorshipBody {
                    prior_goal_id: id,
                    authorship: "user",
                    evidence: Some(vec![ReplayEvidenceRef { memory_id: id }]),
                }),
            }),
            serialized(&ReplayDeclaration {
                version: 1,
                command: ReplayCommand::Modify(ModifyBody {
                    prior_goal_id: id,
                    replacement: payload(),
                    authorship: "user",
                    evidence: Some(vec![ReplayEvidenceRef { memory_id: id }]),
                }),
            }),
            serialized(&ReplayDeclaration {
                version: 1,
                command: ReplayCommand::DecomposeChild(DecomposeChildBody {
                    parent_goal_id: id,
                    authorship: "user",
                    topology: (&topology()).into(),
                    child_index: 0,
                    payload: payload(),
                    evidence: vec![ReplayEvidenceRef { memory_id: id }],
                }),
            }),
        ];
        for raw in declarations {
            let rewritten = rewrite_identity(raw.clone());
            let reparsed: StoredReplayDeclaration =
                serde_json::from_value(rewritten.clone()).expect("rewritten v1 declaration");
            assert_eq!(reparsed.version, 1);
            assert_eq!(rewritten["version"], raw["version"]);
            assert_eq!(rewritten["verb"], raw["verb"]);
            if raw["verb"] == "transition" || raw["verb"] == "achieve" {
                assert_eq!(rewritten, raw);
            }
        }
    }

    #[test]
    fn rewrite_refuses_unknown_version_and_closed_values() {
        let base = json!({
            "version": 1,
            "verb": "transition",
            "prior_goal_id": uuid::Uuid::from_u128(1),
            "next_state": "Active",
            "authorship": "user"
        });
        for (field, value) in [
            ("version", json!(2)),
            ("next_state", json!("Unknown")),
            ("authorship", json!("robot")),
            ("pending", json!("later")),
            ("unexpected", json!(true)),
        ] {
            let mut raw = base.clone();
            raw[field] = value;
            assert!(
                rewrite_goal_replay(raw, &replacement(), &|id| Ok(id), &|id| Ok(id)).is_err(),
                "{field}"
            );
        }
    }

    #[test]
    fn rewrite_refuses_unknown_sidecar_kind() {
        let raw = json!({
            "version": 1,
            "verb": "modify",
            "prior_goal_id": uuid::Uuid::from_u128(1),
            "replacement": {
                "schema_id": "old/schema", "schema_version": 1,
                "title": "old", "text": "old", "payload": [0],
                "sidecar": {
                    "kind": "Unknown", "schema_id": "old/schema",
                    "schema_version": 1, "payload": {}
                }
            },
            "authorship": "user", "evidence": null
        });
        assert!(rewrite_goal_replay(raw, &replacement(), &|id| Ok(id), &|id| Ok(id)).is_err());
    }

    #[test]
    fn rewrite_propagates_mapping_refusal() {
        let id = uuid::Uuid::from_u128(1);
        let raw = serialized(&ReplayDeclaration {
            version: 1,
            command: ReplayCommand::Transition(TransitionBody {
                prior_goal_id: id,
                next_state: ReplayGoalState(GoalState::Active),
                authorship: "user",
                pending: None,
            }),
        });
        let result = rewrite_goal_replay(
            raw,
            &replacement(),
            &|_| Err(internal("memory map refused")),
            &|_| Err(internal("goal map refused")),
        );
        assert!(result.is_err());
    }

    #[test]
    fn typed_draft_projection_preserves_nested_v1_declarations() {
        use proxiima_core::{GoalDependencyRef, GoalEvidenceRef, GoalMotivationRef, MemoryId};

        let id = uuid::Uuid::from_u128(1);
        let payload = proxiima_core::TaskGoalV1 {
            due_at: Some(time::OffsetDateTime::UNIX_EPOCH),
            priority: Some(proxiima_core::TaskPriority::High),
        };
        let encoded = GoalPayloadWrite::from_payload("title", "text", payload.clone()).unwrap();
        let topology = GoalTopologyWrite::new(
            vec![GoalMotivationRef::perspective(MemoryId::new(id))],
            vec![GoalDependencyRef::new(GoalId::new(id))],
            vec![GoalEvidenceRef::new(MemoryId::new(id))],
        )
        .unwrap();
        let draft = GoalDraft::active_from_payload_write(
            OwnerId::new(id),
            encoded,
            topology,
            GoalAuthorship::User,
            IdempotencyKey::new("replay-projection").unwrap(),
        );
        let legacy = json!({
            "schema_id": draft.schema_id.as_str(),
            "schema_version": draft.schema_version.into_inner(),
            "title": draft.title,
            "text": draft.text,
            "payload": draft.payload,
            "sidecar": {
                "kind": "Goal",
                "schema_id": draft.schema_id.as_str(),
                "schema_version": draft.schema_version.into_inner(),
                "payload": payload,
            },
            "state": draft.state,
            "topology": draft.topology,
            "supersedes_goal_id": null,
            "authorship": "user",
        });
        assert_eq!(serialized(&draft_declaration(&draft).unwrap()), legacy);
    }

    #[test]
    fn create_declaration_preserves_the_versioned_wire_shape() {
        let create = ReplayDeclaration {
            version: 1,
            command: ReplayCommand::Create(CreateBody {
                draft: ReplayDraft {
                    schema_id: "test/schema-v1".to_owned(),
                    schema_version: 1,
                    title: "title".to_owned(),
                    text: "text".to_owned(),
                    payload: vec![1, 2, 3],
                    sidecar: None,
                    state: ReplayGoalState(GoalState::Active),
                    topology: (&topology()).into(),
                    supersedes_goal_id: None,
                    authorship: "user",
                },
            }),
        };
        assert_eq!(
            serialized(&create),
            json!({
                "version": 1,
                "verb": "create",
                "draft": {
                    "schema_id": "test/schema-v1",
                    "schema_version": 1,
                    "title": "title",
                    "text": "text",
                    "payload": [1, 2, 3],
                    "sidecar": null,
                    "state": "Active",
                    "topology": {"motivated_by": [], "dependencies": [], "evidence": []},
                    "supersedes_goal_id": null,
                    "authorship": "user"
                }
            })
        );
    }

    #[test]
    fn transition_declaration_preserves_the_versioned_wire_shape() {
        let id = uuid::Uuid::from_u128(1);
        let pause = ReplayDeclaration {
            version: 1,
            command: ReplayCommand::Transition(TransitionBody {
                prior_goal_id: id,
                next_state: ReplayGoalState(GoalState::Paused),
                authorship: "system_tool",
                pending: Some(ReplayPendingWork(proxiima_core::PendingWork::Keep)),
            }),
        };
        assert_eq!(
            serialized(&pause),
            json!({
                "version": 1,
                "verb": "transition",
                "prior_goal_id": id,
                "next_state": "Paused",
                "authorship": "system_tool",
                "pending": "keep"
            })
        );

        let resume = ReplayDeclaration {
            version: 1,
            command: ReplayCommand::Transition(TransitionBody {
                prior_goal_id: id,
                next_state: ReplayGoalState(GoalState::Active),
                authorship: "external",
                pending: None,
            }),
        };
        assert_eq!(
            serialized(&resume),
            json!({
                "version": 1,
                "verb": "transition",
                "prior_goal_id": id,
                "next_state": "Active",
                "authorship": "external"
            })
        );
    }

    #[test]
    fn achieve_declaration_preserves_the_versioned_wire_shape() {
        let id = uuid::Uuid::from_u128(1);
        let achieve = ReplayDeclaration {
            version: 1,
            command: ReplayCommand::Achieve(PriorAuthorshipBody {
                prior_goal_id: id,
                authorship: "user",
                evidence: Some(Vec::new()),
            }),
        };
        assert_eq!(
            serialized(&achieve),
            json!({
                "version": 1,
                "verb": "achieve",
                "prior_goal_id": id,
                "authorship": "user",
                "evidence": []
            })
        );
    }

    #[test]
    fn modify_declaration_preserves_the_versioned_wire_shape() {
        let id = uuid::Uuid::from_u128(1);
        let modify = ReplayDeclaration {
            version: 1,
            command: ReplayCommand::Modify(ModifyBody {
                prior_goal_id: id,
                replacement: payload(),
                authorship: "user",
                evidence: None,
            }),
        };
        assert_eq!(
            serialized(&modify),
            json!({
                "version": 1,
                "verb": "modify",
                "prior_goal_id": id,
                "replacement": {
                    "schema_id": "test/schema-v1",
                    "schema_version": 1,
                    "title": "title",
                    "text": "text",
                    "payload": [1, 2, 3],
                    "sidecar": null
                },
                "authorship": "user",
                "evidence": null
            })
        );
    }

    #[test]
    fn decompose_child_declaration_preserves_the_versioned_wire_shape() {
        let id = uuid::Uuid::from_u128(1);
        let child = ReplayDeclaration {
            version: 1,
            command: ReplayCommand::DecomposeChild(DecomposeChildBody {
                parent_goal_id: id,
                authorship: "user",
                topology: (&topology()).into(),
                child_index: 0,
                payload: payload(),
                evidence: Vec::new(),
            }),
        };
        assert_eq!(
            serialized(&child),
            json!({
                "version": 1,
                "verb": "decompose_child",
                "parent_goal_id": id,
                "authorship": "user",
                "topology": {"motivated_by": [], "dependencies": [], "evidence": []},
                "child_index": 0,
                "payload": {
                    "schema_id": "test/schema-v1",
                    "schema_version": 1,
                    "title": "title",
                    "text": "text",
                    "payload": [1, 2, 3],
                    "sidecar": null
                },
                "evidence": []
            })
        );
    }
}
