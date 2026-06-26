//! `core/tombstone_personality` — wraps `Engine::tombstone_personality`
//! and emits an audit Fact.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::TombstonePersonalityRequest;
use crate::mcp::core_tools::audit::{audit_emit_failed, personality_config_changed_input};
use crate::mcp::core_tools::payload::{
    PersonalityConfigChangedSubject, PersonalityConfigChangedVerb,
};
use crate::mcp::{McpToolCtx, McpToolError};

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
    validate_confirm_gate(args.confirm, &args.expect_handle, &args.personality)?;
    let pid = ctx.resolve_personality(&args.personality)?;
    let engine = ctx
        .engine()
        .ok_or_else(|| McpToolError::Other("engine unavailable".into()))?;
    let req = TombstonePersonalityRequest {
        principal: ctx.owner.clone(),
        personality_instance_id: pid,
    };
    let (audit, preflight_failure) = match personality_config_changed_input(
        &ctx,
        PersonalityConfigChangedVerb::Tombstone,
        PersonalityConfigChangedSubject::Personality(pid.into_inner()),
        None,
        None,
    ) {
        Ok(input) => (Some(input), None),
        Err(reason) => (None, Some(reason)),
    };
    let resp = engine
        .tombstone_personality_with_audit(&ctx.authz, req, audit)
        .await
        .map_err(|e| McpToolError::Other(e.to_string()))?;
    Ok(TombstonePersonalityOutput {
        status: resp.response.status,
        idempotent_replay: resp.response.idempotent_replay,
        audit_emit_failed: audit_emit_failed(preflight_failure, resp.audit_emit),
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
