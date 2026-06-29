use crate::engine::{
    GoalCreatePayloadWriteRequest, GoalDecomposeRequest, GoalMarkAchievedRequest,
    GoalModifyRequest, GoalTransitionRequest,
};
use crate::mcp::{CoreActionMeta, McpActionArgSpec, McpTool, McpToolCtx, McpToolError};
use crate::verbs::goal_write::{
    ChildGoalDraft, GoalAssignmentTarget, GoalAuthorship, GoalEvidenceRef, GoalPayloadWrite,
    GoalState, GoalTopologyWrite, GoalWriteOutcome, IdempotencyKey, OperatorKind, SystemOrigin,
};
use crate::verbs::schema::PayloadKind;
use crate::{EdgeId, ModelId, OperatorId, PromptVersion, SchemaId, SchemaVersion, ToolId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{WRITE_IDEMPOTENT, WRITE_NON_IDEMPOTENT};

const MAX_CHILD_GOALS: usize = 50;
const CORE_GOAL_SET_SCOPE_KEY: &str = "core_goal:set";
const CORE_GOAL_TRANSITION_SCOPE_KEY: &str = "core_goal:transition";
const CORE_GOAL_MODIFY_SCOPE_KEY: &str = "core_goal:modify";
const CORE_GOAL_MARK_ACHIEVED_SCOPE_KEY: &str = "core_goal:mark_achieved";
const CORE_GOAL_DECOMPOSE_SCOPE_KEY: &str = "core_goal:decompose";
const MCP_OPERATOR_NAMESPACE: uuid::Uuid = uuid::Uuid::from_bytes([
    0x3f, 0x61, 0xde, 0x85, 0x4e, 0x09, 0x45, 0x62, 0x97, 0xc4, 0x8a, 0x74, 0xaa, 0xf9, 0x4a, 0x2c,
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
        scope_key: CORE_GOAL_SET_SCOPE_KEY,
        description: "Set an Active Goal assigned to a Perspective.",
        produces_schema_ids: GOAL_ACTIVATED_SCHEMA_IDS,
        annotations: WRITE_NON_IDEMPOTENT,
    },
    CoreActionMeta {
        tool: CoreGoalTool::NAME,
        action: "transition",
        scope_key: CORE_GOAL_TRANSITION_SCOPE_KEY,
        description: "Pause, resume, or abandon a Goal head.",
        produces_schema_ids: &[],
        annotations: WRITE_NON_IDEMPOTENT,
    },
    CoreActionMeta {
        tool: CoreGoalTool::NAME,
        action: "modify",
        scope_key: CORE_GOAL_MODIFY_SCOPE_KEY,
        description: "Replace an Active Goal head's content.",
        produces_schema_ids: GOAL_ACTIVATED_SCHEMA_IDS,
        annotations: WRITE_NON_IDEMPOTENT,
    },
    CoreActionMeta {
        tool: CoreGoalTool::NAME,
        action: "mark_achieved",
        scope_key: CORE_GOAL_MARK_ACHIEVED_SCOPE_KEY,
        description: "Mark a Goal head Achieved with completion evidence.",
        produces_schema_ids: GOAL_ACHIEVED_SCHEMA_IDS,
        annotations: WRITE_NON_IDEMPOTENT,
    },
    CoreActionMeta {
        tool: CoreGoalTool::NAME,
        action: "decompose",
        scope_key: CORE_GOAL_DECOMPOSE_SCOPE_KEY,
        description: "Create Active child Goals under a parent Goal.",
        produces_schema_ids: GOAL_ACTIVATED_SCHEMA_IDS,
        annotations: WRITE_IDEMPOTENT,
    },
];

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GoalPayloadArgs {
    #[schemars(
        description = "Registered Goal-payload schema id (PayloadKind::Goal). Discover valid ids with `core_list_schemas` (kind=Goal)."
    )]
    pub schema_id: String,
    #[schemars(description = "Goal-payload schema version. Omit to default to 1.")]
    pub schema_version: Option<u32>,
    #[schemars(description = "Short, human-readable goal title, 1 to 240 chars.")]
    pub title: String,
    #[schemars(
        description = "The goal stated in prose, 1 to 20000 chars — what pursuing or achieving it means."
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
    #[serde(default)]
    #[schemars(
        description = "Optional memory handles (`F.../A.../P...`) that motivate this goal. Use `[]` when there is none."
    )]
    pub evidence: Vec<String>,
    #[schemars(
        description = "Optional Perspective memory handle to assign the goal to; omit to use the caller Perspective context."
    )]
    pub target_perspective: Option<String>,
    #[schemars(
        description = "Optional stable idempotency key so a replayed call is a no-op, not a duplicate goal."
    )]
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct GoalWriteOutput {
    pub handle: String,
    pub lifecycle_memory: Option<String>,
    pub edge_handles: Vec<String>,
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

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum CoreGoalOutput {
    Write(GoalWriteOutput),
    Decompose(GoalDecomposeOutput),
}

impl McpTool for CoreGoalTool {
    const NAME: &'static str = "core_goal";
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
                "idempotency_key",
            ],
            required_fields: &["schema_id", "title", "text"],
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
    let payload = encode_goal_payload(&ctx, args.payload)?;
    let evidence = resolve_evidence(&ctx, &args.evidence)?;
    let assignment = target_perspective(&ctx, args.target_perspective.as_deref())?;
    let topology =
        GoalTopologyWrite::new(assignment, Vec::new(), evidence).map_err(McpToolError::Protocol)?;
    let request_id = IdempotencyKey::optional_or_generated("goal_set", args.idempotency_key)
        .map_err(McpToolError::InvalidInput)?;
    let authorship = system_operator_authorship(&ctx, "goal_set");
    let engine = ctx
        .engine()
        .ok_or_else(|| McpToolError::Other("engine unavailable".into()))?;
    let outcome = engine
        .create_goal_from_payload_write(
            &ctx.authz,
            &GoalCreatePayloadWriteRequest {
                principal: ctx.owner,
                topology,
                wake: None,
                payload,
                request_id,
                authorship,
                author_self_perspective_id: ctx.caller_self_perspective,
            },
        )
        .await
        .map_err(McpToolError::Protocol)?;
    Ok(format_goal_write_output(&ctx, outcome))
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GoalTransition {
    Pause,
    Resume,
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
    let engine = ctx
        .engine()
        .ok_or_else(|| McpToolError::Other("engine unavailable".into()))?;
    let outcome = engine
        .transition_goal(
            &ctx.authz,
            &GoalTransitionRequest {
                principal: ctx.owner,
                prior_goal_id: prior,
                next_state,
                authorship: GoalAuthorship::User,
                request_id,
                author_self_perspective_id: ctx.caller_self_perspective,
            },
        )
        .await
        .map_err(McpToolError::Protocol)?;
    Ok(format_goal_write_output(&ctx, outcome))
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
    let engine = ctx
        .engine()
        .ok_or_else(|| McpToolError::Other("engine unavailable".into()))?;
    let outcome = engine
        .mark_goal_achieved(
            &ctx.authz,
            &GoalMarkAchievedRequest {
                principal: ctx.owner,
                prior_goal_id: prior,
                authorship: GoalAuthorship::System(SystemOrigin::Tool {
                    tool_id: ToolId::new(CORE_GOAL_MARK_ACHIEVED_SCOPE_KEY),
                }),
                request_id,
                evidence,
                author_self_perspective_id: ctx.caller_self_perspective,
            },
        )
        .await
        .map_err(McpToolError::Protocol)?;
    Ok(format_goal_write_output(&ctx, outcome))
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
        description = "Optional evidence handles (`F.../A.../P...`) to attach to the modified goal head."
    )]
    pub evidence: Option<Vec<String>>,
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
    let request_id = IdempotencyKey::optional_or_generated("goal_modify", args.idempotency_key)
        .map_err(McpToolError::InvalidInput)?;
    let engine = ctx
        .engine()
        .ok_or_else(|| McpToolError::Other("engine unavailable".into()))?;
    let outcome = engine
        .modify_goal(
            &ctx.authz,
            &GoalModifyRequest {
                principal: ctx.owner,
                prior_goal_id: prior,
                replacement: payload,
                wake: None,
                authorship: GoalAuthorship::User,
                request_id,
                evidence,
                author_self_perspective_id: ctx.caller_self_perspective,
            },
        )
        .await
        .map_err(McpToolError::Protocol)?;
    Ok(format_goal_write_output(&ctx, outcome))
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GoalDecomposeArgs {
    #[schemars(
        description = "Handle of the parent goal the children attach under (from `core_goal` action=set)."
    )]
    pub parent_goal: String,
    #[schemars(
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
        description = "Optional motivating memory handles for this child goal. Use `[]` when there is none."
    )]
    pub evidence: Vec<String>,
}

