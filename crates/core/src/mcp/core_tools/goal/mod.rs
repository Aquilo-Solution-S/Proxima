use crate::mcp::{McpTool, McpToolCtx, McpToolError};
use crate::verbs::goal_write::{
    AchieveGoalAtomicRequest, ChildGoalDraft, CreateGoalAtomicRequest, DecomposeGoalAtomicRequest,
    GoalAtomicContext, GoalAuthorship, GoalDraft, GoalEvidenceRef, GoalPayloadWrite, GoalState,
    GoalWriteOutcome, IdempotencyKey, ModifyGoalAtomicRequest, OperatorKind, SystemOrigin,
    TransitionGoalAtomicRequest,
};
use crate::verbs::schema::PayloadKind;
use crate::{
    EdgeId, MemoryId, ModelId, OperatorId, PersonalityInstanceId, PromptVersion, SchemaId,
    SchemaVersion, ToolId, canonical_json_bytes,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

const MAX_CHILD_GOALS: usize = 50;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GoalPayloadArgs {
    pub schema_id: String,
    pub schema_version: Option<u32>,
    pub title: String,
    pub text: String,
    #[serde(default)]
    pub body: serde_json::Value,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GoalSetArgs {
    #[serde(flatten)]
    pub payload: GoalPayloadArgs,
    #[serde(default)]
    pub evidence: Vec<String>,
    pub target_personality: Option<String>,
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
pub struct GoalSetTool;

impl McpTool for GoalSetTool {
    const NAME: &'static str = "core/goal_set";
    const DESCRIPTION: &'static str = "Set an Active Goal.";
    const PRODUCES_SCHEMA_IDS: &'static [&'static str] =
        &[<crate::GoalActivatedV1 as crate::FactPayload>::SCHEMA_ID];
    type Args = GoalSetArgs;
    type Output = GoalWriteOutput;

    fn call(
        ctx: McpToolCtx,
        args: GoalSetArgs,
    ) -> futures::future::BoxFuture<'static, Result<GoalWriteOutput, McpToolError>> {
        Box::pin(async move {
            let payload = encode_goal_payload(&ctx, args.payload)?;
            let evidence = resolve_evidence(&ctx, &args.evidence)?;
            let target_self =
                target_self_perspective(&ctx, args.target_personality.as_deref()).await?;
            let request_id =
                IdempotencyKey::optional_or_generated("goal_set", args.idempotency_key)
                    .map_err(McpToolError::InvalidInput)?;
            let authorship = system_operator_authorship(&ctx, "goal_set")?;
            let draft = GoalDraft {
                principal: ctx.owner.principal.clone(),
                org_id: Some(ctx.owner.org_id),
                schema_id: payload.schema_id.clone(),
                schema_version: payload.schema_version,
                title: payload.title.clone(),
                text: payload.text.clone(),
                payload: payload.payload.clone(),
                state: GoalState::Active,
                parent_goal_ids: Vec::new(),
                supersedes_goal_id: None,
                authorship,
                request_id: request_id.into_string(),
            };
            let storage = ctx
                .storage()
                .ok_or_else(|| McpToolError::Other("engine storage unavailable".into()))?;
            let outcome = storage
                .create_goal_atomic(&CreateGoalAtomicRequest {
                    draft,
                    context: goal_atomic_context(&ctx),
                    target_self_perspective_id: target_self,
                    evidence,
                })
                .await
                .map_err(McpToolError::Storage)?;
            Ok(format_goal_write_output(&ctx, outcome))
        })
    }
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
    pub goal: String,
    pub transition: GoalTransition,
    pub idempotency_key: Option<String>,
}

#[derive(Debug)]
pub struct GoalTransitionTool;

impl McpTool for GoalTransitionTool {
    const NAME: &'static str = "core/goal_transition";
    const DESCRIPTION: &'static str = "Pause, resume, or abandon a Goal head.";
    type Args = GoalTransitionArgs;
    type Output = GoalWriteOutput;

    fn call(
        ctx: McpToolCtx,
        args: GoalTransitionArgs,
    ) -> futures::future::BoxFuture<'static, Result<GoalWriteOutput, McpToolError>> {
        Box::pin(async move {
            let prior = ctx.resolve_goal(&args.goal)?;
            let next_state = match args.transition {
                GoalTransition::Pause => GoalState::Paused,
                GoalTransition::Resume => GoalState::Active,
                GoalTransition::Abandon => GoalState::Abandoned,
            };
            let request_id =
                IdempotencyKey::optional_or_generated("goal_transition", args.idempotency_key)
                    .map_err(McpToolError::InvalidInput)?;
            let storage = ctx
                .storage()
                .ok_or_else(|| McpToolError::Other("engine storage unavailable".into()))?;
            let outcome = storage
                .transition_goal_atomic(&TransitionGoalAtomicRequest {
                    owner: ctx.owner.clone(),
                    prior_goal_id: prior,
                    next_state,
                    authorship: GoalAuthorship::User,
                    request_id,
                    context: goal_atomic_context(&ctx),
                })
                .await
                .map_err(McpToolError::Storage)?;
            Ok(format_goal_write_output(&ctx, outcome))
        })
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GoalMarkAchievedArgs {
    pub goal: String,
    pub evidence: Vec<String>,
    pub idempotency_key: Option<String>,
}

#[derive(Debug)]
pub struct GoalMarkAchievedTool;

impl McpTool for GoalMarkAchievedTool {
    const NAME: &'static str = "core/goal_mark_achieved";
    const DESCRIPTION: &'static str = "Mark a Goal head Achieved with evidence.";
    const PRODUCES_SCHEMA_IDS: &'static [&'static str] =
        &[<crate::GoalAchievedV1 as crate::FactPayload>::SCHEMA_ID];
    type Args = GoalMarkAchievedArgs;
    type Output = GoalWriteOutput;

    fn call(
        ctx: McpToolCtx,
        args: GoalMarkAchievedArgs,
    ) -> futures::future::BoxFuture<'static, Result<GoalWriteOutput, McpToolError>> {
        Box::pin(async move {
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
            let storage = ctx
                .storage()
                .ok_or_else(|| McpToolError::Other("engine storage unavailable".into()))?;
            let outcome = storage
                .achieve_goal_atomic(&AchieveGoalAtomicRequest {
                    owner: ctx.owner.clone(),
                    prior_goal_id: prior,
                    authorship: GoalAuthorship::System(SystemOrigin::Tool {
                        tool_id: ToolId::new(GoalMarkAchievedTool::NAME),
                    }),
                    request_id,
                    context: goal_atomic_context(&ctx),
                    evidence,
                })
                .await
                .map_err(McpToolError::Storage)?;
            Ok(format_goal_write_output(&ctx, outcome))
        })
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GoalModifyArgs {
    pub goal: String,
    #[serde(flatten)]
    pub payload: GoalPayloadArgs,
    pub evidence: Option<Vec<String>>,
    pub idempotency_key: Option<String>,
}

#[derive(Debug)]
pub struct GoalModifyTool;

impl McpTool for GoalModifyTool {
    const NAME: &'static str = "core/goal_modify";
    const DESCRIPTION: &'static str = "Replace an Active Goal head's content.";
    const PRODUCES_SCHEMA_IDS: &'static [&'static str] =
        &[<crate::GoalActivatedV1 as crate::FactPayload>::SCHEMA_ID];
    type Args = GoalModifyArgs;
    type Output = GoalWriteOutput;

    fn call(
        ctx: McpToolCtx,
        args: GoalModifyArgs,
    ) -> futures::future::BoxFuture<'static, Result<GoalWriteOutput, McpToolError>> {
        Box::pin(async move {
            let prior = ctx.resolve_goal(&args.goal)?;
            let payload = encode_goal_payload(&ctx, args.payload)?;
            let evidence = args
                .evidence
                .as_ref()
                .map(|evidence| resolve_evidence(&ctx, evidence))
                .transpose()?;
            let request_id =
                IdempotencyKey::optional_or_generated("goal_modify", args.idempotency_key)
                    .map_err(McpToolError::InvalidInput)?;
            let storage = ctx
                .storage()
                .ok_or_else(|| McpToolError::Other("engine storage unavailable".into()))?;
            let outcome = storage
                .modify_goal_atomic(&ModifyGoalAtomicRequest {
                    owner: ctx.owner.clone(),
                    prior_goal_id: prior,
                    replacement: payload,
                    authorship: GoalAuthorship::User,
                    request_id,
                    context: goal_atomic_context(&ctx),
                    evidence,
                })
                .await
                .map_err(McpToolError::Storage)?;
            Ok(format_goal_write_output(&ctx, outcome))
        })
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GoalDecomposeArgs {
    pub parent_goal: String,
    pub children: Vec<ChildGoalInput>,
    pub target_personality: Option<String>,
    pub idempotency_key: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ChildGoalInput {
    #[serde(flatten)]
    pub payload: GoalPayloadArgs,
    #[serde(default)]
    pub evidence: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct GoalDecomposeOutput {
    pub parent_goal: String,
    pub children: Vec<GoalWriteOutput>,
    pub idempotent_replay: bool,
}

#[derive(Debug)]
pub struct GoalDecomposeTool;

impl McpTool for GoalDecomposeTool {
    const NAME: &'static str = "core/goal_decompose";
    const DESCRIPTION: &'static str = "Create Active child Goals under a parent Goal.";
    const PRODUCES_SCHEMA_IDS: &'static [&'static str] =
        &[<crate::GoalActivatedV1 as crate::FactPayload>::SCHEMA_ID];
    type Args = GoalDecomposeArgs;
    type Output = GoalDecomposeOutput;

    fn call(
        ctx: McpToolCtx,
        args: GoalDecomposeArgs,
    ) -> futures::future::BoxFuture<'static, Result<GoalDecomposeOutput, McpToolError>> {
        Box::pin(async move {
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
            let target_self =
                target_self_perspective(&ctx, args.target_personality.as_deref()).await?;
            let root_key =
                IdempotencyKey::new(args.idempotency_key).map_err(McpToolError::InvalidInput)?;
            let mut children = Vec::with_capacity(args.children.len());
            for (index, child) in args.children.into_iter().enumerate() {
                children.push(ChildGoalDraft {
                    payload: encode_goal_payload(&ctx, child.payload)?,
                    evidence: resolve_evidence(&ctx, &child.evidence)?,
                    request_id: root_key
                        .child("goal_decompose", index)
                        .map_err(McpToolError::InvalidInput)?,
                });
            }
            let storage = ctx
                .storage()
                .ok_or_else(|| McpToolError::Other("engine storage unavailable".into()))?;
            let outcome = storage
                .decompose_goal_atomic(&DecomposeGoalAtomicRequest {
                    owner: ctx.owner.clone(),
                    parent_goal_id: parent,
                    authorship: GoalAuthorship::System(SystemOrigin::Tool {
                        tool_id: ToolId::new(GoalDecomposeTool::NAME),
                    }),
                    context: goal_atomic_context(&ctx),
                    target_self_perspective_id: target_self,
                    children,
                })
                .await
                .map_err(McpToolError::Storage)?;
            Ok(GoalDecomposeOutput {
                parent_goal: ctx.format_goal(parent),
                children: outcome
                    .children
                    .into_iter()
                    .map(|child| format_goal_write_output(&ctx, child.outcome))
                    .collect(),
                idempotent_replay: outcome.idempotent_replay,
            })
        })
    }
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
    ctx.registry
        .validate_payload(&schema_id, schema_version, PayloadKind::Goal, &args.body)
        .map_err(McpToolError::InvalidInput)?;
    let payload = match schema.json_encoder {
        Some(encode) => encode(&args.body).map_err(McpToolError::InvalidInput)?,
        None => canonical_json_bytes(&args.body),
    };
    Ok(GoalPayloadWrite {
        schema_id,
        schema_version,
        title: title.to_string(),
        text: text.to_string(),
        payload,
    })
}

fn resolve_evidence(
    ctx: &McpToolCtx,
    evidence: &[String],
) -> Result<Vec<GoalEvidenceRef>, McpToolError> {
    evidence
        .iter()
        .map(|handle| {
            ctx.resolve_memory(handle)
                .map(|memory_id| GoalEvidenceRef { memory_id })
        })
        .collect()
}

async fn target_self_perspective(
    ctx: &McpToolCtx,
    target_personality: Option<&str>,
) -> Result<MemoryId, McpToolError> {
    match target_personality {
        Some(handle) => {
            let instance_id = ctx.resolve_personality(handle)?;
            personality_root(ctx, instance_id).await
        }
        None => ctx.caller_self_perspective.ok_or_else(|| {
            McpToolError::InvalidInput(
                "target_personality or caller_self_perspective is required".into(),
            )
        }),
    }
}

async fn personality_root(
    ctx: &McpToolCtx,
    instance_id: PersonalityInstanceId,
) -> Result<MemoryId, McpToolError> {
    let (owner_kind, owner_id) = match &ctx.owner.principal {
        crate::Principal::User(user) => (crate::OwnerPrincipalKind::User, user.into_inner()),
        crate::Principal::Group(group) => (crate::OwnerPrincipalKind::Group, group.into_inner()),
    };
    let row: Option<(uuid::Uuid,)> = sqlx::query_as(
        "SELECT current_root_perspective_memory_id
           FROM proxima_core.personality
          WHERE owner_principal_kind = $1
            AND owner_principal_id = $2
            AND owner_org_id = $3
            AND personality_instance_id = $4
            AND status <> 'tombstoned'::proxima_core.personality_status",
    )
    .bind(owner_kind)
    .bind(owner_id)
    .bind(ctx.owner.org_id.into_inner())
    .bind(instance_id.into_inner())
    .fetch_optional(&ctx.pool)
    .await
    .map_err(|err| McpToolError::Other(err.to_string()))?;
    row.map(|(memory_id,)| MemoryId::new(memory_id))
        .ok_or_else(|| McpToolError::InvalidInput("target personality not found".into()))
}

fn system_operator_authorship(
    ctx: &McpToolCtx,
    prompt_version: &str,
) -> Result<GoalAuthorship, McpToolError> {
    let personality_instance_id = ctx.author.personality_instance_id.ok_or_else(|| {
        McpToolError::InvalidInput("goal_set requires personality author context".into())
    })?;
    Ok(GoalAuthorship::System(SystemOrigin::Operator {
        operator_id: OperatorId::new(uuid::Uuid::now_v7()),
        operator_kind: OperatorKind::AtoGoal,
        model_id: ModelId::new(ctx.author.model_id.clone()),
        prompt_version: PromptVersion::new(prompt_version),
        personality_instance_id,
    }))
}

fn goal_atomic_context(ctx: &McpToolCtx) -> GoalAtomicContext<'_> {
    GoalAtomicContext {
        registry: &ctx.registry,
        embedding_model_id: None,
        author_self_perspective_id: ctx.caller_self_perspective,
    }
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
