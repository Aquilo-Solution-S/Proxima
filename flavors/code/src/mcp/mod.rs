mod sql;

use std::sync::Arc;

use crate::CodeFlavorStore;
use proxima_core::mcp::McpToolAnnotations;
use proxima_core::{ToolCtx, ToolError};

pub(crate) const REPO_HANDLE_KIND: &str = "proxima-code/repo";
pub(crate) const REPO_HANDLE_PREFIX: char = 'R';

/// MCP behaviour hints, one set so eleven tools cannot drift on the same
/// four booleans. `ScopeGateBehavior` demands WRITE when it cannot tell
/// read-only. `open_world(false)`: every tool here reads or writes this
/// deployment's own Postgres and reaches nothing else.
pub(crate) const READ_ONLY: McpToolAnnotations =
    McpToolAnnotations::new().read_only(true).open_world(false);
pub(crate) const WRITE_IDEMPOTENT: McpToolAnnotations = McpToolAnnotations::new()
    .read_only(false)
    .destructive(false)
    .idempotent(true)
    .open_world(false);
pub(crate) const WRITE_NON_IDEMPOTENT: McpToolAnnotations = McpToolAnnotations::new()
    .read_only(false)
    .destructive(false)
    .idempotent(false)
    .open_world(false);
/// `proxima-code_erase_repo` only. Irreversible, and the one annotation a
/// client most needs before deciding what to auto-approve.
pub(crate) const DESTRUCTIVE_NON_IDEMPOTENT: McpToolAnnotations = McpToolAnnotations::new()
    .read_only(false)
    .destructive(true)
    .idempotent(false)
    .open_world(false);

pub(crate) fn code_store(ctx: &ToolCtx) -> Result<Arc<CodeFlavorStore>, ToolError> {
    ctx.service::<CodeFlavorStore>().ok_or_else(|| {
        ToolError::Other("code flavor requires a CodeFlavorStore tool service".into())
    })
}

pub(crate) fn engine(ctx: &ToolCtx) -> Result<Arc<proxima_core::Engine>, ToolError> {
    ctx.engine()
        .ok_or_else(|| ToolError::Other("code flavor tools require Engine".into()))
}

/// Wire-reference grammar on `ToolCtx` via core's extension trait.
pub(crate) use proxima_core::mcp::McpPresentationExt as CodeToolCtxExt;

pub mod emit_execution_request;
pub mod open_file_revision;
pub mod repos;
pub mod search_chunks;
pub mod search_commits;
pub mod work_item_bundle;

pub use emit_execution_request::{
    CodeEmitExecutionPlanTool, CodeEmitExecutionRequestTool, CodeRetryExecutionRequestTool,
};
pub use open_file_revision::CodeOpenFileRevisionTool;
pub use repos::{
    CodeEraseRepoTool, CodeIngestHeadSnapshotTool, CodeListReposTool, CodeRegisterRepoTool,
};
pub use search_chunks::CodeSearchChunksTool;
pub use search_commits::CodeSearchCommitsTool;
pub use work_item_bundle::CodeWorkItemBundleTool;
