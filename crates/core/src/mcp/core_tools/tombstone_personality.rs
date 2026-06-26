//! `core/tombstone_personality` — wraps `Engine::tombstone_personality`
//! and emits an audit Fact.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::mcp::core_tools::audit::{AuditEmit, emit_personality_config_changed};
use crate::mcp::core_tools::payload::{
    PersonalityConfigChangeSnapshot, PersonalityConfigChangedSubject, PersonalityConfigChangedVerb,
};
use crate::mcp::{McpToolCtx, McpToolError};
use crate::{MemoryAction, TombstonePersonalityRequest};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TombstonePersonalityArgs {
    /// `I`-handle of the personality to tombstone.
    pub personality: String,
    /// Must be true to confirm destructive personality tombstoning.
    pub confirm: bool,
    /// Must exactly echo `personality`.
    pub expect_handle: String,
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
    if !ctx
        .authz
        .allows_memory_action(&ctx.owner, MemoryAction::Admin)
    {
        return Err(
            crate::error::ProtocolError::forbidden("requires memory.admin on owner").into(),
        );
    }
    validate_confirm_gate(args.confirm, &args.expect_handle, &args.personality)?;
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

fn validate_confirm_gate(
    confirm: bool,
    expect_handle: &str,
    personality: &str,
) -> Result<(), McpToolError> {
    if !confirm {
        return Err(McpToolError::InvalidInput("confirm must be true".into()));
    }
    if expect_handle != personality {
        return Err(McpToolError::InvalidInput(
            "expect_handle must equal personality".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::validate_confirm_gate;
    use crate::mcp::McpToolError;

    #[test]
    fn confirm_gate_requires_confirm_true() {
        match validate_confirm_gate(false, "I:target", "I:target") {
            Err(McpToolError::InvalidInput(message)) => {
                assert!(message.contains("confirm"));
            }
            other => panic!("expected confirm invalid input, got {other:?}"),
        }
    }

    #[test]
    fn confirm_gate_requires_expect_handle_match() {
        match validate_confirm_gate(true, "I:other", "I:target") {
            Err(McpToolError::InvalidInput(message)) => {
                assert!(message.contains("expect_handle"));
            }
            other => panic!("expected expect_handle invalid input, got {other:?}"),
        }
    }
}
