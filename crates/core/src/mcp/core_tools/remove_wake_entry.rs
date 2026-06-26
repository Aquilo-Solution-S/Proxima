//! `core/remove_wake_entry` — granular delete via `Storage::set_wake_entries_within`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::engine::RemoveWakeEntryRequest;
use crate::mcp::core_tools::audit::{audit_emit_failed, personality_config_changed_input};
use crate::mcp::core_tools::payload::{
    PersonalityConfigChangeSnapshot, PersonalityConfigChangedSubject, PersonalityConfigChangedVerb,
};
use crate::mcp::{McpToolCtx, McpToolError};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RemoveWakeEntryArgs {
    pub wake_entry: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct RemoveWakeEntryOutput {
    pub removed: bool,
    pub audit_emit_failed: Option<String>,
}

pub(super) async fn remove_wake_entry(
    ctx: McpToolCtx,
    args: RemoveWakeEntryArgs,
) -> Result<RemoveWakeEntryOutput, McpToolError> {
    let wid = ctx.resolve_wake_entry(&args.wake_entry)?;
    let engine = ctx
        .engine()
        .ok_or_else(|| McpToolError::Other("engine unavailable".into()))?;

    let (audit, preflight_failure) = match personality_config_changed_input(
        &ctx,
        PersonalityConfigChangedVerb::RemoveWakeEntry,
        PersonalityConfigChangedSubject::WakeEntry(wid),
        Some(PersonalityConfigChangeSnapshot::WakeEntry {
            wake_entry_id: wid,
            patch_applied: None,
        }),
        None,
    ) {
        Ok(input) => (Some(input), None),
        Err(reason) => (None, Some(reason)),
    };
    let resp = engine
        .remove_wake_entry(
            &ctx.authz,
            &RemoveWakeEntryRequest {
                principal: ctx.owner.clone(),
                wake_entry_id: wid,
                audit,
            },
        )
        .await
        .map_err(|e| McpToolError::Other(e.to_string()))?;
    Ok(RemoveWakeEntryOutput {
        removed: resp.removed,
        audit_emit_failed: if resp.removed {
            audit_emit_failed(preflight_failure, resp.audit_emit)
        } else {
            None
        },
    })
}
