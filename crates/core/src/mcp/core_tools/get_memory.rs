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
    /// When the observation occurred (a backdated import's `observed_at`,
    /// else the write time), in the RFC 3339 form `core_search_memories`
    /// gives `created_at`. Present on every Fact of this store.
    #[serde(
        with = "time::serde::rfc3339::option",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "Option<String>")]
    pub occurred_at: Option<time::OffsetDateTime>,
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
    if let Some(origin) = super::other_stores::qualified(&ctx, &args.memory) {
        return remote_memory_output(&ctx, &origin).await;
    }
    let memory_id = ctx.resolve_memory(&args.memory)?;
    // Space key `core_memory_spaces` advertises (`SpaceDefault::Current`).
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

/// `<store>:<handle>` read through the other-stores port, as the caller.
/// The space is the store's name; a remote memory has no local payload,
/// version or neighbor edges.
async fn remote_memory_output(
    ctx: &McpToolCtx,
    origin: &crate::storage_ports::other_stores::QualifiedHandle,
) -> Result<GetMemoryOutput, McpToolError> {
    let (_, memory) = super::other_stores::read_origin(ctx, origin).await?;
    let handle = origin.to_string();
    Ok(GetMemoryOutput {
        handle: handle.clone(),
        memory: handle,
        space: origin.store.to_string(),
        kind: memory.kind.as_str().to_string(),
        schema_id: memory.schema_id,
        schema_version: 1,
        occurred_at: None,
        text: Some(memory.body.clone()),
        payload: serde_json::Value::Null,
        title: memory.title,
        body: Some(memory.body),
        tags: memory.tags,
        neighbor_edges: None,
    })
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
    let class = memory_class(snapshot.kind)?;
    let handle = ctx.format_memory_with_class(snapshot.memory_id, class);
    let payload = snapshot_payload_value(snapshot.payload.as_ref())?;
    let title =
        payload_string(&payload, "title").or_else(|| payload_string(&payload, "conversation_id"));
    let body = snapshot
        .payload
        .as_ref()
        .and_then(crate::SidecarPayload::graph_body)
        .or_else(|| snapshot.text.clone());
    let tags = snapshot
        .payload
        .as_ref()
        .map_or_else(Vec::new, crate::SidecarPayload::graph_tags);
    Ok(GetMemoryOutput {
        handle: handle.clone(),
        memory: handle,
        space,
        kind: snapshot.kind.as_str().to_string(),
        schema_id: snapshot.schema_id.as_str().to_string(),
        schema_version: snapshot.schema_version.into_inner(),
        occurred_at: snapshot.occurred_at,
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

pub(super) fn memory_class(kind: crate::EntityKind) -> Result<MemoryHandleClass, McpToolError> {
    MemoryHandleClass::from_entity_kind(kind)
        .ok_or_else(|| McpToolError::Other(format!("unknown memory kind: {}", kind.as_str())))
}

pub(super) fn payload_string(payload: &serde_json::Value, key: &str) -> Option<String> {
    payload
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned)
}
