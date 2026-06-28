//! `core/update_wake_entry` — granular partial-fields update.
//! `trigger_kind/trigger_id` are immutable; change them via remove + add.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::engine::{UpdateWakeEntryRequest, WakeEntryPatchInput};
use crate::mcp::core_tools::audit::{audit_emit_failed, personality_config_changed_input};
use crate::mcp::core_tools::payload::{
    PersonalityConfigChangeSnapshot, PersonalityConfigChangedSubject, PersonalityConfigChangedVerb,
};
use crate::mcp::{McpToolCtx, McpToolError};
use crate::{WakeEntryAuthoredBy, WakeEntryGoalScope};

#[derive(Debug, Default, Clone, Deserialize, JsonSchema)]
pub struct WakeEntryPatch {
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub instructions: Option<String>,
    #[serde(default)]
    #[schemars(range(min = 0, max = 1000))]
    pub probability_promille: Option<u16>,
    #[serde(default)]
    pub authored_by: Option<WakeEntryAuthoredBy>,
    #[serde(default)]
    pub goal_scope: Option<WakeEntryGoalScope>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UpdateWakeEntryArgs {
    pub wake_entry: String,
    pub patch: WakeEntryPatch,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct UpdateWakeEntryOutput {
    /// `W`-prefixed handle. Matches the `wake_entry` argument that was
    /// passed in.
    pub wake_entry: String,
    pub audit_emit_failed: Option<String>,
}

pub(super) async fn update_wake_entry(
    ctx: McpToolCtx,
    args: UpdateWakeEntryArgs,
) -> Result<UpdateWakeEntryOutput, McpToolError> {
    let wid = ctx.resolve_wake_entry(&args.wake_entry)?;
    let engine = ctx
        .engine()
        .ok_or_else(|| McpToolError::Other("engine unavailable".into()))?;

    let (audit, preflight_failure) = match personality_config_changed_input(
        &ctx,
        PersonalityConfigChangedVerb::UpdateWakeEntry,
        PersonalityConfigChangedSubject::WakeEntry(wid),
        Some(PersonalityConfigChangeSnapshot::WakeEntry {
            wake_entry_id: wid,
            patch_applied: Some(true),
        }),
        None,
    ) {
        Ok(input) => (Some(input), None),
        Err(reason) => (None, Some(reason)),
    };
    let resp = engine
        .update_wake_entry(
            &ctx.authz,
            &UpdateWakeEntryRequest {
                principal: ctx.owner,
                wake_entry_id: wid,
                patch: WakeEntryPatchInput::from(args.patch),
                audit,
            },
        )
        .await
        .map_err(|e| McpToolError::Other(e.to_string()))?;
    Ok(UpdateWakeEntryOutput {
        wake_entry: ctx.format_wake_entry(resp.wake_entry_id),
        audit_emit_failed: audit_emit_failed(preflight_failure, resp.audit_emit),
    })
}

impl From<WakeEntryPatch> for WakeEntryPatchInput {
    fn from(value: WakeEntryPatch) -> Self {
        Self {
            label: value.label,
            enabled: value.enabled,
            instructions: value.instructions,
            probability_promille: value.probability_promille,
            authored_by: value.authored_by,
            goal_scope: value.goal_scope,
        }
    }
}
