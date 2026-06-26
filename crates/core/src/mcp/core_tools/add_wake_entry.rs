//! `core/add_wake_entry` — granular append via `Storage::set_wake_entries_within`.

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};

use crate::engine::AddWakeEntryRequest;
use crate::mcp::core_tools::audit::{audit_emit_failed, personality_config_changed_input};
use crate::mcp::core_tools::payload::{
    PersonalityConfigChangeSnapshot, PersonalityConfigChangedSubject, PersonalityConfigChangedVerb,
};
use crate::mcp::core_tools::wake_entry_input::WakeEntryDraftInput;
use crate::mcp::{McpToolCtx, McpToolError};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AddWakeEntryArgs {
    pub personality: String,
    #[serde(deserialize_with = "deserialize_wake_entry_input")]
    pub entry: WakeEntryDraftInput,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct AddWakeEntryOutput {
    /// `W`-prefixed handle for the new entry. Pass as `wake_entry` to
    /// `update_wake_entry` or `remove_wake_entry`.
    pub wake_entry: String,
    pub audit_emit_failed: Option<String>,
}

fn deserialize_wake_entry_input<'de, D>(deserializer: D) -> Result<WakeEntryDraftInput, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    if let Some(raw) = value.as_str() {
        return serde_json::from_str(raw).map_err(serde::de::Error::custom);
    }
    serde_json::from_value(value).map_err(serde::de::Error::custom)
}

pub(super) async fn add_wake_entry(
    ctx: McpToolCtx,
    args: AddWakeEntryArgs,
) -> Result<AddWakeEntryOutput, McpToolError> {
    let pid = ctx.resolve_personality(&args.personality)?;
    let engine = ctx
        .engine()
        .ok_or_else(|| McpToolError::Other("engine unavailable".into()))?;

    // Resolve input now so handle errors fail fast (before tx).
    let new_draft = args.entry.into_draft(&ctx, pid)?;
    let new_id = new_draft.wake_entry_id;

    let after = PersonalityConfigChangeSnapshot::WakeEntry {
        wake_entry_id: new_id,
        patch_applied: None,
    };
    let (audit, preflight_failure) = match personality_config_changed_input(
        &ctx,
        PersonalityConfigChangedVerb::AddWakeEntry,
        PersonalityConfigChangedSubject::WakeEntry(new_id),
        None,
        Some(after),
    ) {
        Ok(input) => (Some(input), None),
        Err(reason) => (None, Some(reason)),
    };
    let resp = engine
        .add_wake_entry(
            &ctx.authz,
            &AddWakeEntryRequest {
                principal: ctx.owner.clone(),
                personality_instance_id: pid,
                entry: new_draft,
                audit,
            },
        )
        .await
        .map_err(|e| McpToolError::Other(e.to_string()))?;
    Ok(AddWakeEntryOutput {
        wake_entry: ctx.format_wake_entry(resp.wake_entry_id),
        audit_emit_failed: audit_emit_failed(preflight_failure, resp.audit_emit),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_wake_entry_accepts_object_entry() {
        let args: AddWakeEntryArgs = serde_json::from_value(serde_json::json!({
            "personality": "I1",
            "entry": {
                "trigger_kind": "on_memory",
                "trigger_id": "core/personality_config_changed_v1",
                "label": "observe-personality-config",
                "probability_promille": 1000
            }
        }))
        .expect("object entry");

        assert_eq!(args.entry.trigger_id, "core/personality_config_changed_v1");
    }

    #[test]
    fn add_wake_entry_accepts_json_string_entry() {
        let entry = serde_json::json!({
            "trigger_kind": "on_memory",
            "trigger_id": "core/personality_config_changed_v1",
            "label": "observe-personality-config",
            "probability_promille": 1000
        })
        .to_string();
        let args: AddWakeEntryArgs = serde_json::from_value(serde_json::json!({
            "personality": "I1",
            "entry": entry
        }))
        .expect("string entry");

        assert_eq!(args.entry.trigger_id, "core/personality_config_changed_v1");
    }
}
