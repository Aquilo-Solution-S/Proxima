use crate::engine::{
    GoalCreatePayloadWriteRequest, GoalDecomposeRequest, GoalMarkAchievedRequest,
    GoalModifyRequest, GoalTransitionRequest,
};
use crate::mcp::{CoreActionMeta, McpActionArgSpec, McpTool, McpToolCtx, McpToolError};
use crate::protocol::{action as protocol_action, tool as protocol_tool};
use crate::tool::validate_trimmed_len;
use crate::verbs::goal_write::{
    ChildGoalDraft, GoalAssignmentTarget, GoalAuthorship, GoalEvidenceRef, GoalPayloadWrite,
    GoalState, GoalTopologyWrite, GoalWakeConfigWrite, GoalWakeToolId, GoalWakeTrigger,
    GoalWriteOutcome, IdempotencyKey, OperatorKind, SystemOrigin,
};
use crate::verbs::schema::PayloadKind;
use crate::{InputContractId, ModelId, OperatorId, PromptVersion, SchemaId, SchemaVersion, ToolId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{WRITE_IDEMPOTENT, WRITE_NON_IDEMPOTENT};

const MAX_CHILD_GOALS: usize = 50;
const MCP_OPERATOR_NAMESPACE: uuid::Uuid = uuid::Uuid::from_bytes([
    0x3f, 0x61, 0xde, 0x85, 0x4e, 0x09, 0x45, 0x62, 0x97, 0xc4, 0x8a, 0x74, 0xaa, 0xf9, 0x4a, 0x2c,
]);
const MCP_GOAL_INPUT_CONTRACT_NAMESPACE: uuid::Uuid = uuid::Uuid::from_bytes([
    0xb2, 0x3f, 0x07, 0xef, 0xeb, 0x6f, 0x4f, 0xe7, 0xa9, 0xd3, 0x67, 0xa9, 0x53, 0xf3, 0x4a, 0x6d,
]);
const GOAL_ACTIVATED_SCHEMA_IDS: &[&str] =
    &[<crate::GoalActivatedV1 as crate::FactPayload>::SCHEMA_ID];
const GOAL_ACHIEVED_SCHEMA_IDS: &[&str] =
    &[<crate::GoalAchievedV1 as crate::FactPayload>::SCHEMA_ID];
const CORE_GOAL_PRODUCES_SCHEMA_IDS: &[&str] = &[
    <crate::GoalActivatedV1 as crate::FactPayload>::SCHEMA_ID,
    <crate::GoalAchievedV1 as crate::FactPayload>::SCHEMA_ID,
];
pub const CORE_GOAL_ACTIONS: &[CoreActionMeta] = &[
    CoreActionMeta {
        tool: CoreGoalTool::NAME,
        action: "set",
        scope_key: protocol_action::CORE_GOAL_SET,
        description: "Set an Active Goal assigned to a Perspective.",
        produces_schema_ids: GOAL_ACTIVATED_SCHEMA_IDS,
        annotations: WRITE_NON_IDEMPOTENT,
    },
    CoreActionMeta {
        tool: CoreGoalTool::NAME,
        action: "transition",
        scope_key: protocol_action::CORE_GOAL_TRANSITION,
        description: "Pause, resume, or abandon a Goal head.",
        produces_schema_ids: &[],
        annotations: WRITE_NON_IDEMPOTENT,
    },
    CoreActionMeta {
        tool: CoreGoalTool::NAME,
        action: "modify",
        scope_key: protocol_action::CORE_GOAL_MODIFY,
        description: "Replace an Active Goal head's content.",
        produces_schema_ids: GOAL_ACTIVATED_SCHEMA_IDS,
        annotations: WRITE_NON_IDEMPOTENT,
    },
    CoreActionMeta {
        tool: CoreGoalTool::NAME,
        action: "mark_achieved",
        scope_key: protocol_action::CORE_GOAL_MARK_ACHIEVED,
        description: "Mark a Goal head Achieved with completion evidence.",
        produces_schema_ids: GOAL_ACHIEVED_SCHEMA_IDS,
        annotations: WRITE_NON_IDEMPOTENT,
    },
    CoreActionMeta {
        tool: CoreGoalTool::NAME,
        action: "decompose",
        scope_key: protocol_action::CORE_GOAL_DECOMPOSE,
        description: "Create Active child Goals under a parent Goal.",
        produces_schema_ids: GOAL_ACTIVATED_SCHEMA_IDS,
        annotations: WRITE_IDEMPOTENT,
    },
];

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GoalPayloadArgs {
    #[schemars(
        description = "Registered Goal-payload schema id (PayloadKind::Goal). Discover valid ids with the `proxima://schemas{?kind}` resource (kind=Goal)."
    )]
    pub schema_id: String,
    #[schemars(description = "Goal-payload schema version. Omit to default to 1.")]
    pub schema_version: Option<u32>,
    #[schemars(
        length(max = 240),
        description = "Short, human-readable goal title, 1 to 240 chars. Leading and trailing whitespace is removed before the length check."
    )]
    pub title: String,
    #[schemars(
        length(max = 20000),
        description = "The goal stated in prose, 1 to 20000 chars — what pursuing or achieving it means. Leading and trailing whitespace is removed before the length check."
    )]
    pub text: String,
    #[serde(default = "default_empty_object")]
    #[schemars(
        with = "std::collections::BTreeMap<String, serde_json::Value>",
        description = "Structured goal payload conforming to `schema_id`@`schema_version`; must be a JSON object. Omit for `{}`."
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
        description = "Required Fact or Abstraction memory handles (`F...`/`A...`) that motivate this operator-authored goal; at least one is required."
    )]
    pub evidence: Vec<String>,
    #[schemars(
        description = "Optional Perspective memory handle to assign the goal to; omit to use the caller Perspective context."
    )]
    pub target_perspective: Option<String>,
    #[schemars(
        description = "Optional wake config arming this goal: a trigger Fact/Fact-schema, a wake prompt, and a toolset. Armed goals surface on `proxima://wake-candidates` when a matching Fact is appended."
    )]
    pub wake: Option<GoalWakeArgs>,
    #[schemars(
        description = "Optional stable idempotency key so a replayed call is a no-op, not a duplicate goal."
    )]
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GoalWakeArgs {
    #[schemars(
        description = "Fact memory handle (`F...`) whose exact row is the wake trigger. Exactly one of `trigger_fact` / `trigger_schema_id` is required."
    )]
    pub trigger_fact: Option<String>,
    #[schemars(
        description = "Registered Fact schema id; any appended Fact of this schema wakes the goal. Exactly one of `trigger_fact` / `trigger_schema_id` is required."
    )]
    pub trigger_schema_id: Option<String>,
    #[schemars(description = "Fact schema version for `trigger_schema_id`. Omit to default to 1.")]
    pub trigger_schema_version: Option<u32>,
    #[schemars(
        description = "Registered tool or `tool:action` leaf ids the woken run may use (e.g. `core_search_memories`, `core_goal:set`); at least one is required."
    )]
    pub tool_ids: Vec<String>,
    #[schemars(description = "Wake prompt handed to the external harness, 1 to 20000 chars.")]
    pub prompt: String,
    #[schemars(
        description = "Optional memory handles pinned as required readable context for the woken run."
    )]
    pub hard_memories: Option<Vec<String>>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct GoalWriteOutput {
    pub handle: String,
    pub lifecycle_memory: Option<String>,
    /// Index rows the write asserted. Not handles: an edge has no id.
    pub edge_count: usize,
    pub idempotent_replay: bool,
}

