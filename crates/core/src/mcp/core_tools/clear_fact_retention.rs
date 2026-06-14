//! `core/clear_fact_retention` — clear the owner Fact-retention config.

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::mcp::{McpTool, McpToolCtx, McpToolError};

#[derive(Debug, Default)]
pub struct ClearFactRetentionTool;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ClearFactRetentionArgs {}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ClearFactRetentionOutput {
    pub cleared: bool,
}

impl McpTool for ClearFactRetentionTool {
    const NAME: &'static str = "core/clear_fact_retention";
    const DESCRIPTION: &'static str = "Clear the owner Fact-retention duration.";
    type Args = ClearFactRetentionArgs;
    type Output = ClearFactRetentionOutput;

    fn call(
        ctx: McpToolCtx,
        _args: ClearFactRetentionArgs,
    ) -> BoxFuture<'static, Result<ClearFactRetentionOutput, McpToolError>> {
        Box::pin(async move {
            let engine = ctx
                .engine()
                .ok_or_else(|| McpToolError::Other("engine unavailable".into()))?;
            let cleared = engine
                .clear_fact_retention(&ctx.authz, &ctx.owner)
                .await
                .map_err(|e| McpToolError::Other(e.to_string()))?;
            Ok(ClearFactRetentionOutput { cleared })
        })
    }
}
