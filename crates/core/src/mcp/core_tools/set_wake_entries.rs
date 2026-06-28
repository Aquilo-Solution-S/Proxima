//! `core/set_wake_entries` — replace-all bulk write of a personality's
//! wake entries. Mirrors `Engine::set_wake_entries`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::SetWakeEntriesRequest;
use crate::mcp::core_tools::audit::{audit_emit_failed, personality_config_changed_input};
use crate::mcp::core_tools::payload::{
    PersonalityConfigChangedSubject, PersonalityConfigChangedVerb,
};
use crate::mcp::core_tools::wake_entry_input::WakeEntryDraftInput;
use crate::mcp::{McpToolCtx, McpToolError};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetWakeEntriesArgs {
    pub personality: String,
    pub entries: Vec<WakeEntryDraftInput>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SetWakeEntriesOutput {
    pub active_entries: u32,
    pub entry_handles: Vec<String>,
    pub audit_emit_failed: Option<String>,
}

pub(super) async fn set_wake_entries(
    ctx: McpToolCtx,
    args: SetWakeEntriesArgs,
) -> Result<SetWakeEntriesOutput, McpToolError> {
    let pid = ctx.resolve_personality(&args.personality)?;
    let engine = ctx
        .engine()
        .ok_or_else(|| McpToolError::Other("engine unavailable".into()))?;

    // Resolve inputs into drafts.
    let drafts = args
        .entries
        .into_iter()
        .map(|input| input.into_draft(&ctx, pid))
        .collect::<Result<Vec<_>, _>>()?;

    let req = SetWakeEntriesRequest {
        principal: ctx.owner,
        personality_instance_id: pid,
        entries: drafts.clone(),
    };
    let (audit, preflight_failure) = match personality_config_changed_input(
        &ctx,
        PersonalityConfigChangedVerb::SetWakeEntries,
        PersonalityConfigChangedSubject::Personality(pid.into_inner()),
        None,
        None,
    ) {
        Ok(input) => (Some(input), None),
        Err(reason) => (None, Some(reason)),
    };
    let resp = engine
        .set_wake_entries_with_audit(&ctx.authz, &req, audit)
        .await
        .map_err(|e| McpToolError::Other(e.to_string()))?;

    let entry_handles: Vec<String> = drafts
        .iter()
        .map(|d| ctx.format_wake_entry(d.wake_entry_id))
        .collect();
    Ok(SetWakeEntriesOutput {
        active_entries: resp.response.active_entries,
        entry_handles,
        audit_emit_failed: audit_emit_failed(preflight_failure, resp.audit_emit),
    })
}
