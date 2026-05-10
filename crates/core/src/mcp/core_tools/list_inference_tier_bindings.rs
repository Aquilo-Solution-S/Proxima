//! `core/list_inference_tier_bindings` — which inference targets back
//! each model tier for the owner.

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::McpTool;
use crate::mcp::{McpToolCtx, McpToolError};

#[derive(Debug, Default)]
pub struct ListInferenceTierBindingsTool;

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ListInferenceTierBindingsArgs {}

#[derive(Debug, Serialize, JsonSchema)]
pub struct InferenceTierBindingItem {
    pub tier: String,
    pub target_ref: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ListInferenceTierBindingsOutput {
    pub bindings: Vec<InferenceTierBindingItem>,
}

impl McpTool for ListInferenceTierBindingsTool {
    const NAME: &'static str = "core/list_inference_tier_bindings";
    const DESCRIPTION: &'static str = "List tier->target_ref bindings for this owner.";
    type Args = ListInferenceTierBindingsArgs;
    type Output = ListInferenceTierBindingsOutput;

    fn call(
        ctx: McpToolCtx,
        _args: ListInferenceTierBindingsArgs,
    ) -> BoxFuture<'static, Result<ListInferenceTierBindingsOutput, McpToolError>> {
        Box::pin(async move {
            let storage = ctx
                .storage()
                .ok_or_else(|| McpToolError::Other("engine storage unavailable".into()))?;
            let rows = storage
                .list_inference_tier_bindings(&ctx.owner)
                .await
                .map_err(McpToolError::Storage)?;
            let bindings = rows
                .into_iter()
                .map(|row| InferenceTierBindingItem {
                    tier: format!("{:?}", row.tier),
                    target_ref: row.target_ref,
                })
                .collect();
            Ok(ListInferenceTierBindingsOutput { bindings })
        })
    }
}
