//! `core/get_graph` — single-shot read of owner graph metadata plus
//! static catalogs.
//!
//! There is no edge-type catalog here any more: the edge vocabulary is
//! closed at two kinds and lives in docs/16, not in a registry a client
//! has to enumerate.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::engine::GetGraphReadRequest;
use crate::mcp::{McpToolCtx, McpToolError};
use crate::verbs::schema::PayloadKind;

use super::list_schemas::SchemaItem;
use super::list_substrate_tools::{
    SubstrateToolItem, substrate_tool_actions, substrate_tool_source,
};

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct GetGraphArgs {}

#[derive(Debug, Serialize, JsonSchema)]
pub struct GetGraphOutput {
    /// Whether an embedding client is installed (semantic/hybrid search can
    /// embed queries). Note: `true` does NOT imply stored vectors exist —
    /// check `pending_embedding_jobs`.
    pub embeddings_client_configured: bool,
    /// Counts the owner's embedding jobs in `pending` or `processing` state
    /// across all embedding models; excludes permanently-`failed` jobs, so `0`
    /// means no retryable/in-flight backlog remains (not a proof that every
    /// memory embedded successfully).
    pub pending_embedding_jobs: u64,
    /// Counts the owner's embedding jobs in the terminal `failed` state (retries
    /// exhausted). A non-zero value means some Facts are stuck without an
    /// embedding until a `reconcile` requeues them — an operator signal on this
    /// readiness resource.
    pub failed_embedding_jobs: u64,
    /// Static schema catalog from the frozen `FlavorRegistry`.
    pub schemas: Vec<SchemaItem>,
    /// Dispatchable substrate and flavor-registered MCP tool ids.
    pub substrate_tools: Vec<SubstrateToolItem>,
}

fn kind_str(k: PayloadKind) -> &'static str {
    match k {
        PayloadKind::Fact => "Fact",
        PayloadKind::Abstraction => "Abstraction",
        PayloadKind::Perspective => "Perspective",
        PayloadKind::Goal => "Goal",
        PayloadKind::CitedObject => "CitedObject",
        PayloadKind::CitationMapping => "CitationMapping",
    }
}

/// # Errors
///
/// Returns storage, engine, or projection failures.
pub async fn get_graph(
    ctx: McpToolCtx,
    _args: GetGraphArgs,
) -> Result<GetGraphOutput, McpToolError> {
    let engine = ctx.require_engine()?;
    let embeddings_client_configured = engine.embed_client().is_some();
    let graph = engine
        .get_graph(&ctx.authz, &GetGraphReadRequest { owner: ctx.owner })
        .await?;

    let schemas = ctx
        .registry
        .list()
        .into_iter()
        .map(|info| SchemaItem {
            schema_id: info.schema_id.as_str().to_string(),
            schema_version: info.schema_version.into_inner(),
            kind: kind_str(info.kind).to_string(),
        })
        .collect();

    let substrate_tools = scoped_substrate_tools(&ctx);

    Ok(GetGraphOutput {
        embeddings_client_configured,
        pending_embedding_jobs: graph.pending_embedding_jobs,
        failed_embedding_jobs: graph.failed_embedding_jobs,
        schemas,
        substrate_tools,
    })
}

/// Build the owner's substrate-tool inventory, filtered to the caller's
/// `ToolScope` so a deployment profile that hides tools is not defeated by
/// `get_graph` re-advertising them.
fn scoped_substrate_tools(ctx: &McpToolCtx) -> Vec<SubstrateToolItem> {
    ctx.registry
        .list_mcp_tools()
        .iter()
        .filter(|desc| {
            ctx.authz
                .tool_scope()
                .allows_tool_advertisement(desc.name, !desc.action_arg_specs.is_empty())
        })
        .map(|desc| SubstrateToolItem {
            tool_id: desc.name.to_string(),
            source: substrate_tool_source(desc),
            description: desc.description.to_string(),
            actions: substrate_tool_actions(ctx, desc),
        })
        .collect()
}
