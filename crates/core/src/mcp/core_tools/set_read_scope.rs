//! `core/set_read_scope` — replace explicit personality read-scope grants.

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::SetReadScopeRequest;
use crate::authz::Role;
use crate::mcp::core_tools::audit::{AuditEmit, emit_personality_config_changed};
use crate::mcp::core_tools::payload::{
    PersonalityConfigChangeSnapshot, PersonalityConfigChangedSubject, PersonalityConfigChangedVerb,
};
use crate::mcp::{McpTool, McpToolCtx, McpToolError};

#[derive(Debug, Default)]
pub struct SetReadScopeTool;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetReadScopeArgs {
    /// `I`-handle for the reader personality whose explicit grants are
    /// replaced.
    pub personality: String,
    /// `I`-handles the reader may read in addition to itself.
    pub readable_personalities: Vec<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SetReadScopeOutput {
    pub personality: String,
    pub readable_count: u32,
    pub readable_personalities: Vec<String>,
    pub audit_emit_failed: Option<String>,
}

impl McpTool for SetReadScopeTool {
    const NAME: &'static str = "core/set_read_scope";
    const DESCRIPTION: &'static str = "Replace explicit cross-personality read grants for one \
         personality. Identity reads are implicit; list only additional readable I-handles.";
    type Args = SetReadScopeArgs;
    type Output = SetReadScopeOutput;

    fn call(
        ctx: McpToolCtx,
        args: SetReadScopeArgs,
    ) -> BoxFuture<'static, Result<SetReadScopeOutput, McpToolError>> {
        Box::pin(async move {
            crate::engine::authorize(&ctx.authz, &ctx.owner.principal, Role::Admin)
                .map_err(|e| McpToolError::Other(e.to_string()))?;
            let pid = ctx.resolve_personality(&args.personality)?;
            let readable_ids = args
                .readable_personalities
                .iter()
                .map(|handle| ctx.resolve_personality(handle))
                .collect::<Result<Vec<_>, _>>()?;
            let storage = ctx
                .storage()
                .ok_or_else(|| McpToolError::Other("engine storage unavailable".into()))?;

            let before = storage
                .list_read_scope(&crate::ListReadScopeRequest {
                    principal: ctx.owner.principal.clone(),
                    org_id: Some(ctx.owner.org_id),
                    reader_personality_instance_id: pid,
                })
                .await
                .ok()
                .map(|response| PersonalityConfigChangeSnapshot::ReadScope {
                    readable_personality_instance_ids: response
                        .readable_personality_instance_ids
                        .into_iter()
                        .map(crate::personality::personality::PersonalityInstanceId::into_inner)
                        .collect::<Vec<_>>(),
                });

            let response = storage
                .set_read_scope(&SetReadScopeRequest {
                    principal: ctx.owner.principal.clone(),
                    org_id: Some(ctx.owner.org_id),
                    reader_personality_instance_id: pid,
                    readable_personality_instance_ids: readable_ids.clone(),
                })
                .await
                .map_err(McpToolError::Storage)?;

            let after_ids = readable_ids
                .iter()
                .copied()
                .filter(|id| *id != pid)
                .map(crate::personality::personality::PersonalityInstanceId::into_inner)
                .collect::<Vec<_>>();
            let after = PersonalityConfigChangeSnapshot::ReadScope {
                readable_personality_instance_ids: after_ids,
            };
            let audit = emit_personality_config_changed(
                &ctx,
                PersonalityConfigChangedVerb::SetReadScope,
                PersonalityConfigChangedSubject::Personality(pid.into_inner()),
                before,
                Some(after),
            )
            .await;
            let audit_emit_failed = match audit {
                AuditEmit::Ok => None,
                AuditEmit::Failed { reason } => Some(reason),
            };

            let readable_personalities = readable_ids
                .into_iter()
                .filter(|id| *id != pid)
                .map(|id| ctx.format_personality(id))
                .collect();
            Ok(SetReadScopeOutput {
                personality: ctx.format_personality(pid),
                readable_count: response.readable_count,
                readable_personalities,
                audit_emit_failed,
            })
        })
    }
}
