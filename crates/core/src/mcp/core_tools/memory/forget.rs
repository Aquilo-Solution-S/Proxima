use crate::mcp::{McpTool, McpToolCtx, McpToolError};
use crate::protocol::tool as protocol_tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::super::DESTRUCTIVE_NON_IDEMPOTENT;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ForgetArgs {
    #[schemars(
        description = "Memory to cool: `F:<uuid>`, `A:<uuid>`, or `P:<uuid>`. The id is `t`."
    )]
    pub memory: String,
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
    const DESCRIPTION: &'static str = "Cool one memory t: PUT cold object, delete hot row, announce.forget. ingest_keys stay. Refuses if a remaining hot non-Fact would lose its last hot pin / cooled-Fact leaf.";
    const ANNOTATIONS: Option<crate::mcp::McpToolAnnotations> = Some(DESTRUCTIVE_NON_IDEMPOTENT);
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
            engine.forget_memory(&authz, owner, memory_id).await?;
            Ok(ForgetOutput {
                ok: true,
                memory: args.memory,
            })
        })
    }
}
