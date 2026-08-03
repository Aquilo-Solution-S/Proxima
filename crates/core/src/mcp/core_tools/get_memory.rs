//! `core/get_memory` — wire-facing single-memory read by prefixed id.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::MemoryHandleClass;
use crate::engine::GetMemoryReadRequest;
use crate::mcp::{McpToolCtx, McpToolError};

use super::memory::search::{NeighborEdge, neighbor_edges_from_rows};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetMemoryArgs {
    /// Memory reference: `F:<uuid>`, `A:<uuid>`, or `P:<uuid>`.
    pub memory: String,
    /// Include edges touching the memory. Default: false.
    #[serde(default)]
    pub expand_neighbors: bool,
    /// Optional space key from `core_memory_spaces`. Authorization resolves
    /// the entry owner; this only selects how the space is reported back.
    #[serde(default)]
    pub space: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
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
    let memory_id = ctx.resolve_memory(&args.memory)?;
    // Report a key `core_memory_spaces` actually advertises. This used to
    // default to the literal "entry", which no space is called: an agent
    // following the documented "use a returned `space` key" loop fed it back
    // to a write and got `unknown memory space: entry`.
    let output_space = super::memory_spaces::resolve_space_owner(
        &ctx,
        args.space.as_deref(),
        super::memory_spaces::SpaceDefault::Current,
    )?
    .key;
    let engine = ctx.require_engine()?;
    let response = engine
        .get_memory(
            &ctx.authz,
            &GetMemoryReadRequest {
                memory_id,
                include_neighbor_edges: args.expand_neighbors,
            },
        )
        .await
        .map_err(|err| {
            // Re-word the engine's handle-less not-found with the wire
            // handle the caller actually passed.
            let err = McpToolError::from(err);
            if err.kind() == crate::mcp::McpToolErrorKind::NotFound {
                McpToolError::NotFound(format!("memory {} not found", args.memory))
            } else {
                err
            }
        })?;
    let snapshot = response
        .memory
        .ok_or_else(|| McpToolError::NotFound(format!("memory {} not found", args.memory)))?;
    let neighbor_edges = if args.expand_neighbors {
        Some(neighbor_edges_from_rows(&ctx, response.neighbor_edges))
    } else {
        None
    };
    project_memory_snapshot(&ctx, snapshot, output_space, neighbor_edges)
}

/// Project a storage snapshot into the wire output shape shared by
/// `get_memory`, the `proxima://memories` batch read, and
/// `facts_citing_object`.
pub(super) fn project_memory_snapshot(
    ctx: &McpToolCtx,
    snapshot: crate::read_models::MemorySnapshot,
    space: String,
    neighbor_edges: Option<Vec<NeighborEdge>>,
) -> Result<GetMemoryOutput, McpToolError> {
    let class = memory_class(&snapshot.kind)?;
    let handle = ctx.format_memory_with_class(snapshot.memory_id, class);
    let payload = snapshot_payload_value(snapshot.payload.as_ref())?;
    let title =
        payload_string(&payload, "title").or_else(|| payload_string(&payload, "conversation_id"));
    let body = payload_string(&payload, "body")
        .or_else(|| payload_string(&payload, "text"))
        .or_else(|| snapshot.text.clone());
    let tags = payload_tags(&payload);
    Ok(GetMemoryOutput {
        handle: handle.clone(),
        memory: handle,
        space,
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
