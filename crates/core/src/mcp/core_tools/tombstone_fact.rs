//! `core_fact/tombstone` - forget one Fact and tombstone its derivatives.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::mcp::{McpToolCtx, McpToolError};
use crate::verbs::fact_cleanup::OrphanedS3Blob;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TombstoneFactArgs {
    /// `F`-handle (or prefixed id) of the Fact to forget.
    pub fact: String,
    /// Must be true to confirm destructive Fact erasure.
    pub confirm: bool,
    /// Must exactly echo `fact`.
    pub expect_handle: String,
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
    validate_confirm_gate(args.confirm, &args.expect_handle, &args.fact)?;
    let fact_id = ctx.resolve_fact_memory(&args.fact)?;
    let engine = ctx
        .engine()
        .ok_or_else(|| McpToolError::Other("engine unavailable".into()))?;
    let outcome = engine
        .tombstone_fact(&ctx.authz, &ctx.owner, fact_id)
        .await?;
    Ok(TombstoneFactMcpOutput {
        fact_erased: outcome.fact_erased,
        idempotent_replay: !outcome.fact_erased,
        derivatives_tombstoned: outcome.derivatives_tombstoned,
        cited_objects_erased: outcome.cited_objects_erased,
        orphaned_s3_blobs: outcome.orphaned_s3_blobs,
    })
}

fn validate_confirm_gate(
    confirm: bool,
    expect_handle: &str,
    fact: &str,
) -> Result<(), McpToolError> {
    if !confirm {
        return Err(McpToolError::InvalidInput("confirm must be true".into()));
    }
    if expect_handle != fact {
        return Err(McpToolError::InvalidInput(
            "expect_handle must equal fact".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::validate_confirm_gate;
    use crate::mcp::McpToolError;

    #[test]
    fn confirm_gate_requires_confirm_true() {
        match validate_confirm_gate(false, "F:target", "F:target") {
            Err(McpToolError::InvalidInput(message)) => {
                assert!(message.contains("confirm"));
            }
            other => panic!("expected confirm invalid input, got {other:?}"),
        }
    }

    #[test]
    fn confirm_gate_requires_expect_handle_match() {
        match validate_confirm_gate(true, "F:other", "F:target") {
            Err(McpToolError::InvalidInput(message)) => {
                assert!(message.contains("expect_handle"));
            }
            other => panic!("expected expect_handle invalid input, got {other:?}"),
        }
    }
}
