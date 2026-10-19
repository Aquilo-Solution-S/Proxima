use crate::engine::{
    GoalCreatePayloadWriteRequest, GoalDecomposeRequest, GoalMarkAchievedRequest,
    GoalModifyRequest, GoalTransitionRequest,
};
use crate::mcp::{
    CoreActionMeta, McpActionArgSpec, McpTool, McpToolAudience, McpToolCtx, McpToolError,
};
use crate::mcp::{Replay, ToolEffect};
use crate::protocol::{action as protocol_action, tool as protocol_tool};
use crate::tool::validate_trimmed_len;
use crate::verbs::goal_write::{
    ChildGoalDraft, GoalAssignmentTarget, GoalAuthorship, GoalEvidenceRef, GoalPayloadWrite,
    GoalTopologyWrite, GoalTransitionKind, GoalWriteOutcome, IdempotencyKey, OperatorKind,
    SystemOrigin,
};
use crate::verbs::schema::PayloadKind;
use crate::{
    InputContractId, ModelId, OperatorId, PendingWork, PromptVersion, SchemaId, SchemaVersion,
    ToolId,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

const MAX_CHILD_GOALS: usize = 50;
/// Prefix of every decompose child key, `"goal_decompose:{root}:{index}"`.
const DECOMPOSE_CHILD_PREFIX: &str = "goal_decompose";
/// The longest decompose root key whose every child key still fits
/// [`IdempotencyKey::MAX_CHARS`]: the prefix, two separators and the widest
/// child index come off the common bound (180 - (14 + 2 + 2) = 162).
const DECOMPOSE_ROOT_KEY_MAX: usize = IdempotencyKey::MAX_CHARS
    - (DECOMPOSE_CHILD_PREFIX.len() + 2 + decimal_digits(MAX_CHILD_GOALS - 1));

/// How many decimal digits `n` prints with.
const fn decimal_digits(mut n: usize) -> usize {
    let mut digits = 1;
    while n >= 10 {
        n /= 10;
        digits += 1;
    }
    digits
}
const MCP_OPERATOR_NAMESPACE: uuid::Uuid = uuid::Uuid::from_bytes([
    0x3f, 0x61, 0xde, 0x85, 0x4e, 0x09, 0x45, 0x62, 0x97, 0xc4, 0x8a, 0x74, 0xaa, 0xf9, 0x4a, 0x2c,
]);
const MCP_GOAL_INPUT_CONTRACT_NAMESPACE: uuid::Uuid = uuid::Uuid::from_bytes([
    0xb2, 0x3f, 0x07, 0xef, 0xeb, 0x6f, 0x4f, 0xe7, 0xa9, 0xd3, 0x67, 0xa9, 0x53, 0xf3, 0x4a, 0x6d,
]);
const CORE_GOAL_PRODUCES_SCHEMA_IDS: &[&str] = &[];
pub const CORE_GOAL_ACTIONS: &[CoreActionMeta] = &[
    CoreActionMeta {
        tool: CoreGoalTool::NAME,
        action: "set",
        scope_key: protocol_action::CORE_GOAL_SET,
        produces_schema_ids: &[],
    },
    CoreActionMeta {
        tool: CoreGoalTool::NAME,
        action: "transition",
        scope_key: protocol_action::CORE_GOAL_TRANSITION,
        produces_schema_ids: &[],
    },
    CoreActionMeta {
        tool: CoreGoalTool::NAME,
        action: "modify",
        scope_key: protocol_action::CORE_GOAL_MODIFY,
        produces_schema_ids: &[],
    },
    CoreActionMeta {
        tool: CoreGoalTool::NAME,
        action: "mark_achieved",
        scope_key: protocol_action::CORE_GOAL_MARK_ACHIEVED,
        produces_schema_ids: &[],
    },
    CoreActionMeta {
        tool: CoreGoalTool::NAME,
        action: "decompose",
        scope_key: protocol_action::CORE_GOAL_DECOMPOSE,
        produces_schema_ids: &[],
    },
];

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct GoalPayloadArgs {
    #[schemars(
        description = "Goal-payload schema id, e.g. `core/simple-text-v1`. The `proxima://schemas?kind=Goal` resource lists every registered id."
    )]
    pub schema_id: String,
    #[schemars(description = "Version of `schema_id`. Omit for 1.")]
    pub schema_version: Option<u32>,
    #[schemars(
        length(max = 240),
        description = "Short goal title, 1 to 240 chars after trimming."
    )]
    pub title: String,
    #[schemars(
        length(max = 20000),
        description = "The goal in prose — what pursuing or achieving it means. 1 to 20000 chars after trimming."
    )]
    pub text: String,
    #[serde(default = "default_empty_object")]
    #[schemars(
        with = "std::collections::BTreeMap<String, serde_json::Value>",
        description = "Payload object whose fields `schema_id` defines. Omit for `{}`."
    )]
    pub body: serde_json::Value,
}

