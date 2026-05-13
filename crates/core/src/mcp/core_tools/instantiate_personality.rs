//! `core/instantiate_personality` — wraps Engine::instantiate_personality
//! and emits a `core/personality_config_changed_v1` Fact.

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::InstantiatePersonalityRequest;
use crate::McpTool;
use crate::mcp::core_tools::audit::{AuditEmit, emit_personality_config_changed};
use crate::mcp::core_tools::payload::{
    PersonalityConfigChangedSubject, PersonalityConfigChangedVerb,
};
use crate::mcp::{McpToolCtx, McpToolError};

#[derive(Debug, Default)]
pub struct InstantiatePersonalityTool;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct InstantiatePersonalityArgs {
    pub display_name: String,
    pub purpose: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct InstantiatePersonalityOutput {
    /// `P`-prefixed handle for the new instance. Pass as `personality`
    /// to subsequent CRUD calls.
    pub personality: String,
    pub audit_emit_failed: Option<String>,
}

impl McpTool for InstantiatePersonalityTool {
    const NAME: &'static str = "core/instantiate_personality";
    const DESCRIPTION: &'static str = "Instantiate one inert personality with a Root Perspective and \
         empty WakeConfig. Returns the new P-handle in the `personality` field — pass that value as \
         the `personality` argument to add_wake_entry, get_personality, tombstone_personality, etc.";
    type Args = InstantiatePersonalityArgs;
    type Output = InstantiatePersonalityOutput;

    fn call(
        ctx: McpToolCtx,
        args: InstantiatePersonalityArgs,
    ) -> BoxFuture<'static, Result<InstantiatePersonalityOutput, McpToolError>> {
        Box::pin(async move {
            let display_name = args.display_name.trim().to_string();
            let purpose = args.purpose.trim().to_string();
            if display_name.is_empty() {
                return Err(McpToolError::InvalidInput("display_name is empty".into()));
            }
            if purpose.is_empty() {
                return Err(McpToolError::InvalidInput("purpose is empty".into()));
            }
            let engine = ctx
                .engine()
                .ok_or_else(|| McpToolError::Other("engine unavailable".into()))?;
            let req = InstantiatePersonalityRequest {
                owner: ctx.owner.clone(),
                display_name: display_name.clone(),
                purpose: purpose.clone(),
            };
            let resp = engine
                .instantiate_personality(req)
                .await
                .map_err(|e| McpToolError::Other(e.to_string()))?;
            let after = serde_json::json!({
                "personality_instance_id": resp.instance_id.into_inner(),
                "display_name": display_name,
                "purpose": purpose,
            });
            let audit = emit_personality_config_changed(
                &ctx,
                PersonalityConfigChangedVerb::Instantiate,
                PersonalityConfigChangedSubject::Personality(resp.instance_id.into_inner()),
                None,
                Some(after),
            )
            .await;
            let p_handle = ctx.handles.as_ref().unwrap().assign_personality(resp.instance_id);
            let audit_emit_failed = match audit {
                AuditEmit::Ok => None,
                AuditEmit::Failed { reason } => {
                    tracing::warn!(reason, "personality_config_changed audit emit failed");
                    Some(reason)
                }
            };
            Ok(InstantiatePersonalityOutput {
                personality: p_handle.as_str().to_string(),
                audit_emit_failed,
            })
        })
    }
}
