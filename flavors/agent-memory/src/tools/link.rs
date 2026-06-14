use proxima_core::mcp::{McpTool, McpToolCtx, McpToolError};
use proxima_core::{EdgeAuthorshipKind, EdgeId, MemoryId};
use proxima_storage_pg::verbs::edge_append::{EdgeDraft, append_edge_in_tx};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{AGENT_LINK_RELATION, AgentLinkV1};

use super::util::{map_storage, memory_kind_for_edge, owner_columns};

const LINK_NAMESPACE: uuid::Uuid = uuid::Uuid::from_bytes([
    0x4d, 0x70, 0x9b, 0xfb, 0x71, 0xc7, 0x4e, 0x37, 0xb2, 0x88, 0x3a, 0x09, 0xe7, 0x05, 0x69, 0xb5,
]);

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
    const NAME: &'static str = "proxima-agent-memory/proxima_link";
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

            let edge_id = link_edge_id(&ctx.owner, source_id, target_id);
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
            let edge_draft = EdgeDraft {
                edge_id,
                relation,
                source_kind: memory_kind_for_edge(source_kind),
                source_memory_id: Some(source_id),
                source_goal_id: None,
                target_kind: memory_kind_for_edge(target_kind),
                target_memory_id: Some(target_id),
                target_goal_id: None,
                authorship_kind: EdgeAuthorshipKind::ExternalAgent,
                authorship_owner_memory_id: ctx.caller_self_perspective.map(MemoryId::into_inner),
                owner: &ctx.owner,
            };
            let mut tx = ctx.pool.begin().await.map_err(map_storage)?;
            append_edge_in_tx(&mut tx, &edge_draft, Some(&payload))
                .await
                .map_err(McpToolError::Storage)?;
            tx.commit().await.map_err(map_storage)?;

            Ok(LinkOutput {
                edge_handle: ctx.format_edge(EdgeId::new(edge_id)),
            })
        })
    }
}

fn resolve_memory(ctx: &McpToolCtx, raw: &str) -> Result<uuid::Uuid, McpToolError> {
    ctx.resolve_memory(raw)
        .map(proxima_core::MemoryId::into_inner)
}

async fn load_kind(
    ctx: &McpToolCtx,
    memory_id: uuid::Uuid,
) -> Result<Option<proxima_core::EntityKind>, McpToolError> {
    let (owner_kind, owner_principal_id, _) = owner_columns(&ctx.owner);
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

fn link_edge_id(owner: &proxima_core::Owner, source: uuid::Uuid, target: uuid::Uuid) -> uuid::Uuid {
    let (kind, principal_id, org_id) = owner_columns(owner);
    let mut key = Vec::with_capacity(96);
    key.extend_from_slice(kind.as_str().as_bytes());
    key.push(0);
    key.extend_from_slice(principal_id.as_bytes());
    key.push(0);
    key.extend_from_slice(org_id.as_bytes());
    key.push(0);
    key.extend_from_slice(AGENT_LINK_RELATION.as_bytes());
    key.push(0);
    key.extend_from_slice(source.as_bytes());
    key.push(0);
    key.extend_from_slice(target.as_bytes());
    uuid::Uuid::new_v5(&LINK_NAMESPACE, &key)
}