#[derive(Debug)]
pub struct CoreGoalTool;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum CoreGoalArgs {
    Set(GoalSetArgs),
    Transition(GoalTransitionArgs),
    Modify(GoalModifyArgs),
    MarkAchieved(GoalMarkAchievedArgs),
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
    const DESCRIPTION: &'static str =
        "Goal write dispatcher — set/transition/modify/mark_achieved/decompose.";
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
                "wake",
                "idempotency_key",
            ],
            required_fields: &["schema_id", "title", "text", "evidence"],
        },
        McpActionArgSpec {
            action: "transition",
            allowed_fields: &["goal", "transition", "idempotency_key"],
            required_fields: &["goal", "transition"],
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
                "wake",
                "clear_wake",
                "idempotency_key",
            ],
            required_fields: &["goal", "schema_id", "title", "text"],
        },
        McpActionArgSpec {
            action: "mark_achieved",
            allowed_fields: &["goal", "evidence", "idempotency_key"],
            required_fields: &["goal", "evidence"],
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
            "goal set requires >=1 Fact|Abstraction evidence handle motivating the goal".into(),
        ));
    }
    let payload = encode_goal_payload(&ctx, args.payload)?;
    let evidence = resolve_evidence(&ctx, &args.evidence)?;
    let assignment = target_perspective(&ctx, args.target_perspective.as_deref())?;
    let wake = args
        .wake
        .map(|wake| encode_wake_config(&ctx, wake))
        .transpose()?;
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
                wake,
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
        description = "Goal handle to transition (the `handle` returned by `core_goal` action=set/decompose)."
    )]
    pub goal: String,
    #[schemars(description = "Lifecycle transition to apply: `pause`, `resume`, or `abandon`.")]
    pub transition: GoalTransition,
    #[schemars(description = "Optional stable idempotency key for replay-safe transitions.")]
    pub idempotency_key: Option<String>,
}

