//! `core/list_read_scope` — read-only projection of explicit personality
//! read-scope grants.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ListReadScopeRequest;
use crate::mcp::{McpToolCtx, McpToolError};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListReadScopeArgs {
    /// `I`-handle for the reader personality.
    pub personality: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ListReadScopeOutput {
    pub personality: String,
    pub identity_read_allowed: bool,
    pub readable_personalities: Vec<String>,
}

pub(super) async fn list_read_scope(
    ctx: McpToolCtx,
    args: ListReadScopeArgs,
) -> Result<ListReadScopeOutput, McpToolError> {
    let pid = ctx.resolve_personality(&args.personality)?;
    let engine = ctx
        .engine()
        .ok_or_else(|| McpToolError::Other("engine unavailable".into()))?;
    let response = engine
        .list_read_scope(
            &ctx.authz,
            &ListReadScopeRequest {
                principal: ctx.owner.clone(),
                reader_personality_instance_id: pid,
            },
        )
        .await?;
    let readable_personalities = response
        .readable_personality_instance_ids
        .into_iter()
        .map(|id| ctx.format_personality(id))
        .collect();
    Ok(ListReadScopeOutput {
        personality: ctx.format_personality(pid),
        identity_read_allowed: true,
        readable_personalities,
    })
}
