//! Wire-shape for `WakeEntryDraft` input via MCP tools. Strips the
//! engine-allocated `personality_instance_id` (filled in by the tool
//! layer) and converts `wake_entry_id: Uuid` to `wake_entry_id:
//! Option<W-handle>`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::mcp::{McpToolCtx, McpToolError};
use crate::{
    PersonalityInstanceId, WakeEntryAuthoredBy, WakeEntryDraft, WakeEntryGoalScope,
    WakeEntryTriggerKind,
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
    #[serde(default)]
    pub authored_by: WakeEntryAuthoredBy,
    #[schemars(range(min = 0, max = 1000))]
    pub probability_promille: u16,
    #[serde(default)]
    pub goal_scope: WakeEntryGoalScope,
    #[serde(default)]
    pub instructions: String,
}

fn default_enabled() -> bool {
    true
}

impl WakeEntryDraftInput {
    /// Resolve into a `WakeEntryDraft`. Allocates a fresh UUID when
    /// `wake_entry_id` is `None`; resolves through `ctx.resolve_wake_entry`
    /// otherwise so the call works in `Handles`, `RawIds`, and prefixed-id modes.
    ///
    /// # Errors
    ///
    /// Propagates the `McpToolError` from `ctx.resolve_wake_entry` when
    /// `wake_entry_id` is set but does not resolve.
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
            authored_by: self.authored_by,
            probability_promille: self.probability_promille,
            goal_scope: self.goal_scope,
            instructions: self.instructions,
        })
    }
}
