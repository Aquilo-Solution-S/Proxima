//! In-process dispatch into wake-visible substrate tools.

use std::sync::Arc;

use proxima_core::harness::{HarnessContext, SubstrateToolBinding};
use proxima_core::mcp::{
    HarnessSubstrateBridge, HarnessSubstrateCall, HarnessSubstrateError, McpAuthorContext,
};
use serde_json::Value;

#[derive(Debug, Clone)]
pub enum SubstrateDispatchResult {
    Ok(Value),
    Recoverable(String),
    Fatal(String),
}

pub async fn dispatch(
    bridge: &Arc<dyn HarnessSubstrateBridge>,
    binding: &SubstrateToolBinding,
    args: Value,
    ctx: &HarnessContext,
    model_id: &str,
) -> SubstrateDispatchResult {
    let author = McpAuthorContext {
        model_id: model_id.to_string(),
        client_name: "proxima-harness".to_string(),
        client_version: env!("CARGO_PKG_VERSION").to_string(),
        caller_self_perspective: Some(ctx.root_perspective_memory_id),
    };

    let call = HarnessSubstrateCall {
        canonical_name: binding.canonical_name.clone(),
        args,
        owner: ctx.owner.clone(),
        wake_token: ctx.wake_token,
        author,
    };

    match bridge.call_harness_tool(call).await {
        Ok(v) => SubstrateDispatchResult::Ok(v),
        Err(HarnessSubstrateError::Storage(e) | HarnessSubstrateError::Layering(e)) => {
            SubstrateDispatchResult::Fatal(e)
        }
        Err(other) => SubstrateDispatchResult::Recoverable(other.to_string()),
    }
}