fn default_empty_object() -> serde_json::Value {
    serde_json::Value::Object(serde_json::Map::new())
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GoalSetArgs {
    #[serde(flatten)]
    pub payload: GoalPayloadArgs,
    #[schemars(
        length(min = 1),
        description = "Fact or Abstraction handles (`<uuid>`) that motivate the goal; at least one."
    )]
    pub evidence: Vec<String>,
    #[schemars(
        description = "Perspective handle (`<uuid>`) the goal is assigned to, e.g. a stance `core_interpret` returned. Required unless the host supplies a caller Perspective."
    )]
    pub target_perspective: Option<String>,
    #[schemars(
        description = "Optional key, 1 to 180 chars, naming this one request: same key and content replays; changed content is `idempotency_conflict`."
    )]
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct GoalWriteOutput {
    pub handle: String,
    /// Index rows the write asserted. Not handles: an edge has no id.
    pub edge_count: usize,
    pub idempotent_replay: bool,
}

#[derive(Debug)]
pub struct CoreGoalTool;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum CoreGoalArgs {
    /// Create an Active goal assigned to a Perspective.
    Set(GoalSetArgs),
    /// Pause, resume, or abandon a goal; abandon is refused while a child goal is Active or Paused.
    Transition(GoalTransitionArgs),
    /// Replace an Active goal's title, text and payload; omitted evidence carries over.
    Modify(GoalModifyArgs),
    /// Mark a goal Achieved, citing the memories that show it; a goal with child goals needs every
    /// child Achieved or Abandoned and at least one Achieved.
    MarkAchieved(GoalMarkAchievedArgs),
    /// Create Active child Goals under a parent goal.
    Decompose(GoalDecomposeArgs),
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(untagged)]
pub enum CoreGoalOutput {
    Write(GoalWriteOutput),
    Decompose(GoalDecomposeOutput),
}

impl McpTool for CoreGoalTool {
    const NAME: &'static str = protocol_tool::CORE_GOAL;
    const DESCRIPTION: &'static str = "Write Goals: set one, decompose it into child goals, \
         pause/resume/abandon it, replace its content, mark it achieved. Every write returns the \
         goal's new head `handle`; the next write on that goal names that handle. set and decompose \
         assign goals to a Perspective: pass target_perspective unless the host supplies a caller \
         Perspective. A pause names what happens to work already queued for the goal: pending is \
         keep or discard. A parent is marked achieved only after its children: none Active or \
         Paused, at least one Achieved. An open goal never sits under a closed one: achieve and \
         abandon a parent after its children.";
    const PRODUCES_SCHEMA_IDS: &'static [&'static str] = CORE_GOAL_PRODUCES_SCHEMA_IDS;
    const ACTION_ARG_SPECS: &'static [McpActionArgSpec] = &[
        McpActionArgSpec {
            action: "set",
            allowed_fields: &[
                "schema_id",
                "schema_version",
                "title",
                "text",
                "body",
                "evidence",
                "target_perspective",
                "idempotency_key",
            ],
            required_fields: &["schema_id", "title", "text", "evidence"],
            effect: ToolEffect::Additive(Replay::NonIdempotent),
            audience: McpToolAudience::Shared,
        },
        McpActionArgSpec {
            action: "transition",
            allowed_fields: &["goal", "transition", "pending", "idempotency_key"],
            // `pending` is required for pause and refused for the other two:
            // `GoalTransitionArgs::transition_kind` decides, once.
            required_fields: &["goal", "transition"],
            effect: ToolEffect::Additive(Replay::NonIdempotent),
            audience: McpToolAudience::Shared,
        },
        McpActionArgSpec {
            action: "modify",
            allowed_fields: &[
                "goal",
                "schema_id",
                "schema_version",
                "title",
                "text",
                "body",
                "evidence",
                "idempotency_key",
            ],
            required_fields: &["goal", "schema_id", "title", "text"],
            effect: ToolEffect::Additive(Replay::NonIdempotent),
            audience: McpToolAudience::Shared,
        },
        McpActionArgSpec {
            action: "mark_achieved",
            allowed_fields: &["goal", "evidence", "idempotency_key"],
            required_fields: &["goal", "evidence"],
            effect: ToolEffect::Additive(Replay::NonIdempotent),
            audience: McpToolAudience::Shared,
        },
        McpActionArgSpec {
            action: "decompose",
            allowed_fields: &[
                "parent_goal",
                "children",
                "target_perspective",
                "idempotency_key",
            ],
            required_fields: &["parent_goal", "children", "idempotency_key"],
            effect: ToolEffect::Additive(Replay::Idempotent),
            audience: McpToolAudience::Shared,
        },
    ];
    type Args = CoreGoalArgs;
    type Output = CoreGoalOutput;

    fn call(
        ctx: McpToolCtx,
        args: CoreGoalArgs,
    ) -> futures::future::BoxFuture<'static, Result<CoreGoalOutput, McpToolError>> {
        Box::pin(async move {
            match args {
                CoreGoalArgs::Set(args) => goal_set(ctx, args).await.map(CoreGoalOutput::Write),
                CoreGoalArgs::Transition(args) => {
                    goal_transition(ctx, args).await.map(CoreGoalOutput::Write)
                }
                CoreGoalArgs::Modify(args) => {
                    goal_modify(ctx, args).await.map(CoreGoalOutput::Write)
                }
                CoreGoalArgs::MarkAchieved(args) => goal_mark_achieved(ctx, args)
                    .await
                    .map(CoreGoalOutput::Write),
                CoreGoalArgs::Decompose(args) => goal_decompose(ctx, args)
                    .await
                    .map(CoreGoalOutput::Decompose),
            }
        })
    }
}

