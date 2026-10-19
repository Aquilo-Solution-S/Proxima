use crate::mcp::{McpTool, McpToolCtx, McpToolError};
use crate::mcp::{Replay, ToolEffect};
use crate::protocol::tool as protocol_tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ForgetArgs {
    #[schemars(description = "Memory to forget or restore, in your current space: memory UUIDs.")]
    pub memory: String,
    #[schemars(
        description = "forget (default) hides the memory; restore brings a forgotten memory back. Restoring a live memory changes nothing."
    )]
    #[serde(default)]
    pub action: ForgetAction,
}

/// What `core_forget` does to the memory.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ForgetAction {
    #[default]
    Forget,
    Restore,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ForgetOutput {
    pub ok: bool,
    pub memory: String,
}

#[derive(Debug)]
pub struct ForgetTool;

impl McpTool for ForgetTool {
    const NAME: &'static str = protocol_tool::CORE_FORGET;
    const DESCRIPTION: &'static str = "Forget one memory: it is kept but hidden from search, recall, walks and other stores until restored (action restore); erase removes it.";
    const EFFECT: Option<ToolEffect> = Some(ToolEffect::Destructive(Replay::NonIdempotent));
    type Args = ForgetArgs;
    type Output = ForgetOutput;

    fn call(
        ctx: McpToolCtx,
        args: ForgetArgs,
    ) -> futures::future::BoxFuture<'static, Result<ForgetOutput, McpToolError>> {
        Box::pin(async move {
            let memory_id = ctx.resolve_memory(&args.memory)?;
            let engine = ctx.require_engine()?;
            let owner = ctx.owner;
            let authz = ctx
                .authz
                .clone()
                .narrowed_to_owner(owner)
                .ok_or_else(|| McpToolError::NotAuthorized("forget".into()))?;
            match args.action {
                ForgetAction::Forget => engine.forget_memory(&authz, owner, memory_id).await?,
                ForgetAction::Restore => engine.restore_memory(&authz, owner, memory_id).await?,
            }
            Ok(ForgetOutput {
                ok: true,
                memory: args.memory,
            })
        })
    }
}