async fn goal_transition(
    ctx: McpToolCtx,
    args: GoalTransitionArgs,
) -> Result<GoalWriteOutput, McpToolError> {
    let prior = ctx.resolve_goal(&args.goal)?;
    let next_state = match args.transition {
        GoalTransition::Pause => GoalState::Paused,
        GoalTransition::Resume => GoalState::Active,
        GoalTransition::Abandon => GoalState::Abandoned,
    };
    let request_id = IdempotencyKey::optional_or_generated("goal_transition", args.idempotency_key)
        .map_err(McpToolError::InvalidInput)?;
    let engine = ctx.require_engine()?;
    let outcome = engine
        .transition_goal(
            &ctx.authz,
            &GoalTransitionRequest {
                owner: ctx.owner,
                prior_goal_id: prior,
                next_state,
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
        description = "Goal handle to mark Achieved (from `core_goal` action=set/decompose)."
    )]
    pub goal: String,
    #[schemars(
        description = "Memory handles (`F.../A.../P...`) evidencing completion; at least one is required."
    )]
    pub evidence: Vec<String>,
    #[schemars(description = "Optional stable idempotency key for replay-safe completion.")]
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
        description = "Goal handle whose Active head is replaced (from `core_goal` action=set/decompose)."
    )]
    pub goal: String,
    #[serde(flatten)]
    pub payload: GoalPayloadArgs,
    #[schemars(
        description = "Fact or Abstraction evidence handles (`F...`/`A...`) for the operator-authored modified goal head."
    )]
    pub evidence: Option<Vec<String>>,
    #[schemars(
        description = "Optional replacement wake config for the new goal head. Omit to carry the prior head's wake config forward; mutually exclusive with `clear_wake`."
    )]
    pub wake: Option<GoalWakeArgs>,
    #[serde(default)]
    #[schemars(
        description = "Set true to disarm the goal: the new head carries no wake config. Mutually exclusive with `wake`."
    )]
    pub clear_wake: bool,
    #[schemars(description = "Optional stable idempotency key for replay-safe modification.")]
    pub idempotency_key: Option<String>,
}