async fn goal_set(ctx: McpToolCtx, args: GoalSetArgs) -> Result<GoalWriteOutput, McpToolError> {
    if args.evidence.is_empty() {
        return Err(McpToolError::InvalidInput(
            "goal set requires >=1 Fact or Abstraction evidence handle motivating the goal".into(),
        ));
    }
    let payload = encode_goal_payload(&ctx, args.payload)?;
    let evidence = resolve_goal_evidence_handles(&ctx, &args.evidence)?;
    let assignment = target_perspective(&ctx, args.target_perspective.as_deref())?;
    let topology =
        GoalTopologyWrite::new(assignment, Vec::new(), evidence).map_err(McpToolError::Protocol)?;
    let request_id = IdempotencyKey::optional_or_generated("goal_set", args.idempotency_key)
        .map_err(McpToolError::InvalidInput)?;
    let authorship = system_operator_authorship(&ctx, "goal_set");
    let engine = ctx.require_engine()?;
    let outcome = engine
        .create_goal_from_payload_write(
            &ctx.authz,
            &GoalCreatePayloadWriteRequest {
                owner: ctx.owner,
                topology,
                payload,
                request_id,
                authorship,
                author_self_perspective_id: ctx.caller_self_perspective,
            },
        )
        .await
        .map_err(McpToolError::Protocol)?;
    Ok(format_goal_write_output(&ctx, &outcome))
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GoalTransition {
    #[serde(alias = "Pause", alias = "PAUSE")]
    Pause,
    #[serde(alias = "Resume", alias = "RESUME")]
    Resume,
    #[serde(alias = "Abandon", alias = "ABANDON")]
    Abandon,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GoalTransitionArgs {
    #[schemars(
        description = "Goal handle `<uuid>` from the latest write on this goal; an older handle is refused."
    )]
    pub goal: String,
    #[schemars(
        description = "pause applies to an Active goal, resume to a Paused one, abandon to an Active or Paused one. Use mark_achieved to achieve. abandon is refused while a child goal is still Active or Paused."
    )]
    pub transition: GoalTransition,
    #[schemars(
        description = "Required for pause, refused for resume and abandon. keep: the scheduler holds the work it queued for this goal and runs it after a resume; abandoning the goal drops it. discard: it drops that work, ready or waiting to retry. Running work finishes either way. null is the same as omitting it."
    )]
    pub pending: Option<PendingWork>,
    #[schemars(
        description = "Optional key, 1 to 180 chars, naming this one request: same key and content replays; changed content is `idempotency_conflict`."
    )]
    pub idempotency_key: Option<String>,
}

impl GoalTransitionArgs {
    /// The one place `transition` and `pending` meet: what the caller wrote
    /// becomes a [`GoalTransitionKind`], in which `pending` exists only on a
    /// pause.
    fn transition_kind(&self) -> Result<GoalTransitionKind, McpToolError> {
        match (&self.transition, self.pending) {
            (GoalTransition::Pause, Some(pending)) => Ok(GoalTransitionKind::Pause { pending }),
            (GoalTransition::Pause, None) => Err(McpToolError::InvalidInput(
                "pending is required when transition is pause: \"keep\" (the scheduler holds the \
                 queued work and runs it after a resume) or \"discard\" (it drops the queued work)"
                    .into(),
            )),
            (GoalTransition::Resume, None) => Ok(GoalTransitionKind::Resume),
            (GoalTransition::Abandon, None) => Ok(GoalTransitionKind::Abandon),
            (GoalTransition::Resume | GoalTransition::Abandon, Some(_)) => Err(
                McpToolError::InvalidInput("pending applies only when transition is pause".into()),
            ),
        }
    }
}

async fn goal_transition(
    ctx: McpToolCtx,
    args: GoalTransitionArgs,
) -> Result<GoalWriteOutput, McpToolError> {
    let transition = args.transition_kind()?;
    let prior = ctx.resolve_goal(&args.goal)?;
    let request_id = IdempotencyKey::optional_or_generated("goal_transition", args.idempotency_key)
        .map_err(McpToolError::InvalidInput)?;
    let engine = ctx.require_engine()?;
    let outcome = engine
        .transition_goal(
            &ctx.authz,
            &GoalTransitionRequest {
                owner: ctx.owner,
                prior_goal_id: prior,
                transition,
                authorship: GoalAuthorship::User,
                request_id,
                author_self_perspective_id: ctx.caller_self_perspective,
            },
        )
        .await
        .map_err(McpToolError::Protocol)?;
    Ok(format_goal_write_output(&ctx, &outcome))
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GoalMarkAchievedArgs {
    #[schemars(
        description = "Goal handle `<uuid>` from the latest write on this goal; an older handle is refused, and so is a goal while a child goal is still Active or Paused."
    )]
    pub goal: String,
    #[schemars(
        length(min = 1),
        description = "Fact or Abstraction UUIDs that show completion; at least one."
    )]
    pub evidence: Vec<String>,
    #[schemars(
        description = "Optional key, 1 to 180 chars, naming this one request: same key and content replays; changed content is `idempotency_conflict`."
    )]
    pub idempotency_key: Option<String>,
}

