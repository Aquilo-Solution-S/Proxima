//! `core/update_wake_entry` — granular partial-fields update.
//! `trigger_kind/trigger_id` are immutable; change them via remove + add.

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::McpTool;
use crate::intervention::InterventionPolicy;
use crate::mcp::core_tools::audit::{AuditEmit, emit_personality_config_changed};
use crate::mcp::core_tools::payload::{
    PersonalityConfigChangeSnapshot, PersonalityConfigChangedSubject, PersonalityConfigChangedVerb,
};
use crate::mcp::{McpToolCtx, McpToolError};
use crate::{ModelTier, WakeEntryAuthoredBy, WakeEntryGoalScope, WakeExecutionMode};

#[derive(Debug, Default)]
pub struct UpdateWakeEntryTool;

#[derive(Debug, Default, Clone, Deserialize, JsonSchema)]
pub struct WakeEntryPatch {
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub instructions: Option<String>,
    #[serde(default)]
    pub model_tier: Option<ModelTier>,
    /// Outer Option = field present in patch; inner Option = set to None
    /// or to Some(value).
    #[serde(default)]
    pub inference_target_ref: Option<Option<String>>,
    #[serde(default)]
    pub substrate_tool_palette: Option<Vec<String>>,
    #[serde(default)]
    pub required_produced_schema_ids: Option<Vec<String>>,
    #[serde(default)]
    #[schemars(range(min = 0, max = 1000))]
    pub probability_promille: Option<u16>,
    #[serde(default)]
    #[schemars(range(min = 1))]
    pub max_rounds: Option<u16>,
    #[serde(default)]
    pub intervention_policy: Option<Option<InterventionPolicy>>,
    #[serde(default)]
    pub execution_mode: Option<WakeExecutionMode>,
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

impl McpTool for UpdateWakeEntryTool {
    const NAME: &'static str = "core/update_wake_entry";
    const DESCRIPTION: &'static str = "Update one wake entry. Args: \
         `{\"wake_entry\": \"W1\", \"patch\": {…}}` where the W-handle comes from list_wake_entries or \
         get_personality. Only fields present in `patch` change. To change trigger_kind/trigger_id, use \
         remove_wake_entry + add_wake_entry.";
    type Args = UpdateWakeEntryArgs;
    type Output = UpdateWakeEntryOutput;

    fn call(
        ctx: McpToolCtx,
        args: UpdateWakeEntryArgs,
    ) -> BoxFuture<'static, Result<UpdateWakeEntryOutput, McpToolError>> {
        Box::pin(async move {
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
            let registry = ctx.registry.clone();
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
                if let Some(v) = patch.model_tier {
                    entry.model_tier = v;
                }
                if let Some(v) = patch.inference_target_ref {
                    entry.inference_target_ref = v;
                }
                if let Some(v) = patch.substrate_tool_palette {
                    entry.substrate_tool_palette = v;
                }
                if let Some(v) = patch.required_produced_schema_ids {
                    entry.required_produced_schema_ids = v;
                }
                if let Some(v) = patch.probability_promille {
                    entry.probability_promille = v;
                }
                if let Some(v) = patch.max_rounds {
                    entry.max_rounds = v;
                }
                if let Some(v) = patch.intervention_policy {
                    entry.intervention_policy = v;
                }
                if let Some(v) = patch.execution_mode {
                    entry.execution_mode = v;
                }
                if let Some(v) = patch.authored_by {
                    entry.authored_by = v;
                }
                if let Some(v) = patch.goal_scope {
                    entry.goal_scope = v;
                }
                crate::inference::set_wake_entries::validate_wake_entries_static_config(
                    registry.as_ref(),
                    &next,
                )
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
        })
    }
}
