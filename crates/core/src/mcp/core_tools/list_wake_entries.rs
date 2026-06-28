//! `core/list_wake_entries` — read-only wake-entries projection for one
//! personality, with W-handles assigned.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::mcp::{McpToolCtx, McpToolError};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListWakeEntriesArgs {
    /// `I`-handle for the personality whose wake entries to list.
    pub personality: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ListWakeEntriesItem {
    /// `W`-prefixed handle. Pass as `wake_entry` to `update_wake_entry`,
    /// `remove_wake_entry`.
    pub wake_entry: String,
    pub trigger_kind: String,
    pub trigger_id: String,
    pub label: String,
    pub enabled: bool,
    pub instructions: String,
    pub probability_promille: u16,
    pub goal_scope: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ListWakeEntriesOutput {
    pub wake_entries: Vec<ListWakeEntriesItem>,
}

pub(super) async fn list_wake_entries(
    ctx: McpToolCtx,
    args: ListWakeEntriesArgs,
) -> Result<ListWakeEntriesOutput, McpToolError> {
    let pid = ctx.resolve_personality(&args.personality)?;
    let engine = ctx
        .engine()
        .ok_or_else(|| McpToolError::Other("engine unavailable".into()))?;
    let rows = engine
        .list_personality_instances(&ctx.authz, &ctx.owner, true)
        .await?;
    let row = rows
        .into_iter()
        .find(|r| r.personality_instance_id == pid)
        .ok_or_else(|| {
            McpToolError::Other(format!("personality {} not found", args.personality))
        })?;
    let wake_entries = row
        .wake_entries
        .into_iter()
        .map(|e| ListWakeEntriesItem {
            wake_entry: ctx.format_wake_entry(e.wake_entry_id),
            trigger_kind: e.trigger_kind.as_str().to_string(),
            trigger_id: e.trigger_id,
            label: e.label,
            enabled: e.enabled,
            instructions: e.instructions,
            probability_promille: e.probability_promille,
            goal_scope: e.goal_scope.as_str().to_string(),
        })
        .collect();
    Ok(ListWakeEntriesOutput { wake_entries })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authz::{AuthPath, AuthzContext};
    use crate::mcp::HandleTable;
    use crate::mcp::OutputMode;
    use crate::{Engine, FlavorRegistry, McpAuthorContext, McpToolExtensions, OwnerRef, UserId};
    use std::sync::Arc;

    #[tokio::test]
    async fn list_wake_entries_unknown_handle_errs() {
        let owner = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));
        let engine = Arc::new(Engine::new(FlavorRegistry::new().freeze()));
        let ctx = McpToolCtx {
            owner,
            authz: AuthzContext::single_owner(&owner, AuthPath::System),
            handles: Some(Arc::new(HandleTable::new())),
            mode: OutputMode::Handles,
            registry: Arc::new(FlavorRegistry::new().freeze()),
            author: McpAuthorContext {
                model_id: "t".into(),
                client_name: "t".into(),
                client_version: "0".into(),
                personality_instance_id: None,
                caller_self_perspective: None,
            },
            caller_self_perspective: None,
            master_token_id: None,
            extensions: McpToolExtensions::default(),
            engine: Some(engine),
        };
        let err = list_wake_entries(
            ctx,
            ListWakeEntriesArgs {
                personality: "I99".into(),
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(
            err,
            McpToolError::Resolve(crate::mcp::ResolveError::Unknown { .. })
        ));
    }
}
