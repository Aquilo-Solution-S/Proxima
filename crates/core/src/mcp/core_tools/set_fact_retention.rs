//! `core/set_fact_retention` — set the owner Fact-retention config.

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::mcp::{McpTool, McpToolCtx, McpToolError};

#[derive(Debug, Default)]
pub struct SetFactRetentionTool;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetFactRetentionArgs {
    pub retention_seconds: u64,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SetFactRetentionOutput {
    pub retention_seconds: u64,
}

impl McpTool for SetFactRetentionTool {
    const NAME: &'static str = "core/set_fact_retention";
    const DESCRIPTION: &'static str = "Set the owner Fact-retention duration in seconds.";
    type Args = SetFactRetentionArgs;
    type Output = SetFactRetentionOutput;

    fn call(
        ctx: McpToolCtx,
        args: SetFactRetentionArgs,
    ) -> BoxFuture<'static, Result<SetFactRetentionOutput, McpToolError>> {
        Box::pin(async move {
            let engine = ctx
                .engine()
                .ok_or_else(|| McpToolError::Other("engine unavailable".into()))?;
            engine
                .set_fact_retention(&ctx.authz, &ctx.owner, args.retention_seconds)
                .await
                .map_err(|e| McpToolError::Other(e.to_string()))?;
            Ok(SetFactRetentionOutput {
                retention_seconds: args.retention_seconds,
            })
        })
    }
}
