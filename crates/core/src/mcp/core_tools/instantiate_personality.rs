//! `core/instantiate_personality` — wraps `Engine::instantiate_personality`
//! and emits a `core/personality_config_changed_v1` Fact.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::InstantiatePersonalityRequest;
use crate::mcp::core_tools::audit::{audit_emit_failed, personality_config_changed_input};
use crate::mcp::core_tools::payload::{
    PersonalityConfigChangedSubject, PersonalityConfigChangedVerb,
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
        principal: ctx.owner,
        display_name: display_name.clone(),
    };
    let (audit, preflight_failure) = match personality_config_changed_input(
        &ctx,
        PersonalityConfigChangedVerb::Instantiate,
        PersonalityConfigChangedSubject::Personality(uuid::Uuid::nil()),
        None,
        None,
    ) {
        Ok(input) => (Some(input), None),
        Err(reason) => {
            tracing::warn!(reason, "personality_config_changed audit emit failed");
            (None, Some(reason))
        }
    };
    let resp = engine
        .instantiate_personality_with_audit(&ctx.authz, req, audit)
        .await
        .map_err(|e| McpToolError::Other(e.to_string()))?;
    if let crate::engine::PersonalityConfigAuditEmit::Failed { reason } = &resp.audit_emit {
        tracing::warn!(reason, "personality_config_changed audit emit failed");
    }
    Ok(InstantiatePersonalityOutput {
        personality: ctx.format_personality(resp.response.instance_id),
        audit_emit_failed: audit_emit_failed(preflight_failure, resp.audit_emit),
    })
}
