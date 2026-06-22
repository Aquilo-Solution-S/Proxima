//! `core/set_fact_retention` — set the owner Fact-retention config.

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::mcp::{McpTool, McpToolCtx, McpToolError};

#[derive(Debug, Default)]
pub struct SetFactRetentionTool;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetFactRetentionArgs {
    pub retention_seconds: Option<u64>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SetFactRetentionOutput {
    pub retention_seconds: Option<i64>,
}

impl McpTool for SetFactRetentionTool {
    const NAME: &'static str = "core_set_fact_retention";
    const DESCRIPTION: &'static str =
        "Set or clear the owner Fact-retention duration. Omit/null retention_seconds to clear.";
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
            if let Some(retention_seconds) = args.retention_seconds {
                engine
                    .set_fact_retention(&ctx.authz, &ctx.owner, retention_seconds)
                    .await
                    .map_err(|e| McpToolError::Other(e.to_string()))?;
            } else {
                engine
                    .clear_fact_retention(&ctx.authz, &ctx.owner)
                    .await
                    .map_err(|e| McpToolError::Other(e.to_string()))?;
            }
            let retention_seconds = engine
                .get_fact_retention(&ctx.authz, &ctx.owner)
                .await
                .map_err(|e| McpToolError::Other(e.to_string()))?;
            Ok(SetFactRetentionOutput { retention_seconds })
        })
    }
}
