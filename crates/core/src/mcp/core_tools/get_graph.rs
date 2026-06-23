//! `core/get_graph` — single-shot read of the owner's full personality
//! graph plus the static catalogs that wake-entry config references.
//!
//! Composes the data that would otherwise require five round trips
//! (`list_personalities` + `get_personality` per P, `list_schemas`,
//! `list_edge_types`, `list_substrate_tools`) into one atomic response. The
//! personality projection mirrors `get_personality` and the catalog
//! projections mirror their respective `list_*` tools so the shapes
//! already familiar to the frontend stay intact.

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::McpTool;
use crate::mcp::{McpToolCtx, McpToolError};
use crate::verbs::schema::PayloadKind;

use super::get_personality::{GetPersonalityOutput, GetPersonalityWakeEntry};
use super::list_edge_types::EdgeTypeItem;
use super::list_schemas::SchemaItem;
use super::list_substrate_tools::SubstrateToolItem;

#[derive(Debug, Default)]
pub struct GetGraphTool;

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct GetGraphArgs {
    /// Include tombstoned personalities. Default: false.
    #[serde(default)]
    pub include_tombstoned: bool,
}

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
    /// Owner Fact-retention duration in seconds, if configured.
    pub fact_retention_seconds: Option<i64>,
    /// Every personality the owner owns, fully expanded — same shape as
    /// `core/get_personality` output.
    pub personalities: Vec<GetPersonalityOutput>,
    /// Static schema catalog from the frozen `FlavorRegistry`.
    pub schemas: Vec<SchemaItem>,
    /// Static edge-type catalog from the frozen `FlavorRegistry`.
    pub edge_types: Vec<EdgeTypeItem>,
    /// Dispatchable substrate and flavor-registered MCP tool ids.
    pub substrate_tools: Vec<SubstrateToolItem>,
}

fn kind_str(k: PayloadKind) -> &'static str {
    match k {
        PayloadKind::Fact => "Fact",
        PayloadKind::Abstraction => "Abstraction",
        PayloadKind::Perspective => "Perspective",
        PayloadKind::Goal => "Goal",
        PayloadKind::Edge => "Edge",
        PayloadKind::CitedObject => "CitedObject",
        PayloadKind::CitationMapping => "CitationMapping",
    }
}

impl McpTool for GetGraphTool {
    const NAME: &'static str = "core_get_graph";
    const DESCRIPTION: &'static str = "Single-shot read of the owner's full personality graph plus the catalogs that wake-entry \
         config references (schemas, edge types, substrate tools). Use this in place of five separate list_/get_ round trips when \
         rendering a graph view. Args: `{\"include_tombstoned\": false}` (default).";
    type Args = GetGraphArgs;
    type Output = GetGraphOutput;

    fn call(
        ctx: McpToolCtx,
        args: GetGraphArgs,
    ) -> BoxFuture<'static, Result<GetGraphOutput, McpToolError>> {
        Box::pin(get_graph(ctx, args))
    }
}

pub(crate) async fn get_graph(
    ctx: McpToolCtx,
    args: GetGraphArgs,
) -> Result<GetGraphOutput, McpToolError> {
    let storage = ctx
        .storage()
        .ok_or_else(|| McpToolError::Other("engine storage unavailable".into()))?;
    let embeddings_client_configured = ctx
        .engine()
        .and_then(crate::engine::Engine::embed_client)
        .is_some();
    let pending_embedding_jobs = storage
        .count_pending_embedding_jobs(&ctx.owner)
        .await
        .map_err(McpToolError::Storage)?;
    let engine = ctx
        .engine()
        .ok_or_else(|| McpToolError::Other("engine unavailable".into()))?;
    let fact_retention_seconds = engine
        .get_fact_retention(&ctx.authz, &ctx.owner)
        .await
        .map_err(|err| McpToolError::Other(err.to_string()))?;

    let personality_rows = storage
        .list_personality_instances(&ctx.owner, args.include_tombstoned)
        .await
        .map_err(McpToolError::Storage)?;
    let personalities = personality_rows
        .into_iter()
        .map(|row| {
            let personality = ctx.format_personality(row.personality_instance_id);
            let root_perspective =
                ctx.format_perspective_memory(row.current_root_perspective_memory_id);
            let wake_entries = row
                .wake_entries
                .into_iter()
                .map(|e| GetPersonalityWakeEntry {
                    wake_entry: ctx.format_wake_entry(e.wake_entry_id),
                    trigger_kind: e.trigger_kind.as_str().to_string(),
                    trigger_id: e.trigger_id,
                    label: e.label,
                    enabled: e.enabled,
                    instructions: e.instructions,
                    authored_by: e.authored_by.as_str().to_string(),
                    probability_promille: e.probability_promille,
                    goal_scope: e.goal_scope.as_str().to_string(),
                })
                .collect();
            GetPersonalityOutput {
                personality,
                display_name: row.display_name,
                status: row.status,
                root_perspective,
                wake_entries,
            }
        })
        .collect();

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

    let edge_types = ctx
        .registry
        .list_relations()
        .iter()
        .map(|rel| EdgeTypeItem {
            edge_type: rel.relation.clone(),
            class: rel.class.as_str().to_string(),
        })
        .collect();

    let substrate_tools = scoped_substrate_tools(&ctx);

    Ok(GetGraphOutput {
        embeddings_client_configured,
        pending_embedding_jobs,
        fact_retention_seconds,
        personalities,
        schemas,
        edge_types,
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
                .capabilities
                .tool_scope
                .allows_group_advertisement(desc.name)
        })
        .map(|desc| {
            let source = if desc.name.starts_with("core/") {
                "substrate".into()
            } else {
                let flavor = desc.name.split('/').next().unwrap_or("flavor");
                format!("flavor:{flavor}")
            };
            SubstrateToolItem {
                tool_id: desc.name.to_string(),
                source,
                description: desc.description.to_string(),
            }
        })
        .collect()
}
