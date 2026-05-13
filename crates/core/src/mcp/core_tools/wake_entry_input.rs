//! Wire-shape for `WakeEntryDraft` input via MCP tools. Strips the
//! engine-allocated `personality_instance_id` (filled in by the tool
//! layer) and converts `wake_entry_id: Uuid` to `wake_entry_id:
//! Option<W-handle>`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::mcp::{McpToolCtx, McpToolError};
use crate::{
    ModelTier, PersonalityInstanceId, WakeEntryAuthoredBy, WakeEntryDraft, WakeEntryGoalScope,
    WakeEntryTriggerKind, WakeExecutionMode,
};

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct WakeEntryDraftInput {
    /// Optional W-handle. Omit for new entries (UUID is allocated);
    /// pass an existing handle to preserve identity in a bulk replace.
    #[serde(default)]
    pub wake_entry_id: Option<String>,
    pub trigger_kind: WakeEntryTriggerKind,
    pub trigger_id: String,
    pub label: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default = "default_execution_mode")]
    pub execution_mode: WakeExecutionMode,
    #[serde(default)]
    pub authored_by: WakeEntryAuthoredBy,
    #[schemars(range(min = 0, max = 1000))]
    pub probability_promille: u16,
    #[serde(default)]
    pub goal_scope: WakeEntryGoalScope,
    #[serde(default)]
    pub instructions: String,
    #[serde(default = "default_model_tier")]
    pub model_tier: ModelTier,
    #[serde(default)]
    pub inference_target_ref: Option<String>,
    #[serde(default)]
    pub substrate_tool_palette: Vec<String>,
    #[serde(default)]
    pub workspace_tool_palette: Vec<String>,
    #[schemars(range(min = 0))]
    pub max_rounds: u16,
}

fn default_enabled() -> bool {
    true
}
fn default_execution_mode() -> WakeExecutionMode {
    WakeExecutionMode::SubstrateOnly
}
fn default_model_tier() -> ModelTier {
    ModelTier::Standard
}

impl WakeEntryDraftInput {
    /// Resolve into a `WakeEntryDraft`. Allocates a fresh UUID when
    /// `wake_entry_id` is `None`; resolves through `ctx.resolve_wake_entry`
    /// otherwise so the call works in both `Handles` and `RawIds` modes.
    pub fn into_draft(
        self,
        ctx: &McpToolCtx,
        personality_instance_id: PersonalityInstanceId,
    ) -> Result<WakeEntryDraft, McpToolError> {
        let wake_entry_id = match self.wake_entry_id {
            None => uuid::Uuid::now_v7(),
            Some(handle) => ctx.resolve_wake_entry(&handle)?,
        };
        Ok(WakeEntryDraft {
            wake_entry_id,
            personality_instance_id,
            trigger_kind: self.trigger_kind,
            trigger_id: self.trigger_id,
            label: self.label,
            enabled: self.enabled,
            execution_mode: self.execution_mode,
            authored_by: self.authored_by,
            probability_promille: self.probability_promille,
            goal_scope: self.goal_scope,
            instructions: self.instructions,
            model_tier: self.model_tier,
            inference_target_ref: self.inference_target_ref,
            substrate_tool_palette: self.substrate_tool_palette,
            workspace_tool_palette: self.workspace_tool_palette,
            max_rounds: self.max_rounds,
        })
    }
}
