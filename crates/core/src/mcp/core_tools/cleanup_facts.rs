//! `core/cleanup_facts` — run the owner Fact-retention sweep.

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::mcp::{McpTool, McpToolCtx, McpToolError};

#[derive(Debug, Default)]
pub struct CleanupFactsTool;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CleanupFactsArgs {}

#[derive(Debug, Serialize, JsonSchema)]
pub struct CleanupFactsOutput {
    pub facts_erased: u64,
    pub derivatives_tombstoned: u64,
}

impl McpTool for CleanupFactsTool {
    const NAME: &'static str = "core/cleanup_facts";
    const DESCRIPTION: &'static str =
        "Hard-erase due owner Facts and tombstone direct derived memory dependents.";
    type Args = CleanupFactsArgs;
    type Output = CleanupFactsOutput;

    fn call(
        ctx: McpToolCtx,
        _args: CleanupFactsArgs,
    ) -> BoxFuture<'static, Result<CleanupFactsOutput, McpToolError>> {
        Box::pin(async move {
            let engine = ctx
                .engine()
                .ok_or_else(|| McpToolError::Other("engine unavailable".into()))?;
            let outcome = engine
                .cleanup_due_facts(&ctx.authz, &ctx.owner)
                .await
                .map_err(|e| McpToolError::Other(e.to_string()))?;
            Ok(CleanupFactsOutput {
                facts_erased: outcome.facts_erased,
                derivatives_tombstoned: outcome.derivatives_tombstoned,
            })
        })
    }
}
