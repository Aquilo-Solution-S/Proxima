//! `core/set_read_scope` — replace explicit personality read-scope grants.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::engine::SetReadScopeAdminRequest;
use crate::mcp::core_tools::audit::{audit_emit_failed, personality_config_changed_input};
use crate::mcp::core_tools::payload::{
    PersonalityConfigChangedSubject, PersonalityConfigChangedVerb,
};
use crate::mcp::{McpToolCtx, McpToolError};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetReadScopeArgs {
    /// `I`-handle for the reader personality whose explicit grants are
    /// replaced.
    pub personality: String,
    /// `I`-handles the reader may read in addition to itself.
    pub readable_personalities: Vec<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SetReadScopeOutput {
    pub personality: String,
    pub readable_count: u32,
    pub readable_personalities: Vec<String>,
    pub audit_emit_failed: Option<String>,
}

pub(super) async fn set_read_scope(
    ctx: McpToolCtx,
    args: SetReadScopeArgs,
) -> Result<SetReadScopeOutput, McpToolError> {
    let pid = ctx.resolve_personality(&args.personality)?;
    let readable_ids = args
        .readable_personalities
        .iter()
        .map(|handle| ctx.resolve_personality(handle))
        .collect::<Result<Vec<_>, _>>()?;
    let engine = ctx
        .engine()
        .ok_or_else(|| McpToolError::Other("engine unavailable".into()))?;
    let (audit, preflight_failure) = match personality_config_changed_input(
        &ctx,
        PersonalityConfigChangedVerb::SetReadScope,
        PersonalityConfigChangedSubject::Personality(pid.into_inner()),
        None,
        None,
    ) {
        Ok(input) => (Some(input), None),
        Err(reason) => (None, Some(reason)),
    };
    let response = engine
        .set_read_scope(
            &ctx.authz,
            &SetReadScopeAdminRequest {
                principal: ctx.owner.clone(),
                reader_personality_instance_id: pid,
                readable_personality_instance_ids: readable_ids,
                audit,
            },
        )
        .await
        .map_err(|e| McpToolError::Other(e.to_string()))?;

    let readable_personalities = response
        .readable_personality_instance_ids
        .into_iter()
        .filter(|id| *id != pid)
        .map(|id| ctx.format_personality(id))
        .collect();
    Ok(SetReadScopeOutput {
        personality: ctx.format_personality(pid),
        readable_count: response.response.readable_count,
        readable_personalities,
        audit_emit_failed: audit_emit_failed(preflight_failure, response.audit_emit),
    })
}
