//! `core/instantiate_personality` — wraps `Engine::instantiate_personality`
//! and emits a `core/personality_config_changed_v1` Fact.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::InstantiatePersonalityRequest;
use crate::mcp::core_tools::audit::{AuditEmit, emit_personality_config_changed};
use crate::mcp::core_tools::payload::{
    PersonalityConfigChangeSnapshot, PersonalityConfigChangedSubject, PersonalityConfigChangedVerb,
};
use crate::mcp::{McpToolCtx, McpToolError};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct InstantiatePersonalityArgs {
    pub display_name: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct InstantiatePersonalityOutput {
    /// `I`-prefixed handle for the new instance. Pass as `personality`
    /// to subsequent CRUD calls.
    pub personality: String,
    pub audit_emit_failed: Option<String>,
}

pub(super) async fn instantiate_personality(
    ctx: McpToolCtx,
    args: InstantiatePersonalityArgs,
) -> Result<InstantiatePersonalityOutput, McpToolError> {
    let display_name = args.display_name.trim().to_string();
    if display_name.is_empty() {
        return Err(McpToolError::InvalidInput("display_name is empty".into()));
    }
    let engine = ctx
        .engine()
        .ok_or_else(|| McpToolError::Other("engine unavailable".into()))?;
    let req = InstantiatePersonalityRequest {
        principal: ctx.owner.clone(),
        display_name: display_name.clone(),
    };
    let resp = engine
        .instantiate_personality(&ctx.authz, req)
        .await
        .map_err(|e| McpToolError::Other(e.to_string()))?;
    let after = PersonalityConfigChangeSnapshot::Personality {
        personality_instance_id: Some(resp.instance_id.into_inner()),
        display_name: Some(display_name),
        status: None,
        wake_entry_count: None,
    };
    let audit = emit_personality_config_changed(
        &ctx,
        PersonalityConfigChangedVerb::Instantiate,
        PersonalityConfigChangedSubject::Personality(resp.instance_id.into_inner()),
        None,
        Some(after),
    )
    .await;
    let audit_emit_failed = match audit {
        AuditEmit::Ok => None,
        AuditEmit::Failed { reason } => {
            tracing::warn!(reason, "personality_config_changed audit emit failed");
            Some(reason)
        }
    };
    Ok(InstantiatePersonalityOutput {
        personality: ctx.format_personality(resp.instance_id),
        audit_emit_failed,
    })
}
