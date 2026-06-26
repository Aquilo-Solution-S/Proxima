//! `core/list_read_scope` — read-only projection of explicit personality
//! read-scope grants.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ListReadScopeRequest;
use crate::MemoryAction;
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
    if !ctx
        .authz
        .allows_memory_action(&ctx.owner, MemoryAction::Admin)
    {
        return Err(
            crate::error::ProtocolError::forbidden("requires memory.admin on owner").into(),
        );
    }
    let pid = ctx.resolve_personality(&args.personality)?;
    let storage = ctx
        .storage()
        .ok_or_else(|| McpToolError::Other("engine storage unavailable".into()))?;
    let response = storage
        .list_read_scope(&ListReadScopeRequest {
            principal: ctx.owner.clone(),
            reader_personality_instance_id: pid,
        })
        .await
        .map_err(McpToolError::Storage)?;
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