async fn goal_mark_achieved(
    ctx: McpToolCtx,
    args: GoalMarkAchievedArgs,
) -> Result<GoalWriteOutput, McpToolError> {
    if args.evidence.is_empty() {
        return Err(McpToolError::InvalidInput(
            "evidence must contain at least one memory handle".into(),
        ));
    }
    let prior = ctx.resolve_goal(&args.goal)?;
    let evidence = resolve_evidence(&ctx, &args.evidence)?;
    let request_id =
        IdempotencyKey::optional_or_generated("goal_mark_achieved", args.idempotency_key)
            .map_err(McpToolError::InvalidInput)?;
    let engine = ctx.require_engine()?;
    let outcome = engine
        .mark_goal_achieved(
            &ctx.authz,
            &GoalMarkAchievedRequest {
                owner: ctx.owner,
                prior_goal_id: prior,
                authorship: GoalAuthorship::System(SystemOrigin::Tool {
                    tool_id: ToolId::new(protocol_action::CORE_GOAL_MARK_ACHIEVED),
                }),
                request_id,
                evidence,
                author_self_perspective_id: ctx.caller_self_perspective,
            },
        )
        .await
        .map_err(McpToolError::Protocol)?;
    Ok(format_goal_write_output(&ctx, &outcome))
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GoalModifyArgs {
    #[schemars(
        description = "Goal handle `<uuid>` from the latest write on this goal; an older handle is refused."
    )]
    pub goal: String,
    #[serde(flatten)]
    pub payload: GoalPayloadArgs,
    #[schemars(
        length(min = 1),
        description = "Replacement Fact or Abstraction handles (`<uuid>`), at least one. Omit to keep the current evidence."
    )]
    pub evidence: Option<Vec<String>>,
    #[schemars(
        description = "Optional key, 1 to 180 chars, naming this one request: same key and content replays; changed content is `idempotency_conflict`."
    )]
    pub idempotency_key: Option<String>,
}

async fn goal_modify(
    ctx: McpToolCtx,
    args: GoalModifyArgs,
) -> Result<GoalWriteOutput, McpToolError> {
    if args.evidence.as_ref().is_some_and(Vec::is_empty) {
        return Err(McpToolError::InvalidInput(
            "Goal evidence must contain at least one Fact or Abstraction handle".into(),
        ));
    }
    let prior = ctx.resolve_goal(&args.goal)?;
    let payload = encode_goal_payload(&ctx, args.payload)?;
    let evidence = match args.evidence.as_ref() {
        Some(evidence) => Some(resolve_goal_evidence_handles(&ctx, evidence)?),
        None => None,
    };
    let request_id = IdempotencyKey::optional_or_generated("goal_modify", args.idempotency_key)
        .map_err(McpToolError::InvalidInput)?;
    let engine = ctx.require_engine()?;
    let outcome = engine
        .modify_goal(
            &ctx.authz,
            &GoalModifyRequest {
                owner: ctx.owner,
                prior_goal_id: prior,
                replacement: payload,
                authorship: system_operator_authorship(&ctx, "goal_modify"),
                request_id,
                evidence,
                author_self_perspective_id: ctx.caller_self_perspective,
            },
        )
        .await
        .map_err(McpToolError::Protocol)?;
    Ok(format_goal_write_output(&ctx, &outcome))
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GoalDecomposeArgs {
    #[schemars(description = "The parent goal's current head `handle`.")]
    pub parent_goal: String,
    #[schemars(
        length(min = 1, max = 50),
        description = "Child goals to create, 1 to 50; each starts Active under the parent; the parent can be achieved or abandoned once no child is open (achieving also needs one achieved child)."
    )]
    pub children: Vec<ChildGoalInput>,
    #[schemars(
        description = "Perspective handle (`<uuid>`) the children are assigned to; not inherited from the parent. Required unless the host supplies a caller Perspective."
    )]
    pub target_perspective: Option<String>,
    #[schemars(
        description = "Key, at most 162 chars, naming this one request: same key and content replays; changed content is `idempotency_conflict`; each child's key derives from it."
    )]
    pub idempotency_key: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ChildGoalInput {
    #[serde(flatten)]
    pub payload: GoalPayloadArgs,
    #[schemars(
        length(min = 1),
        description = "Fact or Abstraction handles (`<uuid>`) that motivate this child; at least one."
    )]
    pub evidence: Vec<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct GoalDecomposeOutput {
    pub parent_goal: String,
    pub children: Vec<GoalWriteOutput>,
    pub idempotent_replay: bool,
}

