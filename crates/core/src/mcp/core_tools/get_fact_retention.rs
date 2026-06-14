//! `core/get_fact_retention` — read the owner Fact-retention config.

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::mcp::{McpTool, McpToolCtx, McpToolError};

#[derive(Debug, Default)]
pub struct GetFactRetentionTool;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetFactRetentionArgs {}

#[derive(Debug, Serialize, JsonSchema)]
pub struct GetFactRetentionOutput {
    pub retention_seconds: Option<i64>,
}

impl McpTool for GetFactRetentionTool {
    const NAME: &'static str = "core/get_fact_retention";
    const DESCRIPTION: &'static str = "Get the owner Fact-retention duration, if configured.";
    type Args = GetFactRetentionArgs;
    type Output = GetFactRetentionOutput;

    fn call(
        ctx: McpToolCtx,
        _args: GetFactRetentionArgs,
    ) -> BoxFuture<'static, Result<GetFactRetentionOutput, McpToolError>> {
        Box::pin(async move {
            let engine = ctx
                .engine()
                .ok_or_else(|| McpToolError::Other("engine unavailable".into()))?;
            let retention_seconds = engine
                .get_fact_retention(&ctx.authz, &ctx.owner)
                .await
                .map_err(|e| McpToolError::Other(e.to_string()))?;
            Ok(GetFactRetentionOutput { retention_seconds })
        })
    }
}
