mod sql;

use std::sync::Arc;

use crate::CodeFlavorStore;
use proxima_core::mcp::{McpToolCaller, McpToolPresentation};
use proxima_core::{EdgeId, GoalId, MemoryId, ToolCtx, ToolError};

pub(crate) const REPO_HANDLE_KIND: &str = "proxima-code/repo";
pub(crate) const REPO_HANDLE_PREFIX: char = 'R';

/// Reject `limit: 0` on any paged read, matching core's
/// `reject_zero_limit`.
///
/// The two ends of the range are not symmetric. A limit *above* the
/// maximum is clamped, because "as many as you will give me" is still the
/// caller's intent and the page they get answers it. Zero answers nothing:
/// it produces a well-formed empty page that no client can tell apart from
/// "nothing matched".
pub(crate) fn reject_zero_limit(limit: Option<u32>) -> Result<(), ToolError> {
    if limit == Some(0) {
        return Err(ToolError::InvalidInput("limit must be at least 1".into()));
    }
    Ok(())
}

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

#[cfg(test)]
mod page_limit_tests {
    use super::reject_zero_limit;
    use proxima_core::ToolError;

    #[test]
    fn zero_is_rejected_and_omitted_is_not() {
        assert!(reject_zero_limit(Some(0)).is_err());
        assert!(reject_zero_limit(Some(1)).is_ok());
        assert!(reject_zero_limit(None).is_ok());
        assert!(reject_zero_limit(Some(u32::MAX)).is_ok());
    }

    #[test]
    fn the_rejection_is_invalid_input_not_a_server_fault() {
        let ToolError::InvalidInput(message) = reject_zero_limit(Some(0)).unwrap_err() else {
            panic!("a zero limit must be invalid input, not any other error kind");
        };
        assert!(
            message.contains("at least 1"),
            "the message must tell the caller the bound: {message}"
        );
    }
}