async fn goal_decompose(
    ctx: McpToolCtx,
    args: GoalDecomposeArgs,
) -> Result<GoalDecomposeOutput, McpToolError> {
    // The root key is checked first, against the bound its child keys need,
    // so an over-long key is a root-key error before anything is resolved
    // or any child is built.
    let root_key = IdempotencyKey::new_bounded(
        args.idempotency_key,
        DECOMPOSE_ROOT_KEY_MAX,
        "(child keys append \":<index>\")",
    )
    .map_err(McpToolError::InvalidInput)?;
    if args.children.is_empty() {
        return Err(McpToolError::InvalidInput(
            "children must contain at least one child goal".into(),
        ));
    }
    if args.children.len() > MAX_CHILD_GOALS {
        return Err(McpToolError::InvalidInput(format!(
            "children must contain at most {MAX_CHILD_GOALS} child goals"
        )));
    }
    let parent = ctx.resolve_goal(&args.parent_goal)?;
    let assignment = target_perspective(&ctx, args.target_perspective.as_deref())?;
    let topology = GoalTopologyWrite::new(assignment, Vec::new(), Vec::new())
        .map_err(McpToolError::Protocol)?;
    let mut children = Vec::with_capacity(args.children.len());
    for (index, child) in args.children.into_iter().enumerate() {
        children.push(ChildGoalDraft {
            payload: encode_goal_payload(&ctx, child.payload)?,
            evidence: resolve_goal_evidence_handles(&ctx, &child.evidence)?,
            // Cannot fail: the root bound leaves room for every child key.
            request_id: root_key
                .child(DECOMPOSE_CHILD_PREFIX, index)
                .map_err(|reason| {
                    McpToolError::Other(format!("decompose child key invariant: {reason}"))
                })?,
        });
    }
    let engine = ctx.require_engine()?;
    let outcome = engine
        .decompose_goal(
            &ctx.authz,
            &GoalDecomposeRequest {
                owner: ctx.owner,
                parent_goal_id: parent,
                authorship: system_operator_authorship(&ctx, "goal_decompose"),
                topology,
                children,
                author_self_perspective_id: ctx.caller_self_perspective,
            },
        )
        .await
        .map_err(McpToolError::Protocol)?;
    Ok(GoalDecomposeOutput {
        parent_goal: parent.into_inner().to_string(),
        children: outcome
            .children
            .into_iter()
            .map(|child| format_goal_write_output(&ctx, &child.outcome))
            .collect(),
        idempotent_replay: outcome.idempotent_replay,
    })
}

pub(crate) fn encode_goal_payload(
    ctx: &McpToolCtx,
    args: GoalPayloadArgs,
) -> Result<GoalPayloadWrite, McpToolError> {
    let title = validate_trimmed_len("goal title", &args.title, 240)?;
    let text = validate_trimmed_len("goal text", &args.text, 20_000)?;
    let schema_id = SchemaId::new(args.schema_id);
    let schema_version = SchemaVersion::new(args.schema_version.unwrap_or(1));
    let schema = ctx
        .registry
        .lookup_payload(&schema_id, schema_version, PayloadKind::Goal)
        .ok_or_else(|| {
            McpToolError::InvalidInput(format!(
                "unregistered GoalPayload schema {} v{}",
                schema_id.as_str(),
                schema_version.into_inner(),
            ))
        })?;
    if !args.body.is_object() {
        return Err(McpToolError::InvalidInput(format!(
            "body for GoalPayload schema {} v{} must be a JSON object",
            schema_id.as_str(),
            schema_version.into_inner(),
        )));
    }
    let protocol_payload = ctx
        .registry
        .ingest_protocol_payload(&schema_id, schema_version, PayloadKind::Goal, &args.body)
        .map_err(McpToolError::InvalidInput)?;
    let sidecar_payload = schema
        .sidecar_table
        .is_some()
        .then_some(protocol_payload.sidecar_payload);
    let payload_bytes = protocol_payload.key_bytes.ok_or_else(|| {
        McpToolError::InvalidInput(format!(
            "GoalPayload schema {} v{} did not produce key bytes",
            schema_id.as_str(),
            schema_version.into_inner(),
        ))
    })?;
    Ok(GoalPayloadWrite {
        schema_id,
        schema_version,
        title: title.to_string(),
        text: text.to_string(),
        payload: payload_bytes,
        sidecar_payload,
    })
}

fn resolve_evidence(
    ctx: &McpToolCtx,
    evidence: &[String],
) -> Result<Vec<GoalEvidenceRef>, McpToolError> {
    evidence
        .iter()
        .map(|handle| ctx.resolve_memory(handle).map(GoalEvidenceRef::new))
        .collect()
}

/// Parse Goal evidence handles (bare UUIDs, at least one); storage checks each is a live owned Fact or Abstraction in the write transaction.
pub(crate) fn resolve_goal_evidence_handles(
    ctx: &McpToolCtx,
    evidence: &[String],
) -> Result<Vec<GoalEvidenceRef>, McpToolError> {
    if evidence.is_empty() {
        return Err(McpToolError::InvalidInput(
            "Goal evidence must contain at least one Fact or Abstraction handle".into(),
        ));
    }
    resolve_evidence(ctx, evidence)
}

