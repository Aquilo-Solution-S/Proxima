use crate::mcp::{McpTool, McpToolCtx, McpToolError};
use crate::storage::DerivedEdgeSpec;
use crate::{EdgeAuthorshipKind, MemoryAction, MemoryId, Owner, SidecarPayload};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{AGENT_LINK_RELATION, AgentLinkV1};

use super::util::memory_kind_for_edge;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct LinkArgs {
    #[schemars(
        description = "`A...` or `P...` source memory handle for the agent-authored link edge. \
                       Facts cannot be a link source (strict layering: source layer ≥ target layer)."
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
    #[serde(default)]
    #[schemars(
        description = "Memory space key from core_memory_spaces. All handles in this call must belong to this space."
    )]
    pub space: Option<String>,
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
    const NAME: &'static str = "core_link";
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
            let space = super::super::memory_spaces::resolve_space_owner(
                &ctx,
                args.space.as_deref(),
                super::super::memory_spaces::SpaceDefault::Current,
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
            if !ctx
                .authz
                .allows_memory_action(&space.owner, MemoryAction::Write)
            {
                return Err(crate::error::ProtocolError::forbidden(format!(
                    "requires memory.write on space {}",
                    space.key
                ))
                .into());
            }

            let source_id = resolve_memory(&ctx, &args.source)?;
            let target_id = resolve_memory(&ctx, &args.target)?;
            if source_id == target_id {
                return Err(McpToolError::InvalidInput("self-loop link rejected".into()));
            }

            let source_kind = load_kind(&ctx, &space.owner, source_id).await?;
            // Only Abstractions/Perspectives may source an agent link; a Fact
            // (or any non-A/P) source violates strict layering (inv 1).
            if !matches!(
                source_kind,
                Some(crate::EntityKind::Abstraction | crate::EntityKind::Perspective)
            ) {
                return Err(McpToolError::InvalidInput(
                    "a Fact cannot be a link source (strict layering: source layer ≥ target \
                     layer); derive an Abstraction over the Fact and link from that"
                        .into(),
                ));
            }
            let target_kind = load_kind(&ctx, &space.owner, target_id).await?;

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
                owner: &space.owner,
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
    owner: &Owner,
    memory_id: MemoryId,
) -> Result<Option<crate::EntityKind>, McpToolError> {
    let storage = ctx
        .storage()
        .ok_or_else(|| McpToolError::Other("engine storage unavailable".into()))?;
    storage
        .load_memory_kinds(owner, &[memory_id])
        .await?
        .into_iter()
        .next()
        .map(|row| row.kind)
        .ok_or_else(|| {
            McpToolError::InvalidInput(
                "cross-space derive/link is not supported; choose one memory space".into(),
            )
        })
}
