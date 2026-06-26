//! `core/get_memory` — wire-facing single-memory read by id or handle.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::mcp::{McpToolCtx, McpToolError};
use crate::personality::{PersonalityInstanceId, SidecarSpec};
use crate::verbs::schema::PayloadKind;
use crate::{MemoryAction, MemoryHandleClass, MemoryId, SchemaId};

use super::memory::search::{NeighborEdge, neighbor_edges};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetMemoryArgs {
    /// Memory reference: `F:<uuid>`, `A:<uuid>`, `P:<uuid>`, raw uuid, or handle.
    pub memory: String,
    /// Include edges touching the memory. Default: false.
    #[serde(default)]
    pub expand_neighbors: bool,
    /// Memory space key from `core_memory_spaces`. Omit for current owner.
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authoring_personality_instance_id: Option<String>,
    pub text: Option<String>,
    pub wake_chain_depth: u16,
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
    let memory_uuid = memory_id.into_inner();
    let space = super::memory_spaces::resolve_space_owner(
        &ctx,
        args.space.as_deref(),
        super::memory_spaces::SpaceDefault::Current,
    )?;
    if !ctx
        .authz
        .allows_memory_action(&space.owner, MemoryAction::Read)
    {
        return Err(crate::error::ProtocolError::forbidden(format!(
            "requires memory.read on space {}",
            space.key
        ))
        .into());
    }
    let storage = ctx
        .storage()
        .ok_or_else(|| McpToolError::Other("engine storage unavailable".into()))?;
    let sidecars = sidecar_specs(&ctx);
    let snapshot = storage
        .load_memory_by_id(&space.owner, memory_id, None, &sidecars)
        .await?
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
        Some(neighbor_edges(&ctx, &space.owner, &[memory_uuid]).await?)
    } else {
        None
    };
    Ok(GetMemoryOutput {
        handle: handle.clone(),
        memory: handle,
        space: space.key,
        kind: snapshot.kind,
        schema_id: snapshot.schema_id.as_str().to_string(),
        schema_version: snapshot.schema_version.into_inner(),
        authoring_personality_instance_id: format_authoring_personality(
            &ctx,
            snapshot.authoring_personality_instance_id,
        ),
        text: snapshot.text,
        wake_chain_depth: snapshot.wake_chain_depth.into_inner(),
        payload,
        title,
        body,
        tags,
        neighbor_edges,
    })
}

/// Resolve a memory reference accepting a prefixed id (`F:…`), a handle, or a
/// bare uuid. The bare-uuid fallback serves the prefixed-id / raw-id wire
/// surfaces; in `Handles` mode it would bypass the per-wake handle table, but
/// `get_memory` has no live `Handles`-mode surface and the subsequent
/// `load_memory_by_id` is owner-scoped, so no cross-owner read is possible.
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

pub(super) fn sidecar_specs(ctx: &McpToolCtx) -> Vec<SidecarSpec> {
    ctx.registry
        .list()
        .into_iter()
        .filter(|schema| {
            matches!(
                schema.kind,
                PayloadKind::Fact | PayloadKind::Abstraction | PayloadKind::Perspective
            ) && schema.sidecar_table.is_some()
        })
        .map(|schema| SidecarSpec {
            schema_id: SchemaId::new(schema.schema_id.as_str().to_string()),
            schema_version: schema.schema_version,
            sidecar_table: schema.sidecar_table.expect("filtered to sidecar schemas"),
        })
        .collect()
}

pub(super) fn memory_class(kind: &str) -> Result<MemoryHandleClass, McpToolError> {
    MemoryHandleClass::from_memory_kind(kind)
        .ok_or_else(|| McpToolError::Other(format!("unknown memory kind: {kind}")))
}

pub(super) fn format_authoring_personality(
    ctx: &McpToolCtx,
    instance_id: Option<PersonalityInstanceId>,
) -> Option<String> {
    instance_id.map(|id| ctx.format_personality(id))
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