async fn goal_modify(
    ctx: McpToolCtx,
    args: GoalModifyArgs,
) -> Result<GoalWriteOutput, McpToolError> {
    let prior = ctx.resolve_goal(&args.goal)?;
    let payload = encode_goal_payload(&ctx, args.payload)?;
    let evidence = args
        .evidence
        .as_ref()
        .map(|evidence| resolve_evidence(&ctx, evidence))
        .transpose()?;
    let wake = match (args.wake, args.clear_wake) {
        (Some(_), true) => {
            return Err(McpToolError::InvalidInput(
                "wake and clear_wake are mutually exclusive".into(),
            ));
        }
        (Some(wake), false) => Some(Some(encode_wake_config(&ctx, wake)?)),
        (None, true) => Some(None),
        (None, false) => None,
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
                wake,
                authorship: GoalAuthorship::User,
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
    #[schemars(
        description = "Handle of the parent goal the children attach under (from `core_goal` action=set)."
    )]
    pub parent_goal: String,
    #[schemars(
        length(max = 50),
        description = "Child goals to create (1 to 50); each is set Active and linked to the parent."
    )]
    pub children: Vec<ChildGoalInput>,
    #[schemars(
        description = "Optional Perspective memory handle to assign children to; omit to use the caller Perspective context."
    )]
    pub target_perspective: Option<String>,
    #[schemars(
        description = "Required stable idempotency key; each child's key derives from it deterministically, so replays are no-ops."
    )]
    pub idempotency_key: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ChildGoalInput {
    #[serde(flatten)]
    pub payload: GoalPayloadArgs,
    #[serde(default)]
    #[schemars(
        description = "Required Fact or Abstraction memory handles (`F...`/`A...`) that motivate this operator-authored child goal."
    )]
    pub evidence: Vec<String>,
    #[schemars(
        description = "Optional wake config arming this child goal (see `core_goal` action=set `wake`)."
    )]
    pub wake: Option<GoalWakeArgs>,
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
    let root_key = IdempotencyKey::new(args.idempotency_key).map_err(McpToolError::InvalidInput)?;
    let mut children = Vec::with_capacity(args.children.len());
    for (index, child) in args.children.into_iter().enumerate() {
        children.push(ChildGoalDraft {
            payload: encode_goal_payload(&ctx, child.payload)?,
            evidence: resolve_evidence(&ctx, &child.evidence)?,
            wake: child
                .wake
                .map(|wake| encode_wake_config(&ctx, wake))
                .transpose()?,
            request_id: root_key
                .child("goal_decompose", index)
                .map_err(McpToolError::InvalidInput)?,
        });
    }
    let engine = ctx.require_engine()?;
    let outcome = engine
        .decompose_goal(
            &ctx.authz,
            &GoalDecomposeRequest {
                owner: ctx.owner,
                parent_goal_id: parent,
                authorship: GoalAuthorship::System(SystemOrigin::Tool {
                    tool_id: ToolId::new(protocol_action::CORE_GOAL_DECOMPOSE),
                }),
                topology,
                children,
                author_self_perspective_id: ctx.caller_self_perspective,
            },
        )
        .await
        .map_err(McpToolError::Protocol)?;
    Ok(GoalDecomposeOutput {
        parent_goal: ctx.format_goal(parent),
        children: outcome
            .children
            .into_iter()
            .map(|child| format_goal_write_output(&ctx, &child.outcome))
            .collect(),
        idempotent_replay: outcome.idempotent_replay,
    })
}

