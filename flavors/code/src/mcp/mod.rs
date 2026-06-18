mod sql;

use std::sync::Arc;

use proxima_core::{McpToolCtx, McpToolError};
use sqlx::PgPool;

pub(crate) const REPO_HANDLE_KIND: &str = "proxima-code/repo";
pub(crate) const REPO_HANDLE_PREFIX: char = 'R';

pub(crate) fn pg_pool(ctx: &McpToolCtx) -> Result<Arc<PgPool>, McpToolError> {
    ctx.extension::<PgPool>().ok_or_else(|| {
        McpToolError::Other("code flavor requires a PgPool MCP context extension".into())
    })
}

pub mod emit_execution_request;
pub mod open_file_revision;
pub mod repos;
pub mod search_chunks;
pub mod search_commits;
pub mod work_item_bundle;

pub use emit_execution_request::{
    CODE_HAS_ACCEPTANCE_CRITERIA_RELATION, CODE_TARGETS_EXECUTION_REQUEST_RELATION,
    CodeEmitExecutionPlanTool, CodeEmitExecutionRequestTool, CodeRetryExecutionRequestTool,
};
pub use open_file_revision::CodeOpenFileRevisionTool;
pub use repos::{CodeIngestHeadSnapshotTool, CodeListReposTool, CodeRegisterRepoTool};
pub use search_chunks::CodeSearchChunksTool;
pub use search_commits::CodeSearchCommitsTool;
pub use work_item_bundle::CodeWorkItemBundleTool;
