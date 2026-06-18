//! `core/get_memory` — wire-facing single-memory read by prefixed UUID.

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::mcp::{McpToolCtx, McpToolError};
use crate::personality::{PersonalityInstanceId, SidecarSpec};
use crate::verbs::schema::PayloadKind;
use crate::{McpTool, MemoryHandleClass, SchemaId};

#[derive(Debug, Default)]
pub struct GetMemoryTool;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetMemoryArgs {
    /// `F:<uuid>`, `A:<uuid>`, or `P:<uuid>` memory id.
    pub memory: String,
}

#[derive(Debug, Serialize)]
pub struct GetMemoryOutput {
    pub memory: String,
    pub kind: String,
    pub schema_id: String,
    pub schema_version: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authoring_personality_instance_id: Option<String>,
    pub text: Option<String>,
    pub wake_chain_depth: u16,
    pub payload: serde_json::Value,
}

impl McpTool for GetMemoryTool {
    const NAME: &'static str = "core/get_memory";
    const DESCRIPTION: &'static str = "Fetch one owner-scoped memory by prefixed id. Returns kind, schema, text, payload, and author.";
    type Args = GetMemoryArgs;
    type Output = GetMemoryOutput;

    fn call(
        ctx: McpToolCtx,
        args: GetMemoryArgs,
    ) -> BoxFuture<'static, Result<GetMemoryOutput, McpToolError>> {
        Box::pin(async move {
            let memory_id = ctx.resolve_memory(&args.memory)?;
            let storage = ctx
                .storage()
                .ok_or_else(|| McpToolError::Other("engine storage unavailable".into()))?;
            let sidecars = sidecar_specs(&ctx);
            let snapshot = storage
                .load_memory_by_id(&ctx.owner, memory_id, None, &sidecars)
                .await?
                .ok_or_else(|| {
                    McpToolError::InvalidInput(format!("memory {memory_id:?} not found"))
                })?;
            let class = memory_class(&snapshot.kind)?;
            Ok(GetMemoryOutput {
                memory: ctx.format_memory_with_class(snapshot.memory_id, class),
                kind: snapshot.kind,
                schema_id: snapshot.schema_id.as_str().to_string(),
                schema_version: snapshot.schema_version.into_inner(),
                authoring_personality_instance_id: format_authoring_personality(
                    &ctx,
                    snapshot.authoring_personality_instance_id,
                ),
                text: snapshot.text,
                wake_chain_depth: snapshot.wake_chain_depth.into_inner(),
                payload: snapshot_payload_value(snapshot.payload.as_ref())?,
            })
        })
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
