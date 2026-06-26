//! Pure MCP-to-domain audit adapters for personality config changes.

use crate::engine::{PersonalityConfigAuditEmit, PersonalityConfigChangedInput};
use crate::mcp::McpToolCtx;
use crate::mcp::core_tools::payload::{
    PersonalityConfigChangeSnapshot, PersonalityConfigChangedSubject, PersonalityConfigChangedVerb,
};

pub use crate::engine::PersonalityConfigAuditEmit as AuditEmit;

/// Build the domain audit input from MCP caller identity fields.
///
/// # Errors
///
/// Returns an error when the MCP server did not populate
/// `caller_self_perspective`.
pub fn personality_config_changed_input(
    ctx: &McpToolCtx,
    verb: PersonalityConfigChangedVerb,
    subject: PersonalityConfigChangedSubject,
    before: Option<PersonalityConfigChangeSnapshot>,
    after: Option<PersonalityConfigChangeSnapshot>,
) -> Result<PersonalityConfigChangedInput, String> {
    let caller_self_perspective = ctx
        .caller_self_perspective
        .ok_or_else(|| "caller_self_perspective missing for audit emit".to_string())?;
    Ok(PersonalityConfigChangedInput {
        caller_self_perspective,
        is_master_token: ctx.master_token_id.is_some(),
        verb,
        subject,
        before,
        after,
    })
}

#[must_use]
pub fn audit_emit_failed(
    preflight_failure: Option<String>,
    audit_emit: PersonalityConfigAuditEmit,
) -> Option<String> {
    preflight_failure.or(match audit_emit {
        PersonalityConfigAuditEmit::Ok => None,
        PersonalityConfigAuditEmit::Failed { reason } => Some(reason),
    })
}
