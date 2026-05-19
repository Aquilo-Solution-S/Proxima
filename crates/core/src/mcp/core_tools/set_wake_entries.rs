//! `core/set_wake_entries` — replace-all bulk write of a personality's
//! wake entries. Mirrors `Engine::set_wake_entries`.

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::SetWakeEntriesRequest;
use crate::auth::Credentials;
use crate::mcp::core_tools::audit::{AuditEmit, emit_personality_config_changed};
use crate::mcp::core_tools::payload::{
    PersonalityConfigChangeSnapshot, PersonalityConfigChangedSubject, PersonalityConfigChangedVerb,
};
use crate::mcp::core_tools::wake_entry_input::WakeEntryDraftInput;
use crate::mcp::{McpTool, McpToolCtx, McpToolError};

#[derive(Debug, Default)]
pub struct SetWakeEntriesTool;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetWakeEntriesArgs {
    pub personality: String,
    pub entries: Vec<WakeEntryDraftInput>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SetWakeEntriesOutput {
    pub active_entries: u32,
    pub entry_handles: Vec<String>,
    pub audit_emit_failed: Option<String>,
}

impl McpTool for SetWakeEntriesTool {
    const NAME: &'static str = "core/set_wake_entries";
    const DESCRIPTION: &'static str = "Replace all wake entries for a personality. Carry-over entries \
         keep their identity by passing the W-handle from list_wake_entries \
         in wake_entry_id; omit wake_entry_id for new entries.";
    type Args = SetWakeEntriesArgs;
    type Output = SetWakeEntriesOutput;

    fn call(
        ctx: McpToolCtx,
        args: SetWakeEntriesArgs,
    ) -> BoxFuture<'static, Result<SetWakeEntriesOutput, McpToolError>> {
        Box::pin(async move {
            let pid = ctx.resolve_personality(&args.personality)?;
            let engine = ctx
                .engine()
                .ok_or_else(|| McpToolError::Other("engine unavailable".into()))?;
            let storage = ctx
                .storage()
                .ok_or_else(|| McpToolError::Other("engine storage unavailable".into()))?;

            // Snapshot before for audit.
            let before_rows = storage
                .list_personality_instances(&ctx.owner, true)
                .await
                .map_err(McpToolError::Storage)?;
            let before = before_rows
                .iter()
                .find(|r| r.personality_instance_id == pid)
                .map(|r| PersonalityConfigChangeSnapshot::WakeEntries {
                    wake_entry_count: r.wake_entries.len(),
                    wake_entry_ids: r.wake_entries.iter().map(|e| e.wake_entry_id).collect(),
                });

            // Resolve inputs into drafts.
            let drafts = args
                .entries
                .into_iter()
                .map(|input| input.into_draft(&ctx, pid))
                .collect::<Result<Vec<_>, _>>()?;

            let req = SetWakeEntriesRequest {
                owner: ctx.owner.clone(),
                personality_instance_id: pid,
                entries: drafts.clone(),
            };
            let resp = engine
                .set_wake_entries(&Credentials::None, &req)
                .await
                .map_err(|e| McpToolError::Other(e.to_string()))?;

            let entry_handles: Vec<String> = drafts
                .iter()
                .map(|d| ctx.format_wake_entry(d.wake_entry_id))
                .collect();

            let after = PersonalityConfigChangeSnapshot::WakeEntries {
                wake_entry_count: drafts.len(),
                wake_entry_ids: drafts.iter().map(|d| d.wake_entry_id).collect(),
            };
            let audit = emit_personality_config_changed(
                &ctx,
                PersonalityConfigChangedVerb::SetWakeEntries,
                PersonalityConfigChangedSubject::Personality(pid.into_inner()),
                before,
                Some(after),
            )
            .await;
            let audit_emit_failed = match audit {
                AuditEmit::Ok => None,
                AuditEmit::Failed { reason } => Some(reason),
            };
            Ok(SetWakeEntriesOutput {
                active_entries: resp.active_entries,
                entry_handles,
                audit_emit_failed,
            })
        })
    }
}
