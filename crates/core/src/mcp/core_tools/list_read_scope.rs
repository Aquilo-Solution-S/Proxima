//! `core/list_read_scope` — read-only projection of explicit personality
//! read-scope grants.

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ListReadScopeRequest;
use crate::McpTool;
use crate::mcp::{McpToolCtx, McpToolError};

#[derive(Debug, Default)]
pub struct ListReadScopeTool;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListReadScopeArgs {
    /// `P`-handle for the reader personality.
    pub personality: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ListReadScopeOutput {
    pub personality: String,
    pub identity_read_allowed: bool,
    pub readable_personalities: Vec<String>,
}

impl McpTool for ListReadScopeTool {
    const NAME: &'static str = "core/list_read_scope";
    const DESCRIPTION: &'static str = "List explicit cross-personality read grants for one reader \
         personality. The identity diagonal is always allowed and is reported separately.";
    type Args = ListReadScopeArgs;
    type Output = ListReadScopeOutput;

    fn call(
        ctx: McpToolCtx,
        args: ListReadScopeArgs,
    ) -> BoxFuture<'static, Result<ListReadScopeOutput, McpToolError>> {
        Box::pin(async move {
            let pid = ctx.resolve_personality(&args.personality)?;
            let storage = ctx
                .storage()
                .ok_or_else(|| McpToolError::Other("engine storage unavailable".into()))?;
            let response = storage
                .list_read_scope(&ListReadScopeRequest {
                    owner: ctx.owner.clone(),
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
        })
    }
}
