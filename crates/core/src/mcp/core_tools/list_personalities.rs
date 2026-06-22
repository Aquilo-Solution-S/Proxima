//! `core/list_personalities` — read-only enumeration of the owner's
//! personalities, returning handles instead of UUIDs.

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::McpTool;
use crate::mcp::{McpToolCtx, McpToolError};
use crate::personality::PersonalityStatus;

#[derive(Debug, Default)]
pub struct ListPersonalitiesTool;

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ListPersonalitiesArgs {
    /// Include tombstoned instances. Default: false.
    #[serde(default)]
    pub include_tombstoned: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ListPersonalitiesItem {
    /// `I`-prefixed handle. Pass as `personality` to `get_personality`,
    /// `tombstone_personality`, `list_wake_entries`, `add_wake_entry`, etc.
    pub personality: String,
    pub display_name: String,
    pub status: PersonalityStatus,
    pub root_perspective: String,
    pub wake_entry_count: u32,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ListPersonalitiesOutput {
    pub personalities: Vec<ListPersonalitiesItem>,
}

impl McpTool for ListPersonalitiesTool {
    const NAME: &'static str = "core_list_personalities";
    const DESCRIPTION: &'static str = "List personality instances for the authenticated owner. Each item \
         carries a `personality` field (I-prefixed handle) — pass that value as the `personality` argument \
         to get_personality, tombstone_personality, list_wake_entries, add_wake_entry, set_wake_entries, \
         and remove_wake_entry.";
    type Args = ListPersonalitiesArgs;
    type Output = ListPersonalitiesOutput;

    fn call(
        ctx: McpToolCtx,
        args: ListPersonalitiesArgs,
    ) -> BoxFuture<'static, Result<ListPersonalitiesOutput, McpToolError>> {
        Box::pin(async move {
            let storage = ctx
                .storage()
                .ok_or_else(|| McpToolError::Other("engine storage unavailable".into()))?;
            let rows = storage
                .list_personality_instances(&ctx.owner, args.include_tombstoned)
                .await
                .map_err(McpToolError::Storage)?;
            let personalities = rows
                .into_iter()
                .map(|row| {
                    let count = u32::try_from(row.wake_entries.len()).unwrap_or(u32::MAX);
                    ListPersonalitiesItem {
                        personality: ctx.format_personality(row.personality_instance_id),
                        display_name: row.display_name,
                        status: row.status,
                        root_perspective: ctx
                            .format_perspective_memory(row.current_root_perspective_memory_id),
                        wake_entry_count: count,
                    }
                })
                .collect();
            Ok(ListPersonalitiesOutput { personalities })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authz::{AuthPath, AuthzContext};
    use crate::mcp::HandleTable;
    use crate::mcp::OutputMode;
    use crate::{Engine, FlavorRegistry, McpAuthorContext, McpToolExtensions, Principal, UserId};
    use std::sync::Arc;

    fn make_ctx() -> McpToolCtx {
        let owner = Principal::User(UserId::new(uuid::Uuid::now_v7()));
        let engine = Arc::new(Engine::new(FlavorRegistry::new().freeze()));
        McpToolCtx {
            owner: owner.clone(),
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
        }
    }

    #[tokio::test]
    async fn list_personalities_against_empty_memory_store_returns_empty() {
        let ctx = make_ctx();
        let out = ListPersonalitiesTool::call(ctx, ListPersonalitiesArgs::default())
            .await
            .expect("ok");
        assert!(out.personalities.is_empty());
    }
}
