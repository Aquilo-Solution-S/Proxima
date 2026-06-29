//! `core/get_memory` — wire-facing single-memory read by id or handle.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::engine::GetMemoryReadRequest;
use crate::mcp::{McpToolCtx, McpToolError};
use crate::{MemoryHandleClass, MemoryId};

use super::memory::search::{NeighborEdge, neighbor_edges_from_rows};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetMemoryArgs {
    /// Memory reference: `F:<uuid>`, `A:<uuid>`, `P:<uuid>`, raw uuid, or handle.
    pub memory: String,
    /// Include edges touching the memory. Default: false.
    #[serde(default)]
    pub expand_neighbors: bool,
    /// Optional display space key. Authorization resolves the entry owner.
    #[serde(default)]
    pub space: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct GetMemoryOutput {
    pub handle: String,
    pub memory: String,
    pub space: String,
    pub kind: String,
    pub schema_id: String,
    pub schema_version: u32,
    pub text: Option<String>,
    pub payload: serde_json::Value,
    pub title: Option<String>,
    pub body: Option<String>,
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub neighbor_edges: Option<Vec<NeighborEdge>>,
}

/// # Errors
///
/// Returns invalid-reference, storage, or projection failures.
pub async fn get_memory(
    ctx: McpToolCtx,
    args: GetMemoryArgs,
) -> Result<GetMemoryOutput, McpToolError> {
    let memory_id = resolve_memory_reference(&ctx, &args.memory)?;
    let output_space = args.space.unwrap_or_else(|| "entry".into());
    let engine = ctx
        .engine()
        .ok_or_else(|| McpToolError::Other("engine unavailable".into()))?;
    let response = engine
        .get_memory(
            &ctx.authz,
            &GetMemoryReadRequest {
                memory_id,
                include_neighbor_edges: args.expand_neighbors,
            },
        )
        .await?;
    let snapshot = response
        .memory
        .ok_or_else(|| McpToolError::InvalidInput(format!("memory {memory_id:?} not found")))?;
    let class = memory_class(&snapshot.kind)?;
    let handle = ctx.format_memory_with_class(snapshot.memory_id, class);
    let payload = snapshot_payload_value(snapshot.payload.as_ref())?;
    let title =
        payload_string(&payload, "title").or_else(|| payload_string(&payload, "conversation_id"));
    let body = payload_string(&payload, "body")
        .or_else(|| payload_string(&payload, "text"))
        .or_else(|| snapshot.text.clone());
    let tags = payload_tags(&payload);
    let neighbor_edges = if args.expand_neighbors {
        Some(neighbor_edges_from_rows(&ctx, response.neighbor_edges))
    } else {
        None
    };
    Ok(GetMemoryOutput {
        handle: handle.clone(),
        memory: handle,
        space: output_space,
        kind: snapshot.kind,
        schema_id: snapshot.schema_id.as_str().to_string(),
        schema_version: snapshot.schema_version.into_inner(),
        text: snapshot.text,
        payload,
        title,
        body,
        tags,
        neighbor_edges,
    })
}

/// Resolve a memory reference accepting a prefixed id (`F:…`), a handle, or a
/// bare uuid. The bare-uuid fallback serves the prefixed-id / raw-id wire
/// surfaces; the engine resolves the entry owner from storage before reading.
fn resolve_memory_reference(ctx: &McpToolCtx, raw: &str) -> Result<MemoryId, McpToolError> {
    match ctx.resolve_memory(raw) {
        Ok(memory_id) => Ok(memory_id),
        Err(resolve_err) => raw
            .parse::<uuid::Uuid>()
            .map(MemoryId::new)
            .map_err(|_| resolve_err),
    }
}

pub(super) fn snapshot_payload_value(
    payload: Option<&crate::SidecarPayload>,
) -> Result<serde_json::Value, McpToolError> {
    let Some(payload) = payload else {
        return Ok(serde_json::Value::Null);
    };
    payload
        .to_protocol_json()
        .map_err(|err| McpToolError::Other(format!("serialize typed payload: {err}")))
}

pub(super) fn memory_class(kind: &str) -> Result<MemoryHandleClass, McpToolError> {
    MemoryHandleClass::from_memory_kind(kind)
        .ok_or_else(|| McpToolError::Other(format!("unknown memory kind: {kind}")))
}

pub(super) fn payload_string(payload: &serde_json::Value, key: &str) -> Option<String> {
    payload
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned)
}

pub(super) fn payload_tags(payload: &serde_json::Value) -> Vec<String> {
    payload
        .get("tags")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|tag| tag.as_str().map(ToOwned::to_owned))
        .collect()
}