fn encode_goal_payload(
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

fn encode_wake_config(
    ctx: &McpToolCtx,
    args: GoalWakeArgs,
) -> Result<GoalWakeConfigWrite, McpToolError> {
    let trigger = match (args.trigger_fact.as_deref(), args.trigger_schema_id) {
        (Some(_), Some(_)) | (None, None) => {
            return Err(McpToolError::InvalidInput(
                "wake requires exactly one of trigger_fact or trigger_schema_id".into(),
            ));
        }
        (Some(fact), None) => {
            if args.trigger_schema_version.is_some() {
                return Err(McpToolError::InvalidInput(
                    "trigger_schema_version requires trigger_schema_id".into(),
                ));
            }
            GoalWakeTrigger::FactMemory {
                memory_id: ctx.resolve_fact_memory(fact)?,
            }
        }
        (None, Some(schema_id)) => GoalWakeTrigger::FactSchema {
            schema_id: SchemaId::new(schema_id),
            schema_version: SchemaVersion::new(args.trigger_schema_version.unwrap_or(1)),
        },
    };
    let tool_ids = args
        .tool_ids
        .iter()
        .map(|raw| GoalWakeToolId::parse(raw.as_str(), &ctx.registry))
        .collect::<Result<Vec<_>, _>>()
        .map_err(McpToolError::Protocol)?;
    let hard_memory_ids = args
        .hard_memories
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(|handle| ctx.resolve_memory(handle))
        .collect::<Result<Vec<_>, _>>()?;
    // Checked here rather than left to `GoalWakeConfigWrite::new`, which
    // enforces the same bound for embedding hosts but cannot name which end
    // of it was broken. Its check stays as the defensive one.
    let prompt = validate_trimmed_len(
        "wake prompt",
        &args.prompt,
        GoalWakeConfigWrite::MAX_PROMPT_CHARS,
    )?;
    GoalWakeConfigWrite::new(trigger, tool_ids, prompt, &hard_memory_ids)
        .map_err(McpToolError::Protocol)
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

fn target_perspective(
    ctx: &McpToolCtx,
    target_perspective: Option<&str>,
) -> Result<GoalAssignmentTarget, McpToolError> {
    match target_perspective {
        Some(handle) => ctx
            .resolve_perspective_memory(handle)
            .map(GoalAssignmentTarget::perspective),
        None => ctx
            .caller_self_perspective
            .map(GoalAssignmentTarget::perspective)
            .ok_or_else(|| {
                McpToolError::InvalidInput(
                    "target_perspective or caller Perspective context is required".into(),
                )
            }),
    }
}

fn system_operator_authorship(ctx: &McpToolCtx, prompt_version: &str) -> GoalAuthorship {
    let operator_key = format!(
        "{}\0{}\0{}\0{}",
        ctx.author.client_name, ctx.author.client_version, ctx.author.model_id, prompt_version
    );
    let operator_id = uuid::Uuid::new_v5(&MCP_OPERATOR_NAMESPACE, operator_key.as_bytes());
    let input_contract_id = uuid::Uuid::new_v5(
        &MCP_GOAL_INPUT_CONTRACT_NAMESPACE,
        format!(
            "{}:{prompt_version}:abstraction-evidence-v1",
            protocol_tool::CORE_GOAL
        )
        .as_bytes(),
    );
    GoalAuthorship::System(SystemOrigin::Operator {
        operator_id: OperatorId::new(operator_id),
        operator_kind: OperatorKind::AtoGoal,
        input_contract_id: InputContractId::new(input_contract_id),
        model_id: ModelId::new(ctx.author.model_id.clone()),
        prompt_version: PromptVersion::new(prompt_version),
    })
}

fn format_goal_write_output(ctx: &McpToolCtx, outcome: &GoalWriteOutcome) -> GoalWriteOutput {
    GoalWriteOutput {
        handle: ctx.format_goal(outcome.goal_id),
        lifecycle_memory: outcome
            .lifecycle_memory_id
            .map(|memory_id| ctx.format_fact_memory(memory_id)),
        edge_count: outcome.edge_count,
        idempotent_replay: outcome.idempotent_replay,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::{McpAuthorContext, McpToolExtensions};
    use crate::{AuthPath, AuthzContext, FlavorRegistry, OwnerRef, UserId};
    use std::sync::Arc;

    fn test_ctx() -> McpToolCtx {
        let owner = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));
        McpToolCtx {
            owner,
            authz: AuthzContext::single_owner(&owner, AuthPath::HostBearer),
            registry: Arc::new(FlavorRegistry::new().freeze_or_panic_for_tests()),
            author: McpAuthorContext {
                model_id: "m".into(),
                client_name: "c".into(),
                client_version: "0".into(),
                caller_self_perspective: None,
            },
            caller_self_perspective: None,
            extensions: McpToolExtensions::default(),
            engine: None,
        }
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
            wake: None,
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
    async fn goal_set_accepts_nonempty_evidence_before_payload_resolution() {
        // With evidence present the empty-evidence guard must pass; the call
        // then fails downstream on the unregistered schema, proving the guard
        // only fires on empty evidence (a Fact/Abstraction handle is accepted).
        let err = goal_set(
            test_ctx(),
            goal_set_args(vec![format!("A:{}", uuid::Uuid::now_v7())]),
        )
        .await
        .expect_err("bogus schema still fails downstream");
        assert!(
            !matches!(err, McpToolError::InvalidInput(ref m) if m.contains("requires >=1")),
            "guard must not fire for non-empty evidence: {err:?}",
        );
        assert!(
            matches!(err, McpToolError::InvalidInput(ref m) if m.contains("unregistered GoalPayload")),
            "expected downstream schema error, got {err:?}",
        );
    }

    fn wake_args(
        trigger_fact: Option<&str>,
        trigger_schema_id: Option<&str>,
        tool_ids: Vec<String>,
    ) -> GoalWakeArgs {
        GoalWakeArgs {
            trigger_fact: trigger_fact.map(ToOwned::to_owned),
            trigger_schema_id: trigger_schema_id.map(ToOwned::to_owned),
            trigger_schema_version: None,
            tool_ids,
            prompt: "wake plan".into(),
            hard_memories: None,
        }
    }

    #[test]
    fn encode_wake_config_requires_exactly_one_trigger() {
        let ctx = test_ctx();
        let fact = format!("F:{}", uuid::Uuid::now_v7());
        let tools = vec!["core_search_memories".to_string()];

        let neither = encode_wake_config(&ctx, wake_args(None, None, tools.clone()))
            .expect_err("no trigger must be rejected");
        assert!(
            matches!(neither, McpToolError::InvalidInput(ref m) if m.contains("exactly one")),
            "got {neither:?}",
        );

        let both = encode_wake_config(
            &ctx,
            wake_args(Some(fact.as_str()), Some("test/fact-v1"), tools.clone()),
        )
        .expect_err("two triggers must be rejected");
        assert!(
            matches!(both, McpToolError::InvalidInput(ref m) if m.contains("exactly one")),
            "got {both:?}",
        );

        encode_wake_config(&ctx, wake_args(Some(fact.as_str()), None, tools.clone()))
            .expect("fact-memory trigger encodes");
        encode_wake_config(&ctx, wake_args(None, Some("test/fact-v1"), tools))
            .expect("fact-schema trigger encodes");
    }

    #[test]
    fn encode_wake_config_rejects_version_without_schema_trigger() {
        let ctx = test_ctx();
        let mut args = wake_args(
            Some(format!("F:{}", uuid::Uuid::now_v7()).as_str()),
            None,
            vec!["core_search_memories".to_string()],
        );
        args.trigger_schema_version = Some(2);
        let err = encode_wake_config(&ctx, args).expect_err("stray schema version must fail");
        assert!(
            matches!(err, McpToolError::InvalidInput(ref m) if m.contains("trigger_schema_id")),
            "got {err:?}",
        );
    }

    #[test]
    fn encode_wake_config_validates_toolset_against_registry() {
        let ctx = test_ctx();

        let empty = encode_wake_config(&ctx, wake_args(None, Some("test/fact-v1"), Vec::new()))
            .expect_err("empty toolset must fail");
        assert!(
            matches!(empty, McpToolError::Protocol(ref e) if e.message.contains("nonempty")),
            "got {empty:?}",
        );

        let unknown = encode_wake_config(
            &ctx,
            wake_args(None, Some("test/fact-v1"), vec!["no_such_tool".into()]),
        )
        .expect_err("unregistered tool id must fail");
        assert!(matches!(unknown, McpToolError::Protocol(_)), "{unknown:?}");

        let grouped = encode_wake_config(
            &ctx,
            wake_args(None, Some("test/fact-v1"), vec!["core_goal".into()]),
        )
        .expect_err("grouped tool without leaf action must fail");
        assert!(
            matches!(grouped, McpToolError::Protocol(ref e) if e.message.contains("leaf")),
            "got {grouped:?}",
        );

        let leaf = encode_wake_config(
            &ctx,
            wake_args(None, Some("test/fact-v1"), vec!["core_goal:set".into()]),
        )
        .expect("leaf action id encodes");
        assert_eq!(
            leaf.tool_ids()
                .iter()
                .map(GoalWakeToolId::as_str)
                .collect::<Vec<_>>(),
            vec!["core_goal:set"],
        );
    }

    #[test]
    fn goal_transition_accepts_mixed_case() {
        assert!(matches!(
            serde_json::from_value::<GoalTransition>(serde_json::json!("Pause")).unwrap(),
            GoalTransition::Pause
        ));
        assert!(matches!(
            serde_json::from_value::<GoalTransition>(serde_json::json!("resume")).unwrap(),
            GoalTransition::Resume
        ));
        assert!(matches!(
            serde_json::from_value::<GoalTransition>(serde_json::json!("ABANDON")).unwrap(),
            GoalTransition::Abandon
        ));
    }

    #[test]
    fn goal_payload_args_body_schema_is_object() {
        let schema =
            serde_json::to_value(schemars::schema_for!(GoalPayloadArgs)).expect("schema JSON");
        let body = schema
            .pointer("/properties/body")
            .expect("body property schema");
        assert_eq!(
            body.get("type").and_then(serde_json::Value::as_str),
            Some("object"),
            "body must be advertised as an object schema: {body:#}",
        );
    }
}
