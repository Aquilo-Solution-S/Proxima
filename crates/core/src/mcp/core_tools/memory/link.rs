use crate::mcp::{McpTool, McpToolCtx, McpToolError};
use crate::storage::DerivedEdgeSpec;
use crate::{EdgeAuthorshipKind, EdgeId, MemoryId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{AGENT_LINK_RELATION, AgentLinkV1};

use super::util::{map_storage, memory_kind_for_edge};

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
            let payload = serde_json::to_value(AgentLinkV1 {
                reason: reason.to_string(),
                confidence: args.confidence,
            })
            .map_err(|err| McpToolError::InvalidInput(err.to_string()))?;
            let edge = DerivedEdgeSpec {
                owner: &ctx.owner,
                relation,
                source_kind: memory_kind_for_edge(source_kind),
                source_memory_id: MemoryId::new(source_id),
                target_kind: memory_kind_for_edge(target_kind),
                target_memory_id: MemoryId::new(target_id),
                authorship_kind: EdgeAuthorshipKind::ExternalAgent,
                authorship_owner_memory_id: ctx.caller_self_perspective,
                edge_payload: Some(&payload),
            };
            let engine = ctx
                .engine()
                .ok_or_else(|| McpToolError::InvalidInput("engine required".into()))?;
            engine.storage().append_memory_edge(&edge).await?;
            let edge_id = load_latest_link_edge_id(&ctx, source_id, target_id).await?;

            Ok(LinkOutput {
                edge_handle: ctx.format_edge(EdgeId::new(edge_id)),
            })
        })
    }
}

fn resolve_memory(ctx: &McpToolCtx, raw: &str) -> Result<uuid::Uuid, McpToolError> {
    ctx.resolve_memory(raw).map(crate::MemoryId::into_inner)
}

async fn load_kind(
    ctx: &McpToolCtx,
    memory_id: uuid::Uuid,
) -> Result<Option<crate::EntityKind>, McpToolError> {
    let (owner_kind, owner_principal_id, _) = ctx.owner.columns();
    sqlx::query_scalar(
        "SELECT kind
         FROM proxima_core.memories
         WHERE memory_id = $1
           AND owner_principal_kind = $2
           AND owner_principal_id = $3",
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .fetch_optional(&ctx.pool)
    .await
    .map_err(map_storage)?
    .ok_or_else(|| McpToolError::InvalidInput(format!("memory {memory_id} not found for owner")))
}

async fn load_latest_link_edge_id(
    ctx: &McpToolCtx,
    source_id: uuid::Uuid,
    target_id: uuid::Uuid,
) -> Result<uuid::Uuid, McpToolError> {
    let (owner_kind, owner_principal_id, _) = ctx.owner.columns();
    sqlx::query_scalar(
        "SELECT edge_id
         FROM proxima_core.edges
         WHERE owner_principal_kind = $1
           AND owner_principal_id = $2
           AND relation = $3
           AND source_memory_id = $4
           AND target_memory_id = $5
         ORDER BY edge_id DESC
         LIMIT 1",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(AGENT_LINK_RELATION)
    .bind(source_id)
    .bind(target_id)
    .fetch_one(&ctx.pool)
    .await
    .map_err(map_storage)
}
