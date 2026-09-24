//! Cancel-safe Fact ingest for a tool whose upstream side effect already
//! happened.

use std::time::Duration;

use proxima_core::engine::FactWrite;
use proxima_core::verbs::fact_ingest::FactIngestOutcome;
use proxima_core::{FactPayload, ToolCtx, ToolError};

/// Record the Fact for an external write a tool just made, so it survives
/// the client disconnecting.
///
/// A tool that sends a mail or pushes a commit and then ingests the Fact
/// recording it loses that Fact when the request is dropped between the
/// two: the unit of work rolls back with the dropped future. This runs the
/// ingest on a detached task (`Engine::ingest_fact_detached`) under the
/// tool's own authorization; `deadline` bounds that task. Call it after the
/// side effect, with nothing else in between.
///
/// # Errors
///
/// `ToolError::Other` when the context carries no engine; otherwise the
/// ingest's `ProtocolError`, including `Internal` when the write missed
/// `deadline` or its task did not finish.
pub async fn ingest_fact_detached<P: FactPayload + Clone>(
    ctx: &ToolCtx,
    write: FactWrite<'_, P>,
    deadline: Duration,
) -> Result<FactIngestOutcome, ToolError> {
    let engine = ctx
        .engine()
        .ok_or_else(|| ToolError::Other("tool context has no engine".into()))?;
    engine
        .ingest_fact_detached(ctx.authz(), write, deadline)
        .await
        .map_err(ToolError::Protocol)
}
