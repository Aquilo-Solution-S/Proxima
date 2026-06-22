//! `core/remove_wake_entry` — granular delete via `Storage::set_wake_entries_within`.

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::McpTool;
use crate::authz::Role;
use crate::mcp::core_tools::audit::{AuditEmit, emit_personality_config_changed};
use crate::mcp::core_tools::payload::{
    PersonalityConfigChangeSnapshot, PersonalityConfigChangedSubject, PersonalityConfigChangedVerb,
};
use crate::mcp::{McpToolCtx, McpToolError};

#[derive(Debug, Default)]
pub struct RemoveWakeEntryTool;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RemoveWakeEntryArgs {
    pub wake_entry: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct RemoveWakeEntryOutput {
    pub removed: bool,
    pub audit_emit_failed: Option<String>,
}

impl McpTool for RemoveWakeEntryTool {
    const NAME: &'static str = "core/remove_wake_entry";
    const DESCRIPTION: &'static str = "Remove one wake entry. Idempotent: returns removed=false if the \
         entry was already absent.";
    type Args = RemoveWakeEntryArgs;
    type Output = RemoveWakeEntryOutput;

    fn call(
        ctx: McpToolCtx,
        args: RemoveWakeEntryArgs,
    ) -> BoxFuture<'static, Result<RemoveWakeEntryOutput, McpToolError>> {
        Box::pin(async move {
            crate::engine::authorize(&ctx.authz, &ctx.owner, Role::Admin)
                .map_err(|e| McpToolError::Other(e.to_string()))?;
            let wid = ctx.resolve_wake_entry(&args.wake_entry)?;
            let storage = ctx
                .storage()
                .ok_or_else(|| McpToolError::Other("engine storage unavailable".into()))?;
            let rows = storage
                .list_personality_instances(&ctx.owner, true)
                .await
                .map_err(McpToolError::Storage)?;
            let Some(row) = rows
                .iter()
                .find(|r| r.wake_entries.iter().any(|e| e.wake_entry_id == wid))
            else {
                return Ok(RemoveWakeEntryOutput {
                    removed: false,
                    audit_emit_failed: None,
                });
            };
            let pid = row.personality_instance_id;

            let mutator: crate::WakeEntriesMutator = Box::new(move |current| {
                Ok(current
                    .iter()
                    .filter(|e| e.wake_entry_id != wid)
                    .cloned()
                    .collect())
            });
            storage
                .set_wake_entries_within(&ctx.owner, pid, mutator)
                .await
                .map_err(|e| McpToolError::Other(e.to_string()))?;

            let audit = emit_personality_config_changed(
                &ctx,
                PersonalityConfigChangedVerb::RemoveWakeEntry,
                PersonalityConfigChangedSubject::WakeEntry(wid),
                Some(PersonalityConfigChangeSnapshot::WakeEntry {
                    wake_entry_id: wid,
                    patch_applied: None,
                }),
                None,
            )
            .await;
            let audit_emit_failed = match audit {
                AuditEmit::Ok => None,
                AuditEmit::Failed { reason } => Some(reason),
            };
            Ok(RemoveWakeEntryOutput {
                removed: true,
                audit_emit_failed,
            })
        })
    }
}
