//! `core/tombstone_personality` — wraps `Engine::tombstone_personality`
//! and emits an audit Fact.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::TombstonePersonalityRequest;
use crate::mcp::core_tools::audit::{AuditEmit, emit_personality_config_changed};
use crate::mcp::core_tools::payload::{
    PersonalityConfigChangeSnapshot, PersonalityConfigChangedSubject, PersonalityConfigChangedVerb,
};
use crate::mcp::{McpToolCtx, McpToolError};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TombstonePersonalityArgs {
    /// `I`-handle of the personality to tombstone.
    pub personality: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct TombstonePersonalityOutput {
    pub status: String,
    pub idempotent_replay: bool,
    pub audit_emit_failed: Option<String>,
}

pub(super) async fn tombstone_personality(
    ctx: McpToolCtx,
    args: TombstonePersonalityArgs,
) -> Result<TombstonePersonalityOutput, McpToolError> {
    let pid = ctx.resolve_personality(&args.personality)?;
    let engine = ctx
        .engine()
        .ok_or_else(|| McpToolError::Other("engine unavailable".into()))?;
    // Snapshot prior state for the audit `before`.
    let storage = ctx
        .storage()
        .ok_or_else(|| McpToolError::Other("engine storage unavailable".into()))?;
    let rows = storage
        .list_personality_instances(&ctx.owner, true)
        .await
        .map_err(McpToolError::Storage)?;
    let before_row = rows.iter().find(|r| r.personality_instance_id == pid);
    let before = before_row.map(|r| PersonalityConfigChangeSnapshot::Personality {
        personality_instance_id: Some(r.personality_instance_id.into_inner()),
        display_name: Some(r.display_name.clone()),
        status: Some(r.status.as_str().to_string()),
        wake_entry_count: Some(r.wake_entries.len()),
    });
    let req = TombstonePersonalityRequest {
        principal: ctx.owner.clone(),
        personality_instance_id: pid,
    };
    let resp = engine
        .tombstone_personality(&ctx.authz, req)
        .await
        .map_err(|e| McpToolError::Other(e.to_string()))?;
    let audit = emit_personality_config_changed(
        &ctx,
        PersonalityConfigChangedVerb::Tombstone,
        PersonalityConfigChangedSubject::Personality(pid.into_inner()),
        before,
        None,
    )
    .await;
    let audit_emit_failed = match audit {
        AuditEmit::Ok => None,
        AuditEmit::Failed { reason } => Some(reason),
    };
    Ok(TombstonePersonalityOutput {
        status: resp.status,
        idempotent_replay: resp.idempotent_replay,
        audit_emit_failed,
    })
}
