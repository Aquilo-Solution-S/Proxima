use crate::mcp::{McpTool, McpToolCtx, McpToolError};
use crate::mcp::{Replay, ToolEffect};
use crate::protocol::tool as protocol_tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RestoreArgs {
    #[schemars(description = "Memory UUID in your current space.")]
    pub memory: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct RestoreOutput {
    pub ok: bool,
    pub memory: String,
}

/// The inverse of [`super::ForgetTool`], served as its own tool so its effect
/// is its own: it removes nothing, and restoring a live memory changes
/// nothing, so a palette or client can admit it apart from forget.
#[derive(Debug)]
pub struct RestoreTool;

impl McpTool for RestoreTool {
    const NAME: &'static str = protocol_tool::CORE_RESTORE;
    const TITLE: &'static str = "Restore a memory";
    const DESCRIPTION: &'static str = "Bring back a forgotten memory; a live one is unchanged.";
    const EFFECT: Option<ToolEffect> = Some(ToolEffect::Additive(Replay::Idempotent));
    type Args = RestoreArgs;
    type Output = RestoreOutput;

    fn call(
        ctx: McpToolCtx,
        args: RestoreArgs,
    ) -> futures::future::BoxFuture<'static, Result<RestoreOutput, McpToolError>> {
        Box::pin(async move {
            let memory_id = ctx.resolve_memory(&args.memory)?;
            let engine = ctx.require_engine()?;
            let owner = ctx.owner;
            let authz = ctx
                .authz
                .clone()
                .narrowed_to_owner(owner)
                .ok_or_else(|| McpToolError::NotAuthorized("restore".into()))?;
            engine.restore_memory(&authz, owner, memory_id).await?;
            Ok(RestoreOutput {
                ok: true,
                memory: args.memory,
            })
        })
    }
}
