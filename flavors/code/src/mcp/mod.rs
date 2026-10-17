mod sql;

use std::sync::Arc;

use crate::CodeFlavorStore;
use proxima_core::{ToolCtx, ToolError};

pub(crate) fn code_store(
    ctx: &ToolCtx,
) -> Result<Arc<crate::store::ScopedCodeFlavorStore>, ToolError> {
    let store = ctx.service::<CodeFlavorStore>().ok_or_else(|| {
        ToolError::Other("code flavor requires a CodeFlavorStore tool service".into())
    })?;
    Ok(Arc::new(
        store.as_ref().clone().with_owner_scope(
            ctx.authz()
                .owner_scope()
                .cloned()
                .ok_or_else(|| ToolError::Other("authenticated owner scope required".into()))?,
        ),
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

/// Parse the memory UUID; each operation's authorized read validates its stored kind.
pub(crate) fn parse_memory_uuid(raw: &str) -> Result<proxima_core::MemoryId, ToolError> {
    parse_uuid(raw).map(proxima_core::MemoryId::new)
}

pub(crate) fn parse_uuid(raw: &str) -> Result<uuid::Uuid, ToolError> {
    proxima_core::mcp::parse_wire_uuid(raw).map_err(|err| ToolError::InvalidInput(err.to_string()))
}

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
    CodeRepoRoots,
};
pub use search_chunks::CodeSearchChunksTool;
pub use search_commits::CodeSearchCommitsTool;
pub use work_item_bundle::CodeWorkItemBundleTool;

#[cfg(test)]
mod tests {
    use super::*;
    use proxima_core::{AuthPath, AuthzContext, FlavorRegistry, ToolServices, UserId};

    #[tokio::test]
    async fn an_unverified_tool_context_cannot_bind_a_store() {
        let owner = UserId::new(uuid::Uuid::now_v7()).into();
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://unused:unused@127.0.0.1:1/unused")
            .expect("valid fixture URL");
        let ctx = ToolCtx::new(
            owner,
            AuthzContext::single_owner(&owner, AuthPath::HostBearer),
            Arc::new(FlavorRegistry::new().freeze_or_panic_for_tests()),
            ToolServices::with(CodeFlavorStore::from_backend_pool_for_tests(pool)),
        );

        let error = code_store(&ctx).expect_err("unverified scope must be refused before SQL");
        assert!(
            matches!(error, ToolError::Other(message) if message == "authenticated owner scope required")
        );
    }
}
