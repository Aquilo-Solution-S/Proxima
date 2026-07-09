//! `core/facts_citing_object` — owner-scoped citation-to-Fact read-back.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::engine::FactsCitingObjectReadRequest;
use crate::mcp::{McpToolCtx, McpToolError};

use super::get_memory::{
    GetMemoryOutput, memory_class, payload_string, payload_tags, snapshot_payload_value,
};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FactsCitingObjectArgs {
    /// Cited object uuid, optionally prefixed as `C:<uuid>`.
    pub cited_object_id: String,
}

#[derive(Debug, Serialize)]
pub struct FactsCitingObjectOutput {
    pub cited_object_id: String,
    pub facts: Vec<GetMemoryOutput>,
}

pub(super) async fn facts_citing_object(
    ctx: McpToolCtx,
    args: FactsCitingObjectArgs,
) -> Result<FactsCitingObjectOutput, McpToolError> {
    let cited_object_id = parse_cited_object_id(&args.cited_object_id)?;
    let engine = ctx
        .engine()
        .ok_or_else(|| McpToolError::Other("engine unavailable".into()))?;
    let snapshots = engine
        .facts_citing_object(
            &ctx.authz,
            &FactsCitingObjectReadRequest { cited_object_id },
        )
        .await?;
    let facts = snapshots
        .into_iter()
        .map(|snapshot| {
            let class = memory_class(&snapshot.kind)?;
            let handle = ctx.format_memory_with_class(snapshot.memory_id, class);
            let payload = snapshot_payload_value(snapshot.payload.as_ref())?;
            let title = payload_string(&payload, "title")
                .or_else(|| payload_string(&payload, "conversation_id"));
            let body = payload_string(&payload, "body")
                .or_else(|| payload_string(&payload, "text"))
                .or_else(|| snapshot.text.clone());
            let tags = payload_tags(&payload);
            Ok(GetMemoryOutput {
                handle: handle.clone(),
                memory: handle,
                space: "current".into(),
                kind: snapshot.kind,
                schema_id: snapshot.schema_id.as_str().to_string(),
                schema_version: snapshot.schema_version.into_inner(),
                text: snapshot.text,
                payload,
                title,
                body,
                tags,
                neighbor_edges: None,
            })
        })
        .collect::<Result<Vec<_>, McpToolError>>()?;
    Ok(FactsCitingObjectOutput {
        cited_object_id: cited_object_id.to_string(),
        facts,
    })
}

pub(super) fn parse_cited_object_id(raw: &str) -> Result<uuid::Uuid, McpToolError> {
    let uuid_part = raw.strip_prefix("C:").unwrap_or(raw);
    uuid_part
        .parse()
        .map_err(|err| McpToolError::InvalidInput(format!("not a cited_object_id uuid: {err}")))
}
