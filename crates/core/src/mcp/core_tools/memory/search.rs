use std::collections::BTreeMap;

use crate::mcp::core_tools::get_memory::{sidecar_specs, snapshot_payload_value};
use crate::mcp::{McpTool, McpToolCtx, McpToolError};
use crate::{MemoryId, PersonalityInstanceId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize)]
pub struct NeighborEdge {
    pub handle: String,
    pub relation: String,
    pub source: Option<String>,
    pub target: Option<String>,
}

pub(crate) async fn load_graph_payloads(
    ctx: &McpToolCtx,
    memory_ids: &[uuid::Uuid],
) -> Result<BTreeMap<uuid::Uuid, GraphPayloadRow>, McpToolError> {
    if memory_ids.is_empty() {
        return Ok(BTreeMap::new());
    }
    let storage = ctx
        .storage()
        .ok_or_else(|| McpToolError::Other("engine storage unavailable".into()))?;
    let ids = memory_ids
        .iter()
        .copied()
        .map(MemoryId::new)
        .collect::<Vec<_>>();
    let rows = storage.load_memory_graph_payloads(&ctx.owner, &ids).await?;
    Ok(rows
        .into_iter()
        .map(|row| {
            let memory_id = row.memory_id.into_inner();
            (memory_id, GraphPayloadRow { tags: row.tags })
        })
        .collect())
}

#[derive(Debug)]
pub(crate) struct GraphPayloadRow {
    pub(crate) tags: Option<Vec<String>>,
}

/// # Errors
///
/// Returns storage errors from the owner-filtered edge query.
pub async fn neighbor_edges(
    ctx: &McpToolCtx,
    memory_ids: &[uuid::Uuid],
) -> Result<Vec<NeighborEdge>, McpToolError> {
    if memory_ids.is_empty() {
        return Ok(Vec::new());
    }
    let storage = ctx
        .storage()
        .ok_or_else(|| McpToolError::Other("engine storage unavailable".into()))?;
    let ids = memory_ids
        .iter()
        .copied()
        .map(MemoryId::new)
        .collect::<Vec<_>>();
    let rows = storage
        .load_neighbor_memory_edges(&ctx.owner, &ids, 200)
        .await?;

    Ok(rows
        .into_iter()
        .map(|row| NeighborEdge {
            handle: ctx.format_edge(row.edge_id),
            relation: row.relation,
            source: row
                .source_memory_id
                .map(|id| format_memory_by_kind(ctx, id, row.source_kind)),
            target: row
                .target_memory_id
                .map(|id| format_memory_by_kind(ctx, id, row.target_kind)),
        })
        .collect())
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct OpenArgs {
    #[schemars(
        description = "`F...`, `A...`, or `P...` memory handle to open and inspect with neighbor edges."
    )]
    pub handle: String,
}

fn format_memory_by_kind(ctx: &McpToolCtx, memory_id: MemoryId, kind: crate::EntityKind) -> String {
    match kind {
        crate::EntityKind::Abstraction => ctx.format_abstraction_memory(memory_id),
        crate::EntityKind::Perspective => ctx.format_perspective_memory(memory_id),
        _ => ctx.format_fact_memory(memory_id),
    }
}

fn format_memory_by_kind_label(ctx: &McpToolCtx, memory_id: MemoryId, kind: &str) -> String {
    match kind {
        "Abstraction" => ctx.format_abstraction_memory(memory_id),
        "Perspective" => ctx.format_perspective_memory(memory_id),
        _ => ctx.format_fact_memory(memory_id),
    }
}

fn format_authoring_personality(
    ctx: &McpToolCtx,
    instance_id: Option<PersonalityInstanceId>,
) -> Option<String> {
    instance_id.map(|id| ctx.format_personality(id))
}

#[derive(Debug, Serialize)]
pub struct OpenOutput {
    pub handle: String,
    pub kind: String,
    pub schema_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authoring_personality_instance_id: Option<String>,
    pub title: Option<String>,
    pub body: Option<String>,
    pub tags: Vec<String>,
    pub neighbor_edges: Vec<NeighborEdge>,
}

#[derive(Debug)]
pub struct OpenTool;

impl McpTool for OpenTool {
    const NAME: &'static str = "core/open";
    const DESCRIPTION: &'static str = "Resolve a memory handle to its payload and neighbor edges.";
    type Args = OpenArgs;
    type Output = OpenOutput;

    fn call(
        ctx: McpToolCtx,
        args: OpenArgs,
    ) -> futures::future::BoxFuture<'static, Result<OpenOutput, McpToolError>> {
        Box::pin(async move {
            let memory_id = ctx.resolve_memory(&args.handle)?;
            let memory_uuid = memory_id.into_inner();
            let storage = ctx
                .storage()
                .ok_or_else(|| McpToolError::Other("engine storage unavailable".into()))?;
            let sidecars = sidecar_specs(&ctx);
            let snapshot = storage
                .load_memory_by_id(&ctx.owner, memory_id, None, &sidecars)
                .await?
                .ok_or_else(|| {
                    McpToolError::InvalidInput(format!("memory {memory_uuid} not found"))
                })?;
            let neighbor_edges = neighbor_edges(&ctx, &[memory_uuid]).await?;
            let payload = snapshot_payload_value(snapshot.payload.as_ref())?;
            let title = payload_string(&payload, "title")
                .or_else(|| payload_string(&payload, "conversation_id"));
            let body = payload_string(&payload, "body")
                .or_else(|| payload_string(&payload, "text"))
                .or_else(|| snapshot.text.clone());
            let tags = payload_tags(&payload);
            Ok(OpenOutput {
                handle: format_memory_by_kind_label(&ctx, snapshot.memory_id, &snapshot.kind),
                kind: snapshot.kind,
                schema_id: snapshot.schema_id.as_str().to_string(),
                authoring_personality_instance_id: format_authoring_personality(
                    &ctx,
                    snapshot.authoring_personality_instance_id,
                ),
                title,
                body,
                tags,
                neighbor_edges,
            })
        })
    }
}

fn payload_string(payload: &serde_json::Value, key: &str) -> Option<String> {
    payload
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned)
}

fn payload_tags(payload: &serde_json::Value) -> Vec<String> {
    payload
        .get("tags")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|tag| tag.as_str().map(ToOwned::to_owned))
        .collect()
}