#[derive(Debug, Serialize)]
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
            wake: None,
            request_id: root_key
                .child("goal_decompose", index)
                .map_err(McpToolError::InvalidInput)?,
        });
    }
    let engine = ctx
        .engine()
        .ok_or_else(|| McpToolError::Other("engine unavailable".into()))?;
    let outcome = engine
        .decompose_goal(
            &ctx.authz,
            &GoalDecomposeRequest {
                principal: ctx.owner,
                parent_goal_id: parent,
                authorship: GoalAuthorship::System(SystemOrigin::Tool {
                    tool_id: ToolId::new(CORE_GOAL_DECOMPOSE_SCOPE_KEY),
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
            .map(|child| format_goal_write_output(&ctx, child.outcome))
            .collect(),
        idempotent_replay: outcome.idempotent_replay,
    })
}

fn encode_goal_payload(
    ctx: &McpToolCtx,
    args: GoalPayloadArgs,
) -> Result<GoalPayloadWrite, McpToolError> {
    let title = args.title.trim();
    if title.is_empty() || title.chars().count() > 240 {
        return Err(McpToolError::InvalidInput(
            "goal title must be 1..=240 chars".into(),
        ));
    }
    let text = args.text.trim();
    if text.is_empty() || text.chars().count() > 20_000 {
        return Err(McpToolError::InvalidInput(
            "goal text must be 1..=20000 chars".into(),
        ));
    }
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
    let payload = ctx
        .registry
        .ingest_protocol_payload(&schema_id, schema_version, PayloadKind::Goal, &args.body)
        .map_err(McpToolError::InvalidInput)?;
    let sidecar_payload = schema
        .sidecar_table
        .is_some()
        .then_some(payload.sidecar_payload);
    let payload = payload.key_bytes.ok_or_else(|| {
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
        payload,
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
    GoalAuthorship::System(SystemOrigin::Operator {
        operator_id: OperatorId::new(operator_id),
        operator_kind: OperatorKind::AtoGoal,
        model_id: ModelId::new(ctx.author.model_id.clone()),
        prompt_version: PromptVersion::new(prompt_version),
    })
}

fn format_goal_write_output(ctx: &McpToolCtx, outcome: GoalWriteOutcome) -> GoalWriteOutput {
    GoalWriteOutput {
        handle: ctx.format_goal(outcome.goal_id),
        lifecycle_memory: outcome
            .lifecycle_memory_id
            .map(|memory_id| ctx.format_fact_memory(memory_id)),
        edge_handles: outcome
            .edge_ids
            .into_iter()
            .map(|edge_id| ctx.format_edge(EdgeId::new(edge_id)))
            .collect(),
        idempotent_replay: outcome.idempotent_replay,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
