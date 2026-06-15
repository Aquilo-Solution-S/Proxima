use std::collections::BTreeMap;

use crate::mcp::core_tools::get_memory::sidecar_specs;
use crate::mcp::{McpTool, McpToolCtx, McpToolError};
use crate::{EdgeId, MemoryId, PersonalityInstanceId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::util::{map_storage, owner_principal};

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
    let (owner_kind, owner_principal_id) = owner_principal(&ctx.owner);
    let rows: Vec<GraphPayloadRow> = sqlx::query_as(
        "SELECT m.memory_id,
                COALESCE(n.tags, d.tags) AS tags
         FROM proxima_core.memories m
         LEFT JOIN proxima_core.agent_note_v1 n USING (memory_id)
         LEFT JOIN proxima_core.agent_derivation_v1 d USING (memory_id)
         WHERE m.owner_principal_kind = $1
           AND m.owner_principal_id = $2
           AND m.memory_id = ANY($3::uuid[])",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(memory_ids)
    .fetch_all(&ctx.pool)
    .await
    .map_err(map_storage)?;
    Ok(rows.into_iter().map(|row| (row.memory_id, row)).collect())
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct GraphPayloadRow {
    pub(crate) memory_id: uuid::Uuid,
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
    let (owner_kind, owner_principal_id) = owner_principal(&ctx.owner);
    let rows: Vec<EdgeRow> = sqlx::query_as(
        "SELECT edge_id, relation, source_kind, source_memory_id, target_kind, target_memory_id
         FROM proxima_core.edges
         WHERE owner_principal_kind = $1
           AND owner_principal_id = $2
           AND (source_memory_id = ANY($3) OR target_memory_id = ANY($3))
         ORDER BY edge_id DESC
         LIMIT 200",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(memory_ids)
    .fetch_all(&ctx.pool)
    .await
    .map_err(map_storage)?;

    Ok(rows
        .into_iter()
        .map(|row| NeighborEdge {
            handle: ctx.format_edge(EdgeId::new(row.edge_id)),
            relation: row.relation,
            source: row
                .source_memory_id
                .map(|id| format_memory_by_kind(ctx, MemoryId::new(id), row.source_kind)),
            target: row
                .target_memory_id
                .map(|id| format_memory_by_kind(ctx, MemoryId::new(id), row.target_kind)),
        })
        .collect())
}

#[derive(Debug, sqlx::FromRow)]
struct EdgeRow {
    edge_id: uuid::Uuid,
    relation: String,
    source_kind: crate::EntityKind,
    source_memory_id: Option<uuid::Uuid>,
    target_kind: crate::EntityKind,
    target_memory_id: Option<uuid::Uuid>,
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
            let title = payload_string(&snapshot.payload_json, "title")
                .or_else(|| payload_string(&snapshot.payload_json, "conversation_id"));
            let body = payload_string(&snapshot.payload_json, "body")
                .or_else(|| payload_string(&snapshot.payload_json, "text"))
                .or_else(|| snapshot.text.clone());
            let tags = payload_tags(&snapshot.payload_json);
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
