//! `core/add_wake_entry` — granular append via `Storage::set_wake_entries_within`.

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};

use crate::mcp::core_tools::audit::{AuditEmit, emit_personality_config_changed};
use crate::mcp::core_tools::payload::{
    PersonalityConfigChangeSnapshot, PersonalityConfigChangedSubject, PersonalityConfigChangedVerb,
};
use crate::mcp::core_tools::wake_entry_input::WakeEntryDraftInput;
use crate::mcp::{McpTool, McpToolCtx, McpToolError};

#[derive(Debug, Default)]
pub struct AddWakeEntryTool;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AddWakeEntryArgs {
    pub personality: String,
    #[serde(deserialize_with = "deserialize_wake_entry_input")]
    pub entry: WakeEntryDraftInput,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct AddWakeEntryOutput {
    /// `W`-prefixed handle for the new entry. Pass as `wake_entry` to
    /// `update_wake_entry`, `remove_wake_entry`, or `replay_wake_events`.
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

impl McpTool for AddWakeEntryTool {
    const NAME: &'static str = "core/add_wake_entry";
    const DESCRIPTION: &'static str = "Append one wake entry to a personality. Args: \
         `{\"personality\": \"I1\", \"entry\": …}`. Returns the new W-handle in the `wake_entry` field. \
         Conflicts with an existing (trigger_kind, trigger_id) on the personality return an error.";
    type Args = AddWakeEntryArgs;
    type Output = AddWakeEntryOutput;

    fn call(
        ctx: McpToolCtx,
        args: AddWakeEntryArgs,
    ) -> BoxFuture<'static, Result<AddWakeEntryOutput, McpToolError>> {
        Box::pin(async move {
            let pid = ctx.resolve_personality(&args.personality)?;
            let storage = ctx
                .storage()
                .ok_or_else(|| McpToolError::Other("engine storage unavailable".into()))?;

            // Resolve input now so handle errors fail fast (before tx).
            let new_draft = args.entry.into_draft(&ctx, pid)?;
            let new_id = new_draft.wake_entry_id;
            let new_trigger_kind = new_draft.trigger_kind;
            let new_trigger_id = new_draft.trigger_id.clone();
            let registry = ctx.registry.clone();

            let mutator: crate::WakeEntriesMutator = Box::new(move |current| {
                if current
                    .iter()
                    .any(|e| e.trigger_kind == new_trigger_kind && e.trigger_id == new_trigger_id)
                {
                    return Err(format!(
                        "wake entry with trigger ({new_trigger_kind:?}, {new_trigger_id}) already exists"
                    ));
                }
                let mut next: Vec<_> = current.to_vec();
                next.push(new_draft);
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

            let after = PersonalityConfigChangeSnapshot::WakeEntry {
                wake_entry_id: new_id,
                patch_applied: None,
            };
            let audit = emit_personality_config_changed(
                &ctx,
                PersonalityConfigChangedVerb::AddWakeEntry,
                PersonalityConfigChangedSubject::WakeEntry(new_id),
                None,
                Some(after),
            )
            .await;
            let audit_emit_failed = match audit {
                AuditEmit::Ok => None,
                AuditEmit::Failed { reason } => Some(reason),
            };
            Ok(AddWakeEntryOutput {
                wake_entry: ctx.format_wake_entry(new_id),
                audit_emit_failed,
            })
        })
    }
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
                "trigger_id": "core/chat-message-v1",
                "label": "receive-chat-message",
                "probability_promille": 1000,
                "max_rounds": 2
            }
        }))
        .expect("object entry");

        assert_eq!(args.entry.trigger_id, "core/chat-message-v1");
        assert_eq!(args.entry.max_rounds, 2);
    }

    #[test]
    fn add_wake_entry_accepts_json_string_entry() {
        let entry = serde_json::json!({
            "trigger_kind": "on_memory",
            "trigger_id": "core/chat-message-v1",
            "label": "receive-chat-message",
            "probability_promille": 1000,
            "max_rounds": 2
        })
        .to_string();
        let args: AddWakeEntryArgs = serde_json::from_value(serde_json::json!({
            "personality": "I1",
            "entry": entry
        }))
        .expect("string entry");

        assert_eq!(args.entry.trigger_id, "core/chat-message-v1");
        assert_eq!(args.entry.max_rounds, 2);
    }
}
