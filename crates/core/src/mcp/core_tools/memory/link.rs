use crate::mcp::{McpTool, McpToolCtx, McpToolError};
use crate::protocol::tool as protocol_tool;
use crate::tool::validate_trimmed_len;
use crate::{AppendMemoryEdgeRequestInput, EdgeAuthorshipKind, MemoryId, SidecarPayload};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{AGENT_LINK_RELATION, AgentLinkV1};

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
    #[schemars(
        description = "Reason for linking source to target, 1 to 1000 chars. Leading and trailing whitespace is removed before the length check."
    )]
    pub reason: String,
    #[serde(default = "default_confidence")]
    #[schemars(
        range(max = 100),
        description = "Confidence score from 0 to 100. Defaults to 80."
    )]
    pub confidence: u8,
    #[serde(default)]
    #[schemars(
        description = "Memory space key from core_memory_spaces. The key selects the write/read context; the persisted edge remains source-owned and source/target handles may be in other readable spaces."
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
    const NAME: &'static str = protocol_tool::CORE_LINK;
    const DESCRIPTION: &'static str =
        "Author a typed agent-link-refers-to edge between two memory handles.";
    type Args = LinkArgs;
    type Output = LinkOutput;

    fn call(
        ctx: McpToolCtx,
        args: LinkArgs,
    ) -> futures::future::BoxFuture<'static, Result<LinkOutput, McpToolError>> {
        Box::pin(async move {
            let reason = validate_trimmed_len("reason", &args.reason, 1000)?;
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
            let source_id = resolve_memory(&ctx, &args.source)?;
            let target_id = resolve_memory(&ctx, &args.target)?;
            if source_id == target_id {
                return Err(McpToolError::InvalidInput("self-loop link rejected".into()));
            }

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
            let edge = AppendMemoryEdgeRequestInput {
                owner: space.owner,
                relation,
                source_memory_id: source_id,
                target_memory_id: target_id,
                authorship_kind: EdgeAuthorshipKind::ExternalAgent,
                authorship_owner_memory_id: ctx.caller_self_perspective,
                sidecar_payload: Some(&payload),
            };
            let engine = ctx.require_engine()?;
            let edge_id = engine
                .append_memory_edge_authorized(&ctx.authz, edge)
                .await
                .map_err(map_link_authoring_error)?;

            Ok(LinkOutput {
                edge_handle: ctx.format_edge(edge_id),
            })
        })
    }
}

fn resolve_memory(ctx: &McpToolCtx, raw: &str) -> Result<MemoryId, McpToolError> {
    ctx.resolve_memory(raw)
}

fn map_link_authoring_error(err: crate::error::ProtocolError) -> McpToolError {
    if err.code == crate::error::ErrorCode::InvalidArgument
        && err.message.contains("rejects source kind Fact")
    {
        return McpToolError::InvalidInput(
            "a Fact cannot be a link source (strict layering: source layer ≥ target layer); derive \
             an Abstraction over the Fact and link from that"
                .into(),
        );
    }
    err.into()
}
