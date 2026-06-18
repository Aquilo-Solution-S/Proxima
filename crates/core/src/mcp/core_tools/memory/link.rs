use crate::mcp::{McpTool, McpToolCtx, McpToolError};
use crate::storage::DerivedEdgeSpec;
use crate::{EdgeAuthorshipKind, MemoryId, SidecarPayload};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{AGENT_LINK_RELATION, AgentLinkV1};

use super::util::memory_kind_for_edge;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct LinkArgs {
    #[schemars(
        description = "`F...`, `A...`, or `P...` source memory handle for the agent-authored link edge."
    )]
    pub source: String,
    #[schemars(
        description = "`F...`, `A...`, or `P...` target memory handle for the agent-authored link edge."
    )]
    pub target: String,
    #[schemars(description = "Reason for linking source to target, 1 to 1000 chars.")]
    pub reason: String,
    #[serde(default = "default_confidence")]
    #[schemars(description = "Confidence score from 0 to 100. Defaults to 80.")]
    pub confidence: u8,
}

fn default_confidence() -> u8 {
    80
}

#[derive(Debug, Serialize)]
pub struct LinkOutput {
    pub edge_handle: String,
}

#[derive(Debug)]
pub struct LinkTool;

impl McpTool for LinkTool {
    const NAME: &'static str = "core/link";
    const DESCRIPTION: &'static str =
        "Author a typed agent-link-refers-to edge between two memory handles.";
    type Args = LinkArgs;
    type Output = LinkOutput;

    fn call(
        ctx: McpToolCtx,
        args: LinkArgs,
    ) -> futures::future::BoxFuture<'static, Result<LinkOutput, McpToolError>> {
        Box::pin(async move {
            let reason = args.reason.trim();
            if reason.is_empty() || reason.chars().count() > 1000 {
                return Err(McpToolError::InvalidInput(
                    "reason must be 1..=1000 chars".into(),
                ));
            }
            if args.confidence > 100 {
                return Err(McpToolError::InvalidInput(
                    "confidence must be 0..=100".into(),
                ));
            }
            let source_id = resolve_memory(&ctx, &args.source)?;
            let target_id = resolve_memory(&ctx, &args.target)?;
            if source_id == target_id {
                return Err(McpToolError::InvalidInput("self-loop link rejected".into()));
            }

            let source_kind = load_kind(&ctx, source_id).await?;
            let target_kind = load_kind(&ctx, target_id).await?;

            let relation = ctx
                .registry
                .resolve_relation(AGENT_LINK_RELATION)
                .ok_or_else(|| {
                    McpToolError::Other(format!("relation {AGENT_LINK_RELATION} not registered"))
                })?;
            let payload = SidecarPayload::edge(AgentLinkV1 {
                reason: reason.to_string(),
                confidence: args.confidence,
            });
            let edge = DerivedEdgeSpec {
                owner: &ctx.owner,
                relation,
                source_kind: memory_kind_for_edge(source_kind),
                source_memory_id: source_id,
                target_kind: memory_kind_for_edge(target_kind),
                target_memory_id: target_id,
                authorship_kind: EdgeAuthorshipKind::ExternalAgent,
                authorship_owner_memory_id: ctx.caller_self_perspective,
                sidecar_payload: Some(&payload),
            };
            let engine = ctx
                .engine()
                .ok_or_else(|| McpToolError::InvalidInput("engine required".into()))?;
            let edge_id = engine.storage().append_memory_edge(&edge).await?;

            Ok(LinkOutput {
                edge_handle: ctx.format_edge(edge_id),
            })
        })
    }
}

fn resolve_memory(ctx: &McpToolCtx, raw: &str) -> Result<MemoryId, McpToolError> {
    ctx.resolve_memory(raw)
}

async fn load_kind(
    ctx: &McpToolCtx,
    memory_id: MemoryId,
) -> Result<Option<crate::EntityKind>, McpToolError> {
    let storage = ctx
        .storage()
        .ok_or_else(|| McpToolError::Other("engine storage unavailable".into()))?;
    storage
        .load_memory_kinds(&ctx.owner, &[memory_id])
        .await?
        .into_iter()
        .next()
        .map(|row| row.kind)
        .ok_or_else(|| {
            McpToolError::InvalidInput(format!(
                "memory {} not found for owner",
                memory_id.into_inner()
            ))
        })
}
