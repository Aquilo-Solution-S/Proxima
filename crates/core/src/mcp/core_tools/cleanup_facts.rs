//! `core/cleanup_facts` — run the owner Fact-retention sweep.

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::mcp::{McpTool, McpToolCtx, McpToolError};
use crate::verbs::fact_cleanup::OrphanedS3Blob;

#[derive(Debug, Default)]
pub struct CleanupFactsTool;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CleanupFactsArgs {}

#[derive(Debug, Serialize, JsonSchema)]
pub struct CleanupFactsOutput {
    pub facts_erased: u64,
    pub derivatives_tombstoned: u64,
    pub cited_objects_erased: u64,
    pub orphaned_s3_blobs: Vec<OrphanedS3Blob>,
}

impl McpTool for CleanupFactsTool {
    const NAME: &'static str = "core_cleanup_facts";
    const DESCRIPTION: &'static str = "Hard-erase due owner Facts, tombstone transitive derived memory dependents, and garbage-collect orphaned citation backing rows while surfacing orphaned S3 blob references.";
    type Args = CleanupFactsArgs;
    type Output = CleanupFactsOutput;

    fn call(
        ctx: McpToolCtx,
        args: CleanupFactsArgs,
    ) -> BoxFuture<'static, Result<CleanupFactsOutput, McpToolError>> {
        Box::pin(cleanup_facts(ctx, args))
    }
}

pub(super) async fn cleanup_facts(
    ctx: McpToolCtx,
    _args: CleanupFactsArgs,
) -> Result<CleanupFactsOutput, McpToolError> {
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
        cited_objects_erased: outcome.cited_objects_erased,
        orphaned_s3_blobs: outcome.orphaned_s3_blobs,
    })
}
