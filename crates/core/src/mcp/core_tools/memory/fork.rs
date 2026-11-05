use crate::goal_collaboration::{ForkMemoryRequest, MemoryDestination};
use crate::mcp::{McpTool, McpToolCtx, McpToolError, Replay, ToolEffect};
use crate::protocol::tool as protocol_tool;
use crate::verbs::idempotency::IdempotencyKey;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ForkArgs {
    pub source: String,
    /// Exactly one destination is required. An owner UUID creates a new
    /// durable owner series; a Goal UUID uses that Goal's working binding.
    pub owner: Option<String>,
    pub working_goal: Option<String>,
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ForkOutput {
    pub memory: String,
    pub series: String,
    pub forked_from: String,
    pub idempotent_replay: bool,
}

#[derive(Debug)]
pub struct ForkTool;

impl McpTool for ForkTool {
    const NAME: &'static str = protocol_tool::CORE_FORK;
    const TITLE: &'static str = "Fork a memory version";
    const DESCRIPTION: &'static str = "Create an independently owned Abstraction or Perspective series from one exact version. A typed schema must opt in; source fork permission and destination write permission are both required.";
    const EFFECT: Option<ToolEffect> = Some(ToolEffect::Additive(Replay::NonIdempotent));
    type Args = ForkArgs;
    type Output = ForkOutput;

    fn call(
        ctx: McpToolCtx,
        args: ForkArgs,
    ) -> futures::future::BoxFuture<'static, Result<ForkOutput, McpToolError>> {
        Box::pin(async move {
            let source = ctx.resolve_memory(&args.source)?;
            let destination = match (args.owner, args.working_goal) {
                (Some(owner), None) => MemoryDestination::Owner(crate::OwnerId::new(
                    crate::mcp::ids::parse_wire_uuid(&owner)?,
                )),
                (None, Some(goal)) => MemoryDestination::WorkingGoal(crate::GoalHandle::new(
                    crate::mcp::ids::parse_wire_uuid(&goal)?,
                )),
                (None, None) | (Some(_), Some(_)) => {
                    return Err(McpToolError::InvalidInput(
                        "provide exactly one of owner or working_goal".into(),
                    ));
                }
            };
            let request_id =
                IdempotencyKey::optional_or_generated("memory_fork", args.idempotency_key)
                    .map_err(McpToolError::InvalidInput)?;
            let outcome = ctx
                .require_engine()?
                .fork_memory(
                    &ctx.authz,
                    &ForkMemoryRequest {
                        source,
                        destination,
                        request_id,
                    },
                )
                .await
                .map_err(McpToolError::Protocol)?;
            Ok(ForkOutput {
                memory: outcome.memory_id.into_inner().to_string(),
                series: outcome.handle.into_inner().to_string(),
                forked_from: outcome.forked_from.into_inner().to_string(),
                idempotent_replay: outcome.idempotent_replay,
            })
        })
    }
}
