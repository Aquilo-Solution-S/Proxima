//! `core/facts_citing_object` — owner-scoped citation-to-Fact read-back.

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::McpTool;
use crate::mcp::{McpToolCtx, McpToolError};

use super::get_memory::{
    GetMemoryOutput, format_authoring_personality, memory_class, sidecar_specs,
};

#[derive(Debug, Default)]
pub struct FactsCitingObjectTool;

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

impl McpTool for FactsCitingObjectTool {
    const NAME: &'static str = "core/facts_citing_object";
    const DESCRIPTION: &'static str =
        "Return owner-scoped Facts whose citation mapping points at cited_object_id.";
    type Args = FactsCitingObjectArgs;
    type Output = FactsCitingObjectOutput;

    fn call(
        ctx: McpToolCtx,
        args: FactsCitingObjectArgs,
    ) -> BoxFuture<'static, Result<FactsCitingObjectOutput, McpToolError>> {
        Box::pin(async move {
            let cited_object_id = parse_cited_object_id(&args.cited_object_id)?;
            let storage = ctx
                .storage()
                .ok_or_else(|| McpToolError::Other("engine storage unavailable".into()))?;
            let sidecars = sidecar_specs(&ctx);
            let snapshots = storage
                .facts_citing_object(&ctx.owner, cited_object_id, &sidecars)
                .await?;
            let facts = snapshots
                .into_iter()
                .map(|snapshot| {
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
                        payload: snapshot.payload_json,
                    })
                })
                .collect::<Result<Vec<_>, McpToolError>>()?;
            Ok(FactsCitingObjectOutput {
                cited_object_id: cited_object_id.to_string(),
                facts,
            })
        })
    }
}

pub(super) fn parse_cited_object_id(raw: &str) -> Result<uuid::Uuid, McpToolError> {
    let uuid_part = raw.strip_prefix("C:").unwrap_or(raw);
    uuid_part
        .parse()
        .map_err(|err| McpToolError::InvalidInput(format!("not a cited_object_id uuid: {err}")))
}