fn target_perspective(
    ctx: &McpToolCtx,
    target_perspective: Option<&str>,
) -> Result<GoalAssignmentTarget, McpToolError> {
    match target_perspective {
        Some(raw) => ctx
            .resolve_memory(raw)
            .map(GoalAssignmentTarget::perspective),
        None => ctx
            .caller_self_perspective
            .map(GoalAssignmentTarget::perspective)
            .ok_or_else(|| {
                McpToolError::InvalidInput(
                    "target_perspective is required: name the Perspective UUID to assign to".into(),
                )
            }),
    }
}

/// Operator identity for a Goal write.
///
/// Deliberately *not* routed through
/// [`operator_label`](super::memory::util::operator_label), unlike every
/// authoring path that records a label. Nothing here is persisted as a
/// label: `SystemOrigin::Operator` has no goal-authorship column, and
/// replay collapses it to `system_operator`, so the only thing the label
/// reaches is the v5 hash below. Validating it would buy no provenance and
/// would turn a blank or over-long request-context `model_id` into a
/// refused `goal_set` — a caller told its intent was rejected over a field
/// the goal never keeps.
///
/// The bound identity already reaches this: the edge resolves
/// `ctx.author.model_id` to the trusted id when the token binds one.
///
/// The input-contract label (`fact-or-abstraction-evidence-v1`, #513) feeds
/// only that hash: no stored column carries it, so renaming it changes no
/// replay.
pub(crate) fn system_operator_authorship(ctx: &McpToolCtx, prompt_version: &str) -> GoalAuthorship {
    let model_id = &ctx.author.model_id;
    let operator_key = format!(
        "{}\0{}\0{}\0{}",
        ctx.author.client_name, ctx.author.client_version, model_id, prompt_version
    );
    let operator_id = uuid::Uuid::new_v5(&MCP_OPERATOR_NAMESPACE, operator_key.as_bytes());
    let input_contract_id = uuid::Uuid::new_v5(
        &MCP_GOAL_INPUT_CONTRACT_NAMESPACE,
        format!(
            "{}:{prompt_version}:fact-or-abstraction-evidence-v1",
            protocol_tool::CORE_GOAL
        )
        .as_bytes(),
    );
    GoalAuthorship::System(SystemOrigin::Operator {
        operator_id: OperatorId::new(operator_id),
        operator_kind: OperatorKind::AtoGoal,
        input_contract_id: InputContractId::new(input_contract_id),
        model_id: ModelId::new(model_id.clone()),
        prompt_version: PromptVersion::new(prompt_version),
    })
}

