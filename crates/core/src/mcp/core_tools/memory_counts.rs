//! `proxima://memories/counts` — how many current memories the caller can
//! read, by kind.
//!
//! One per series head for Fact, Abstraction and Perspective and one per Goal
//! in any state, so a stateful series counts once however many versions it
//! has. A cheap pull for a client that wants the size of its memory before it
//! pages one.

use serde::Serialize;

use crate::mcp::{McpToolCtx, McpToolError};
use crate::read_models::MemoryCounts;

#[derive(Debug, Serialize)]
pub struct MemoryCountsOutput {
    /// Every kind is present, zero when the caller can read none of it.
    pub counts: MemoryCounts,
}

/// # Errors
///
/// Returns authorization (`Forbidden` for a context that reads nothing) or
/// storage failures.
pub async fn memory_counts(ctx: McpToolCtx) -> Result<MemoryCountsOutput, McpToolError> {
    let engine = ctx.require_engine()?;
    let counts = engine.memory_counts(&ctx.authz).await?;
    Ok(MemoryCountsOutput { counts })
}
