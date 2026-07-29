mod sql;

use std::sync::Arc;

use crate::CodeFlavorStore;
use proxima_core::mcp::{McpToolAnnotations, McpToolCaller, McpToolPresentation};
use proxima_core::{EdgeId, GoalId, MemoryId, ToolCtx, ToolError};

pub(crate) const REPO_HANDLE_KIND: &str = "proxima-code/repo";
pub(crate) const REPO_HANDLE_PREFIX: char = 'R';

/// MCP behaviour hints, one set of constants so eleven tools cannot drift
/// apart on the same four booleans. Mirrors core's `core_tools::READ_ONLY`
/// and friends.
///
/// These are load-bearing, not decorative. `ScopeGateBehavior`'s owner-role
/// check asks whether a tool is read-only and demands WRITE access when it
/// cannot tell — so before this flavor declared anything, a viewer was
/// refused `proxima-code_search_chunks`. `open_world(false)` on all of them
/// is true by construction: every one of these tools reads or writes this
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

pub(crate) fn caller(ctx: &ToolCtx) -> Result<Arc<McpToolCaller>, ToolError> {
    ctx.service::<McpToolCaller>()
        .ok_or_else(|| ToolError::Other("code flavor MCP tools require caller metadata".into()))
}

fn presentation(ctx: &ToolCtx) -> Result<Arc<McpToolPresentation>, ToolError> {
    ctx.service::<McpToolPresentation>().ok_or_else(|| {
        ToolError::Other("code flavor MCP tools require presentation service".into())
    })
}

pub(crate) trait CodeToolCtxExt {
    fn format_fact_memory(&self, id: MemoryId) -> String;
    fn format_abstraction_memory(&self, id: MemoryId) -> String;
    fn format_perspective_memory(&self, id: MemoryId) -> String;
    fn format_goal(&self, id: GoalId) -> String;
    fn format_edge(&self, id: EdgeId) -> String;
    fn format_flavor_object(&self, kind: &str, id: uuid::Uuid, prefix: char) -> String;
    fn resolve_fact_memory(&self, raw: &str) -> Result<MemoryId, ToolError>;
    fn resolve_abstraction_memory(&self, raw: &str) -> Result<MemoryId, ToolError>;
    fn resolve_perspective_memory(&self, raw: &str) -> Result<MemoryId, ToolError>;
    fn resolve_flavor_object(&self, raw: &str, kind: &str) -> Result<uuid::Uuid, ToolError>;
}

impl CodeToolCtxExt for ToolCtx {
    fn format_fact_memory(&self, id: MemoryId) -> String {
        presentation(self)
            .expect("MCP presentation service must be present")
            .format_fact_memory(id)
    }

    fn format_abstraction_memory(&self, id: MemoryId) -> String {
        presentation(self)
            .expect("MCP presentation service must be present")
            .format_abstraction_memory(id)
    }

    fn format_perspective_memory(&self, id: MemoryId) -> String {
        presentation(self)
            .expect("MCP presentation service must be present")
            .format_perspective_memory(id)
    }

    fn format_goal(&self, id: GoalId) -> String {
        presentation(self)
            .expect("MCP presentation service must be present")
            .format_goal(id)
    }

    fn format_edge(&self, id: EdgeId) -> String {
        presentation(self)
            .expect("MCP presentation service must be present")
            .format_edge(id)
    }

    fn format_flavor_object(&self, kind: &str, id: uuid::Uuid, prefix: char) -> String {
        presentation(self)
            .expect("MCP presentation service must be present")
            .format_flavor_object(kind, id, prefix)
    }

    fn resolve_fact_memory(&self, raw: &str) -> Result<MemoryId, ToolError> {
        presentation(self)?.resolve_fact_memory(raw)
    }

    fn resolve_abstraction_memory(&self, raw: &str) -> Result<MemoryId, ToolError> {
        presentation(self)?.resolve_abstraction_memory(raw)
    }

    fn resolve_perspective_memory(&self, raw: &str) -> Result<MemoryId, ToolError> {
        presentation(self)?.resolve_perspective_memory(raw)
    }

    fn resolve_flavor_object(&self, raw: &str, kind: &str) -> Result<uuid::Uuid, ToolError> {
        presentation(self)?.resolve_flavor_object(raw, kind)
    }
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
pub use repos::{
    CodeEraseRepoTool, CodeIngestHeadSnapshotTool, CodeListReposTool, CodeRegisterRepoTool,
};
pub use search_chunks::CodeSearchChunksTool;
pub use search_commits::CodeSearchCommitsTool;
pub use work_item_bundle::CodeWorkItemBundleTool;
