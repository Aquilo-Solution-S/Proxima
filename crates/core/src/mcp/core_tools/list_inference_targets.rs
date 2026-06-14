//! `core/list_inference_targets` — read-only enumeration of registered
//! inference targets for the owner.

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::McpTool;
use crate::mcp::{McpToolCtx, McpToolError};

#[derive(Debug, Default)]
pub struct ListInferenceTargetsTool;

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ListInferenceTargetsArgs {}

#[derive(Debug, Serialize, JsonSchema)]
pub struct InferenceTargetItem {
    pub target_ref: String,
    /// Opaque provider config — surfaced as JSON so flavor-specific
    /// shapes pass through without core-side projection.
    pub config: serde_json::Value,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ListInferenceTargetsOutput {
    pub targets: Vec<InferenceTargetItem>,
}

impl McpTool for ListInferenceTargetsTool {
    const NAME: &'static str = "core/list_inference_targets";
    const DESCRIPTION: &'static str = "List inference targets registered for this owner.";
    type Args = ListInferenceTargetsArgs;
    type Output = ListInferenceTargetsOutput;

    fn call(
        ctx: McpToolCtx,
        _args: ListInferenceTargetsArgs,
    ) -> BoxFuture<'static, Result<ListInferenceTargetsOutput, McpToolError>> {
        Box::pin(async move {
            let storage = ctx
                .storage()
                .ok_or_else(|| McpToolError::Other("engine storage unavailable".into()))?;
            let rows = storage
                .list_inference_targets(&ctx.owner)
                .await
                .map_err(McpToolError::Storage)?;
            let targets = rows
                .into_iter()
                .map(|row| InferenceTargetItem {
                    target_ref: row.target_ref,
                    config: serde_json::to_value(&row.config).unwrap_or(serde_json::Value::Null),
                })
                .collect();
            Ok(ListInferenceTargetsOutput { targets })
        })
    }
}
