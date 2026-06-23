//! `core_fact/tombstone` - forget one Fact and tombstone its derivatives.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::mcp::{McpToolCtx, McpToolError};
use crate::verbs::fact_cleanup::OrphanedS3Blob;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TombstoneFactArgs {
    /// `F`-handle (or prefixed id) of the Fact to forget.
    pub fact: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct TombstoneFactMcpOutput {
    pub fact_erased: bool,
    pub idempotent_replay: bool,
    pub derivatives_tombstoned: u64,
    pub cited_objects_erased: u64,
    pub orphaned_s3_blobs: Vec<OrphanedS3Blob>,
}

pub(super) async fn tombstone_fact(
    ctx: McpToolCtx,
    args: TombstoneFactArgs,
) -> Result<TombstoneFactMcpOutput, McpToolError> {
    let fact_id = ctx.resolve_fact_memory(&args.fact)?;
    let engine = ctx
        .engine()
        .ok_or_else(|| McpToolError::Other("engine unavailable".into()))?;
    let outcome = engine
        .tombstone_fact(&ctx.authz, &ctx.owner, fact_id)
        .await
        .map_err(|e| McpToolError::Other(e.to_string()))?;
    Ok(TombstoneFactMcpOutput {
        fact_erased: outcome.fact_erased,
        idempotent_replay: !outcome.fact_erased,
        derivatives_tombstoned: outcome.derivatives_tombstoned,
        cited_objects_erased: outcome.cited_objects_erased,
        orphaned_s3_blobs: outcome.orphaned_s3_blobs,
    })
}