fn format_goal_write_output(_ctx: &McpToolCtx, outcome: &GoalWriteOutcome) -> GoalWriteOutput {
    GoalWriteOutput {
        handle: outcome.goal_id.into_inner().to_string(),
        edge_count: outcome.edge_count,
        idempotent_replay: outcome.idempotent_replay,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::MembershipStorage;
    use crate::mcp::McpAuthorContext;
    use crate::{
        AuthPath, AuthzContext, FlavorRegistry, FlavorServices, MemoryId, OwnerId, UserId,
    };
    use std::sync::Arc;

    fn test_ctx() -> McpToolCtx {
        let owner = OwnerId::from(UserId::new(uuid::Uuid::now_v7()));
        McpToolCtx {
            owner,
            authz: AuthzContext::single_owner(&owner, AuthPath::HostBearer),
            registry: Arc::new(FlavorRegistry::new().freeze_or_panic_for_tests()),
            author: McpAuthorContext {
                model_id: "m".into(),
                trusted_model_id: None,
                client_name: "c".into(),
                client_version: "0".into(),
                caller_self_perspective: None,
            },
            caller_self_perspective: None,
            services: FlavorServices::default(),
            engine: None,
        }
    }

    fn operator_recording_ctx() -> (
        McpToolCtx,
        std::sync::Arc<std::sync::Mutex<Vec<GoalAuthorship>>>,
        MemoryId,
    ) {
        let owner = OwnerId::from(UserId::new(uuid::Uuid::now_v7()));
        let target = MemoryId::new(uuid::Uuid::now_v7());
        let observed = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let storage = MembershipStorage {
            observed_entity_reads: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            observed_kind_loads: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            member: owner,
            group: crate::GroupId::new(uuid::Uuid::now_v7()),
            membership_relation: crate::Relation::Viewer,
            home_owner: Some(owner),
            entity_readable: true,
            memory_kind: Some(crate::EntityKind::Perspective),
            goal_evidence: None,
            observed_fact_writes: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            observed_modify_evidence: std::sync::Arc::new(std::sync::Mutex::new(None)),
            observed_goal_authorship: observed.clone(),
        };
        let engine = std::sync::Arc::new(crate::Engine::compose_or_panic_for_tests(
            storage.storage_ports(),
            |_| {},
        ));
        let authz = crate::AuthzContext::single_owner(&owner, crate::AuthPath::HostBearer);
        let ctx = McpToolCtx {
            owner,
            authz,
            registry: std::sync::Arc::new(crate::FlavorRegistry::new().freeze_or_panic_for_tests()),
            author: McpAuthorContext {
                model_id: "test-model".into(),
                trusted_model_id: None,
                client_name: "test-client".into(),
                client_version: "1".into(),
                caller_self_perspective: Some(target),
            },
            caller_self_perspective: Some(target),
            services: crate::FlavorServices::default(),
            engine: Some(engine),
        };
        (ctx, observed, target)
    }

    fn valid_goal_payload_args() -> GoalPayloadArgs {
        GoalPayloadArgs {
            schema_id: "core/simple-text-v1".into(),
            schema_version: None,
            title: "test goal".into(),
            text: "test goal body".into(),
            body: serde_json::json!({}),
        }
    }

    #[tokio::test]
    async fn direct_goal_actions_forward_operator_authorship() {
        let (ctx, observed, target) = operator_recording_ctx();
        let abstraction = format!("{}", uuid::Uuid::now_v7());
        let perspective = target.into_inner().to_string();

        let set_error = goal_set(
            ctx.clone(),
            GoalSetArgs {
                payload: valid_goal_payload_args(),
                evidence: vec![abstraction.clone()],
                target_perspective: Some(perspective.clone()),
                idempotency_key: Some("operator-set-authorship".into()),
            },
        )
        .await
        .expect_err("recording storage rejects the test write after capture");
        assert!(
            matches!(set_error, McpToolError::Protocol(ref error) if error.code == crate::error::ErrorCode::Internal)
        );

        let modify_error = goal_modify(
            ctx.clone(),
            GoalModifyArgs {
                goal: (crate::GoalId::new(uuid::Uuid::now_v7()))
                    .into_inner()
                    .to_string(),
                payload: valid_goal_payload_args(),
                evidence: Some(vec![abstraction.clone()]),
                idempotency_key: Some("operator-modify-authorship".into()),
            },
        )
        .await
        .expect_err("recording storage rejects the test write after capture");
        assert!(
            matches!(modify_error, McpToolError::Protocol(ref error) if error.code == crate::error::ErrorCode::Internal)
        );

        let decompose_error = goal_decompose(
            ctx.clone(),
            GoalDecomposeArgs {
                parent_goal: (crate::GoalId::new(uuid::Uuid::now_v7()))
                    .into_inner()
                    .to_string(),
                children: vec![ChildGoalInput {
                    payload: valid_goal_payload_args(),
                    evidence: vec![abstraction],
                }],
                target_perspective: Some(perspective),
                idempotency_key: "operator-decompose-authorship".into(),
            },
        )
        .await
        .expect_err("recording storage rejects the test write after capture");
        assert!(
            matches!(decompose_error, McpToolError::Protocol(ref error) if error.code == crate::error::ErrorCode::Internal)
        );

        let authorships = observed
            .lock()
            .expect("goal authorship recorder lock")
            .clone();
        let prompts = authorships
            .iter()
            .map(|authorship| match authorship {
                GoalAuthorship::System(SystemOrigin::Operator { prompt_version, .. }) => {
                    prompt_version.as_str()
                }
                other => panic!("expected operator authorship, got {other:?}"),
            })
            .collect::<Vec<_>>();
        assert_eq!(
            prompts,
            vec!["goal_set", "goal_modify", "goal_decompose"],
            "direct MCP Goal actions must retain their operator provenance"
        );
    }

    /// `children` valid children under a fresh parent, keyed by `root`;
    /// child 0 gets an unresolvable evidence handle when `bad_first`.
    fn decompose_args(
        root: String,
        children: usize,
        bad_first: bool,
        target: MemoryId,
    ) -> GoalDecomposeArgs {
        GoalDecomposeArgs {
            parent_goal: uuid::Uuid::now_v7().to_string(),
            children: (0..children)
                .map(|index| ChildGoalInput {
                    payload: valid_goal_payload_args(),
                    evidence: vec![if bad_first && index == 0 {
                        format!("F:{}", uuid::Uuid::now_v7())
                    } else {
                        uuid::Uuid::now_v7().to_string()
                    }],
                })
                .collect(),
            target_perspective: Some(target.into_inner().to_string()),
            idempotency_key: root,
        }
    }

    /// #479 item 5: a root key one char over the documented bound is refused
    /// up front with a root-key error, before any child is built: not on a
    /// child's evidence, not on a child key at index 10, and not silently
    /// accepted when few children keep every child key short enough.
    #[tokio::test]
    async fn decompose_root_key_one_over_bound_is_refused_before_children() {
        let (ctx, observed, target) = operator_recording_ctx();
        for (children, bad_first) in [(50, true), (50, false), (1, true), (1, false)] {
            let err = goal_decompose(
                ctx.clone(),
                decompose_args("k".repeat(163), children, bad_first, target),
            )
            .await
            .expect_err("an over-long root key is refused");
            assert!(
                matches!(err, McpToolError::InvalidInput(ref message)
                    if message.starts_with("idempotency_key must be at most 162 chars")),
                "{children} children, bad first evidence {bad_first}: {err:?}"
            );
        }
        assert!(
            observed
                .lock()
                .expect("goal authorship recorder lock")
                .is_empty(),
            "no decompose reached the engine"
        );
    }

    #[test]
    fn decompose_root_bound_is_derived() {
        assert_eq!(DECOMPOSE_ROOT_KEY_MAX, 162);
        assert_eq!(decimal_digits(0), 1);
        assert_eq!(decimal_digits(49), 2);
        assert_eq!(decimal_digits(100), 3);
        let schema = serde_json::to_value(schemars::schema_for!(GoalDecomposeArgs))
            .expect("schema serializes");
        let description = schema["properties"]["idempotency_key"]["description"]
            .as_str()
            .expect("the root key is described");
        assert!(
            description.contains(&format!("at most {DECOMPOSE_ROOT_KEY_MAX} chars")),
            "the advertised bound is the enforced one: {description}"
        );
    }

    /// The bound is exact: a 162-char root with the maximum number of
    /// children builds every child key and reaches the engine.
    #[tokio::test]
    async fn decompose_root_key_at_bound_with_max_children_is_accepted() {
        let (ctx, observed, target) = operator_recording_ctx();
        let err = goal_decompose(
            ctx,
            decompose_args("k".repeat(162), MAX_CHILD_GOALS, false, target),
        )
        .await
        .expect_err("recording storage rejects the test write after capture");
        assert!(
            matches!(err, McpToolError::Protocol(ref error) if error.code == crate::error::ErrorCode::Internal),
            "{err:?}"
        );
        assert_eq!(
            observed
                .lock()
                .expect("goal authorship recorder lock")
                .len(),
            1,
            "all 50 child keys were built and the decompose reached storage"
        );
    }

    fn goal_set_args(evidence: Vec<String>) -> GoalSetArgs {
        GoalSetArgs {
            payload: GoalPayloadArgs {
                schema_id: "bogus/goal".into(),
                schema_version: None,
                title: "t".into(),
                text: "b".into(),
                body: serde_json::json!({}),
            },
            evidence,
            target_perspective: None,
            idempotency_key: None,
        }
    }

    #[tokio::test]
    async fn goal_set_rejects_empty_evidence() {
        let err = goal_set(test_ctx(), goal_set_args(Vec::new()))
            .await
            .expect_err("empty evidence must be rejected");
        assert!(
            matches!(err, McpToolError::InvalidInput(ref m) if m.contains("requires >=1")),
            "got {err:?}",
        );
    }

    #[tokio::test]
    async fn goal_modify_rejects_explicit_empty_evidence_before_engine() {
        let err = goal_modify(
            test_ctx(),
            GoalModifyArgs {
                goal: format!("{}", uuid::Uuid::now_v7()),
                payload: GoalPayloadArgs {
                    schema_id: "bogus/goal".into(),
                    schema_version: None,
                    title: "t".into(),
                    text: "b".into(),
                    body: serde_json::json!({}),
                },
                evidence: Some(Vec::new()),
                idempotency_key: None,
            },
        )
        .await
        .expect_err("explicit empty operator evidence must fail at the MCP boundary");
        assert!(
            matches!(err, McpToolError::InvalidInput(ref message) if message.contains("at least one")),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn goal_set_accepts_nonempty_evidence_before_payload_resolution() {
        // With evidence present the empty-evidence guard must pass; the call
        // then fails later on the unregistered schema, proving the guard
        // only fires on empty evidence (a Fact or Abstraction handle is accepted).
        let err = goal_set(
            test_ctx(),
            goal_set_args(vec![format!("{}", uuid::Uuid::now_v7())]),
        )
        .await
        .expect_err("bogus schema still fails later");
        assert!(
            !matches!(err, McpToolError::InvalidInput(ref m) if m.contains("requires >=1")),
            "guard must not fire for non-empty evidence: {err:?}",
        );
        assert!(
            matches!(err, McpToolError::InvalidInput(ref m) if m.contains("unregistered GoalPayload")),
            "expected subsequent schema error, got {err:?}",
        );
    }

    #[test]
    fn goal_evidence_rejects_prefixed_ids_before_storage() {
        let ctx = test_ctx();
        for prefix in ['F', 'A', 'P', 'G'] {
            let raw = format!("{prefix}:{}", uuid::Uuid::now_v7());
            let err = resolve_goal_evidence_handles(&ctx, &[raw]).expect_err("bare UUID required");
            assert!(matches!(err, McpToolError::InvalidInput(_)), "{err}");
        }
    }

    #[test]
    fn goal_evidence_rejects_empty_input() {
        let err = resolve_goal_evidence_handles(&test_ctx(), &[])
            .expect_err("operator Goal evidence must be nonempty");
        assert!(
            matches!(err, McpToolError::InvalidInput(ref message) if message.contains("at least one")),
            "got {err:?}"
        );
    }
}
