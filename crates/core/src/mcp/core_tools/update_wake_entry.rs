//! `core/update_wake_entry` — granular partial-fields update.
//! `trigger_kind/trigger_id` are immutable; change them via remove + add.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::mcp::core_tools::audit::{AuditEmit, emit_personality_config_changed};
use crate::mcp::core_tools::payload::{
    PersonalityConfigChangeSnapshot, PersonalityConfigChangedSubject, PersonalityConfigChangedVerb,
};
use crate::mcp::{McpToolCtx, McpToolError};
use crate::{MemoryAction, Role};
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
    crate::engine::authorize_action(&ctx.authz, &ctx.owner, Role::Admin, MemoryAction::Admin)
        .map_err(|e| McpToolError::Other(e.to_string()))?;
    let wid = ctx.resolve_wake_entry(&args.wake_entry)?;
    let storage = ctx
        .storage()
        .ok_or_else(|| McpToolError::Other("engine storage unavailable".into()))?;

    // Locate the personality owning this wake entry. Owner-scoped
    // lookup is via list_personality_instances.
    let rows = storage
        .list_personality_instances(&ctx.owner, true)
        .await
        .map_err(McpToolError::Storage)?;
    let pid = rows
        .iter()
        .find(|r| r.wake_entries.iter().any(|e| e.wake_entry_id == wid))
        .map(|r| r.personality_instance_id)
        .ok_or_else(|| {
            McpToolError::Other(format!(
                "wake entry {} not found for owner",
                args.wake_entry
            ))
        })?;

    let patch = args.patch.clone();
    let mutator: crate::WakeEntriesMutator = Box::new(move |current| {
        let mut next: Vec<_> = current.to_vec();
        let entry = next
            .iter_mut()
            .find(|e| e.wake_entry_id == wid)
            .ok_or_else(|| format!("wake entry {wid} no longer present"))?;
        if let Some(v) = patch.label {
            entry.label = v;
        }
        if let Some(v) = patch.enabled {
            entry.enabled = v;
        }
        if let Some(v) = patch.instructions {
            entry.instructions = v;
        }
        if let Some(v) = patch.probability_promille {
            entry.probability_promille = v;
        }
        if let Some(v) = patch.authored_by {
            entry.authored_by = v;
        }
        if let Some(v) = patch.goal_scope {
            entry.goal_scope = v;
        }
        crate::personality::validate_wake_entries_detect_config(&next)
            .map_err(|err| err.to_string())?;
        Ok(next)
    });
    storage
        .set_wake_entries_within(&ctx.owner, pid, mutator)
        .await
        .map_err(|e| McpToolError::Other(e.to_string()))?;

    let audit = emit_personality_config_changed(
        &ctx,
        PersonalityConfigChangedVerb::UpdateWakeEntry,
        PersonalityConfigChangedSubject::WakeEntry(wid),
        Some(PersonalityConfigChangeSnapshot::WakeEntry {
            wake_entry_id: wid,
            patch_applied: Some(true),
        }),
        None,
    )
    .await;
    let audit_emit_failed = match audit {
        AuditEmit::Ok => None,
        AuditEmit::Failed { reason } => Some(reason),
    };
    Ok(UpdateWakeEntryOutput {
        wake_entry: ctx.format_wake_entry(wid),
        audit_emit_failed,
    })
}
