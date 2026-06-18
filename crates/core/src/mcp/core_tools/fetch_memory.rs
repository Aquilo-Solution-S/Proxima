//! `core/fetch_memory` — wire-facing single-memory read by prefixed UUID.

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::McpTool;
use crate::mcp::{McpToolCtx, McpToolError};

use super::get_memory::{
    GetMemoryOutput, format_authoring_personality, memory_class, sidecar_specs,
    snapshot_payload_value,
};

#[derive(Debug, Default)]
pub struct FetchMemoryTool;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FetchMemoryArgs {
    /// Memory reference in the ctx output mode: `F:<uuid>`, raw uuid, or handle.
    pub memory: String,
}

impl McpTool for FetchMemoryTool {
    const NAME: &'static str = "core/fetch_memory";
    const DESCRIPTION: &'static str = "Fetch one owner-scoped memory by id/handle. Returns kind, schema, text, payload, and author.";
    type Args = FetchMemoryArgs;
    type Output = GetMemoryOutput;

    fn call(
        ctx: McpToolCtx,
        args: FetchMemoryArgs,
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
