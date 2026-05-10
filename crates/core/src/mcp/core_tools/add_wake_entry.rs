//! `core/add_wake_entry` — granular append via Storage::set_wake_entries_within.

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::mcp::core_tools::audit::{AuditEmit, emit_personality_config_changed};
use crate::mcp::core_tools::payload::{
    PersonalityConfigChangedSubject, PersonalityConfigChangedVerb,
};
use crate::mcp::core_tools::wake_entry_input::WakeEntryDraftInput;
use crate::mcp::{McpTool, McpToolCtx, McpToolError};

#[derive(Debug, Default)]
pub struct AddWakeEntryTool;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AddWakeEntryArgs {
    pub personality: String,
    pub entry: WakeEntryDraftInput,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct AddWakeEntryOutput {
    pub handle: String,
    pub audit_emit_failed: Option<String>,
}

impl McpTool for AddWakeEntryTool {
    const NAME: &'static str = "core/add_wake_entry";
    const DESCRIPTION: &'static str = "Append one wake entry to a personality. Conflicts with an existing \
         (trigger_kind, trigger_id) on the personality return an error.";
    type Args = AddWakeEntryArgs;
    type Output = AddWakeEntryOutput;

    fn call(
        ctx: McpToolCtx,
        args: AddWakeEntryArgs,
    ) -> BoxFuture<'static, Result<AddWakeEntryOutput, McpToolError>> {
        Box::pin(async move {
            let pid = ctx
                .handles
                .resolve_personality(&args.personality)
                .ok_or_else(|| McpToolError::UnknownHandle(args.personality.clone()))?;
            let storage = ctx
                .storage()
                .ok_or_else(|| McpToolError::Other("engine storage unavailable".into()))?;

            // Resolve input now so handle errors fail fast (before tx).
            let new_draft = args.entry.into_draft(&ctx.handles, pid)?;
            let new_id = new_draft.wake_entry_id;
            let new_trigger_kind = new_draft.trigger_kind;
            let new_trigger_id = new_draft.trigger_id.clone();

            let mutator: crate::WakeEntriesMutator = Box::new(move |current| {
                if current
                    .iter()
                    .any(|e| e.trigger_kind == new_trigger_kind && e.trigger_id == new_trigger_id)
                {
                    return Err(format!(
                        "wake entry with trigger ({:?}, {}) already exists",
                        new_trigger_kind, new_trigger_id
                    ));
                }
                let mut next: Vec<_> = current.to_vec();
                next.push(new_draft);
                Ok(next)
            });
            storage
                .set_wake_entries_within(&ctx.owner, pid, mutator)
                .await
                .map_err(|e| McpToolError::Other(e.to_string()))?;

            let after = serde_json::json!({ "wake_entry_id": new_id });
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
            let w_handle = ctx.handles.assign_wake_entry(new_id);
            Ok(AddWakeEntryOutput {
                handle: w_handle.as_str().to_string(),
                audit_emit_failed,
            })
        })
    }
}
