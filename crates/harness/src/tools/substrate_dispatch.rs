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

pub fn typed_emit_args(schema_id: &str, schema_version: u32, args: Value) -> Result<Value, String> {
    let Value::Object(mut payload) = args else {
        return Err("typed emit wrapper arguments must be an object".to_string());
    };
    let text = payload.remove("text");
    let mut wrapped = serde_json::json!({
        "schema_id": schema_id,
        "schema_version": schema_version,
        "payload": Value::Object(payload),
    });
    if let Some(text) = text {
        wrapped["text"] = text;
    }
    Ok(wrapped)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::typed_emit_args;

    #[test]
    fn typed_emit_wrapper_reconstructs_internal_emit_args() {
        let wrapped = typed_emit_args(
            "proxima-goal/goal-activated-v1",
            1,
            json!({
                "goal_id": "G1",
                "planner_directive": "Plan product-first.",
                "text": "Goal activated"
            }),
        )
        .unwrap();

        assert_eq!(wrapped["schema_id"], "proxima-goal/goal-activated-v1");
        assert_eq!(wrapped["schema_version"], 1);
        assert_eq!(wrapped["payload"]["goal_id"], "G1");
        assert_eq!(
            wrapped["payload"]["planner_directive"],
            "Plan product-first."
        );
        assert!(wrapped["payload"]["text"].is_null());
        assert_eq!(wrapped["text"], "Goal activated");
    }
}
