//! `core/list_personalities` — read-only enumeration of the owner's
//! personalities, returning handles instead of UUIDs.

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::McpTool;
use crate::mcp::{McpToolCtx, McpToolError};

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
    pub handle: String,
    pub display_name: String,
    pub status: String,
    pub root_perspective: String,
    pub wake_entry_count: u32,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ListPersonalitiesOutput {
    pub personalities: Vec<ListPersonalitiesItem>,
}

impl McpTool for ListPersonalitiesTool {
    const NAME: &'static str = "core/list_personalities";
    const DESCRIPTION: &'static str = "List personality instances for the authenticated owner. Returns handles \
         (P-prefixed) usable in subsequent CRUD calls.";
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
                    let p_handle = ctx.handles.assign_personality(row.personality_instance_id);
                    let n_handle = ctx
                        .handles
                        .assign_memory(row.current_root_perspective_memory_id);
                    let count = u32::try_from(row.wake_entries.len()).unwrap_or(u32::MAX);
                    ListPersonalitiesItem {
                        handle: p_handle.as_str().to_string(),
                        display_name: row.display_name,
                        status: row.status,
                        root_perspective: n_handle.as_str().to_string(),
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
    use crate::auth::NoAuth;
    use crate::mcp::HandleTable;
    use crate::verbs::query::MemoryStore;
    use crate::{Engine, FlavorRegistry, McpAuthorContext, OrgId, Owner, Principal, UserId};
    use std::sync::Arc;

    fn make_ctx() -> McpToolCtx {
        let owner = Owner {
            principal: Principal::User(UserId::new(uuid::Uuid::now_v7())),
            org_id: OrgId::new(uuid::Uuid::now_v7()),
        };
        let resolver = NoAuth::new(owner.principal.clone(), owner.clone());
        let engine = Arc::new(Engine::new(
            FlavorRegistry::new().freeze(),
            MemoryStore::new(),
            Box::new(resolver),
        ));
        McpToolCtx {
            pool: sqlx::PgPool::connect_lazy("postgres://x/x").expect("lazy"),
            owner,
            handles: Arc::new(HandleTable::new()),
            registry: Arc::new(FlavorRegistry::new().freeze()),
            author: McpAuthorContext {
                model_id: "t".into(),
                client_name: "t".into(),
                client_version: "0".into(),
                caller_self_perspective: None,
            },
            caller_self_perspective: None,
            master_token_id: None,
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
