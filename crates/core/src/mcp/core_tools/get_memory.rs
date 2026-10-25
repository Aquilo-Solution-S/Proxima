//! `core/get_memory` — wire-facing single-memory read by UUID.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::MemoryHandleClass;
use crate::engine::GetMemoryReadRequest;
use crate::mcp::{McpToolCtx, McpToolError};

use super::memory::search::{NeighborEdge, neighbor_edges_from_rows};

/// Which version of the memory's series a read answers with.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VersionSelector {
    /// The version `memory` names.
    #[default]
    Exact,
    /// The newest version of the series `memory` belongs to that the caller
    /// may read.
    Head,
}

impl VersionSelector {
    pub(crate) fn read_at(self) -> crate::storage_ports::other_stores::ReadAt {
        match self {
            Self::Exact => crate::storage_ports::other_stores::ReadAt::Version,
            Self::Head => crate::storage_ports::other_stores::ReadAt::SeriesHead,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetMemoryArgs {
    /// Memory reference: Memory UUID.
    pub memory: String,
    /// Include edges touching the memory. Default: false.
    #[serde(default)]
    pub expand_neighbors: bool,
    /// Optional space key from `core_memory_spaces`, validated (an unknown
    /// key is invalid input) but ignored for labeling: the output always
    /// reports the memory's own space.
    #[serde(default)]
    pub space: Option<String>,
    /// `exact` (default) reads the named version; `head` reads the newest
    /// readable version of its series, in this store or the one that issued
    /// the UUID. The output's `handle` names the version returned.
    #[serde(default)]
    pub version: VersionSelector,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct GetMemoryOutput {
    /// The version this read returned.
    pub handle: String,
    pub memory: String,
    /// The series every version of this memory shares.
    pub series: String,
    pub space: String,
    /// The authoritative owner UUID, independent of the selected space key
    /// and the principal that wrote this memory.
    pub owner: uuid::Uuid,
    pub by: uuid::Uuid,
    pub via: Option<uuid::Uuid>,
    pub kind: crate::EntityKind,
    pub schema_id: String,
    pub schema_version: u32,
    /// When the observation occurred (a backdated import's `observed_at`,
    /// else the write time), in the RFC 3339 form `core_search_memories`
    /// gives `created_at`. Present on every Fact, including another store's.
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
    let memory_id = ctx.resolve_memory(&args.memory)?;
    let engine = ctx.require_engine()?;
    if crate::memory_store_number(memory_id.into_inner()) != engine.store_number() {
        return remote_memory_output(&ctx, memory_id.into_inner(), args.version).await;
    }
    // Validated for compatibility; the label comes from the memory's owner.
    super::memory_spaces::resolve_space_owner(
        &ctx,
        args.space.as_deref(),
        super::memory_spaces::SpaceDefault::Current,
    )?;
    let response = engine
        .get_memory(
            &ctx.authz,
            &GetMemoryReadRequest {
                memory_id,
                include_neighbor_edges: args.expand_neighbors,
                at: args.version.read_at(),
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
    project_memory_snapshot(&ctx, snapshot, neighbor_edges)
}

/// A remote UUID read through the other-stores port, as the caller.
/// The port supplies the memory's owner, payload and occurrence time.
/// Neighbor edges are not included in the remote read.
pub(super) async fn remote_memory_output(
    ctx: &McpToolCtx,
    origin: uuid::Uuid,
    version: VersionSelector,
) -> Result<GetMemoryOutput, McpToolError> {
    let memory = match version {
        VersionSelector::Exact => super::other_stores::read_origin(ctx, origin).await?.1,
        VersionSelector::Head => super::other_stores::read_head(ctx, origin).await?,
    };
    Ok(project_remote_memory(memory))
}

/// Project a UUID-checked remote row for either a single or a batch read.
pub(super) fn project_remote_memory(
    memory: crate::storage_ports::other_stores::RemoteMemory,
) -> GetMemoryOutput {
    let handle = memory.handle.to_string();
    GetMemoryOutput {
        handle: handle.clone(),
        memory: handle,
        series: memory.series.to_string(),
        space: memory.owner.to_string(),
        owner: memory.owner.into_inner(),
        by: memory.writer.by,
        via: memory.writer.via,
        kind: memory.kind,
        schema_id: memory.schema_id,
        schema_version: 1,
        occurred_at: memory.occurred_at,
        text: Some(memory.body.clone()),
        payload: memory.payload,
        title: memory.title,
        body: Some(memory.body),
        tags: memory.tags,
        neighbor_edges: None,
    }
}

/// Project a storage snapshot into the wire output shape shared by
/// `get_memory`, the `proxima://memories` batch read, and
/// `facts_citing_object`.
///
/// `space` is the memory's own space: the key of its home owner
/// (`snapshot.owner`), `current` iff that is the session's owner. It never
/// depends on which space the caller named, so a follow-up write to the
/// reported space lands with the memory's owner.
pub(super) fn project_memory_snapshot(
    ctx: &McpToolCtx,
    snapshot: crate::read_models::MemorySnapshot,
    neighbor_edges: Option<Vec<NeighborEdge>>,
) -> Result<GetMemoryOutput, McpToolError> {
    let space = super::memory_spaces::space_key_for_owner(ctx, snapshot.owner)?;
    let handle = snapshot.memory_id.into_inner().to_string();
    let payload = snapshot_payload_value(snapshot.payload.as_ref())?;
    let title =
        payload_string(&payload, "title").or_else(|| payload_string(&payload, "conversation_id"));
    // Storage derived both through the schema's display rule, the one a
    // search hit reports through.
    let body = snapshot.text.clone();
    let tags = snapshot.tags;
    Ok(GetMemoryOutput {
        handle: handle.clone(),
        memory: handle,
        series: snapshot.series.into_inner().to_string(),
        space,
        owner: snapshot.owner.into_inner(),
        by: snapshot.writer.by,
        via: snapshot.writer.via,
        kind: snapshot.kind,
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

#[cfg(test)]
mod tests {
    use super::{GetMemoryOutput, project_memory_snapshot, project_remote_memory};
    use crate::mcp::core_tools::memory_spaces::test_ctx::ctx_for;
    use crate::read_models::MemorySnapshot;
    use crate::storage_ports::other_stores::RemoteMemory;
    use crate::{EntityKind, MemoryId, SchemaId, SchemaVersion, UserId, WriterAttribution};

    #[test]
    fn local_and_remote_memory_outputs_keep_the_recorded_writer() {
        let ctx = ctx_for(UserId::new(uuid::Uuid::from_u128(1)), Vec::new());
        let origin = crate::new_memory_uuid(1);
        for via in [None, Some(uuid::Uuid::from_u128(3))] {
            let writer = WriterAttribution {
                by: uuid::Uuid::from_u128(2),
                via,
            };
            let local = project_memory_snapshot(
                &ctx,
                MemorySnapshot {
                    memory_id: MemoryId::new(crate::new_memory_uuid(0)),
                    series: crate::SeriesHandle::new(crate::new_memory_uuid(0)),
                    kind: EntityKind::Fact,
                    schema_id: SchemaId::new("core/agent-note-v1".into()),
                    schema_version: SchemaVersion::new(1),
                    owner: ctx.owner,
                    writer,
                    occurred_at: Some(time::OffsetDateTime::UNIX_EPOCH),
                    text: Some("stored note".into()),
                    tags: Vec::new(),
                    payload: None,
                },
                None,
            )
            .expect("local projection");
            let remote_memory = RemoteMemory {
                owner: crate::OwnerId::new(uuid::Uuid::from_u128(4)),
                writer,
                schema_id: "remote/note".into(),
                body: "stored remote note".into(),
                ..RemoteMemory::fixture(origin)
            };
            let remote = project_remote_memory(remote_memory.clone());
            assert_eq!(local.owner, ctx.owner.into_inner());
            assert_eq!(local.space, "current");
            assert_eq!(remote.owner, remote_memory.owner.into_inner());
            assert_eq!(remote.space, remote_memory.owner.to_string());
            assert_eq!(remote.payload, remote_memory.payload);
            assert_eq!(remote.occurred_at, remote_memory.occurred_at);
            for output in [local, remote] {
                let wire = serde_json::to_value(output).expect("wire output");
                assert_eq!(wire["by"], serde_json::json!(writer.by));
                assert_eq!(wire.get("via"), Some(&serde_json::json!(writer.via)));
            }
        }
    }

    #[test]
    fn memory_output_schema_requires_the_nullable_caller() {
        let schema = crate::mcp::schema::mcp_output_schema::<GetMemoryOutput>()
            .expect("memory output schema");
        let required = schema["required"].as_array().expect("required fields");
        assert!(required.contains(&serde_json::json!("by")));
        assert!(required.contains(&serde_json::json!("via")));
        assert!(required.contains(&serde_json::json!("owner")));
        assert_eq!(schema["properties"]["by"]["type"], "string");
        assert_eq!(schema["properties"]["owner"]["type"], "string");
        assert_eq!(
            schema["properties"]["via"]["type"],
            serde_json::json!(["string", "null"])
        );
    }
}
