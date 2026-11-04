use crate::mcp::{McpTool, McpToolCtx, McpToolError};
use crate::mcp::{Replay, ToolEffect};
use crate::protocol::tool as protocol_tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ForgetArgs {
    #[schemars(description = "Memory UUID in the selected space.")]
    pub memory: String,
    #[serde(default)]
    #[schemars(description = "Memory space key from core_memory_spaces. Omit for current owner.")]
    pub space: Option<String>,
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
    const TITLE: &'static str = "Forget a memory";
    const DESCRIPTION: &'static str = "Hide a memory from search, recall, walks and other stores until core_restore; erase removes it.";
    const EFFECT: Option<ToolEffect> = Some(ToolEffect::Destructive(Replay::NonIdempotent));
    type Args = ForgetArgs;
    type Output = ForgetOutput;

    fn call(
        ctx: McpToolCtx,
        args: ForgetArgs,
    ) -> futures::future::BoxFuture<'static, Result<ForgetOutput, McpToolError>> {
        Box::pin(async move {
            let memory_id = ctx.resolve_memory(&args.memory)?;
            let (space, authz) =
                super::super::memory_spaces::resolve_space_for_write(&ctx, args.space.as_deref())?;
            let engine = ctx.require_engine()?;
            engine.forget_memory(&authz, space.owner, memory_id).await?;
            Ok(ForgetOutput {
                ok: true,
                memory: args.memory,
            })
        })
    }
}
