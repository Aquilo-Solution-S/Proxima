mod sql;

use std::sync::Arc;

use crate::CodeFlavorStore;
use proxima_core::{ToolCtx, ToolError};

pub(crate) const REPO_HANDLE_KIND: &str = "proxima-code/repo";
pub(crate) const REPO_HANDLE_PREFIX: char = 'R';

pub(crate) fn code_store(ctx: &ToolCtx) -> Result<Arc<CodeFlavorStore>, ToolError> {
    let store = ctx.service::<CodeFlavorStore>().ok_or_else(|| {
        ToolError::Other("code flavor requires a CodeFlavorStore tool service".into())
    })?;
    Ok(Arc::new(
        store
            .as_ref()
            .clone()
            .with_owner_scope(ctx.authz().owner_scope().cloned()),
    ))
}

pub(crate) fn engine(ctx: &ToolCtx) -> Result<Arc<proxima_core::Engine>, ToolError> {
    ctx.engine()
        .ok_or_else(|| ToolError::Other("code flavor tools require Engine".into()))
}

/// The caller's resolved read set, as stored owner ids, for the lexical
/// candidate scans to bind into `projection.owner_id = ANY($n)`.
///
/// A scan narrowed this way still overfetches and still admits every
/// candidate through `Engine::query`; what the predicate buys is the
/// projection's composite `gin(owner_id, search_tsv)`, which no join to a
/// sidecar's owner can reach.
pub(crate) async fn read_owner_ids(
    engine: &proxima_core::Engine,
    ctx: &ToolCtx,
) -> Result<Vec<uuid::Uuid>, ToolError> {
    proxima::flavor::read_owner_ids(engine, ctx.authz()).await
}

/// Wire-reference grammar on `ToolCtx` via core's extension trait.
pub(crate) use proxima_core::mcp::McpPresentationExt as CodeToolCtxExt;

pub mod emit_execution_request;
pub mod ingest_runs;
pub mod open_file_revision;
pub mod repos;
pub mod search_chunks;
pub mod search_commits;
pub mod work_item_bundle;

pub use emit_execution_request::{
    CodeEmitExecutionPlanTool, CodeEmitExecutionRequestTool, CodeRetryExecutionRequestTool,
};
pub use ingest_runs::{CodeGetIngestRunTool, CodeStartIngestHeadSnapshotTool};
pub use open_file_revision::CodeOpenFileRevisionTool;
pub use repos::{
    CodeEraseRepoTool, CodeIngestHeadSnapshotTool, CodeListReposTool, CodeRegisterRepoTool,
};
pub use search_chunks::CodeSearchChunksTool;
pub use search_commits::CodeSearchCommitsTool;
pub use work_item_bundle::CodeWorkItemBundleTool;
