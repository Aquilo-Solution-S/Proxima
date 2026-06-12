//! `core/list_wake_entries` — read-only wake-entries projection for one
//! personality, with W-handles assigned.

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::McpTool;
use crate::intervention::InterventionPolicy;
use crate::mcp::{McpToolCtx, McpToolError};

#[derive(Debug, Default)]
pub struct ListWakeEntriesTool;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListWakeEntriesArgs {
    /// `I`-handle for the personality whose wake entries to list.
    pub personality: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ListWakeEntriesItem {
    /// `W`-prefixed handle. Pass as `wake_entry` to `update_wake_entry`,
    /// `remove_wake_entry`, and `replay_wake_events`.
    pub wake_entry: String,
    pub trigger_kind: String,
    pub trigger_id: String,
    pub label: String,
    pub enabled: bool,
    pub instructions: String,
    pub probability_promille: u16,
    pub goal_scope: String,
    pub required_produced_schema_ids: Vec<String>,
    pub max_rounds: u16,
    pub intervention_policy: Option<InterventionPolicy>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ListWakeEntriesOutput {
    pub wake_entries: Vec<ListWakeEntriesItem>,
}

impl McpTool for ListWakeEntriesTool {
    const NAME: &'static str = "core/list_wake_entries";
    const DESCRIPTION: &'static str = "List wake entries on one personality. Args: \
         `{\"personality\": \"I1\"}`. Each item carries a `wake_entry` field (W-handle) — pass that \
         value as the `wake_entry` argument to update_wake_entry, remove_wake_entry, or \
         replay_wake_events. Use core/get_personality for the full per-entry payload.";
    type Args = ListWakeEntriesArgs;
    type Output = ListWakeEntriesOutput;

    fn call(
        ctx: McpToolCtx,
        args: ListWakeEntriesArgs,
    ) -> BoxFuture<'static, Result<ListWakeEntriesOutput, McpToolError>> {
        Box::pin(async move {
            let pid = ctx.resolve_personality(&args.personality)?;
            let storage = ctx
                .storage()
                .ok_or_else(|| McpToolError::Other("engine storage unavailable".into()))?;
            let rows = storage
                .list_personality_instances(&ctx.owner, true)
                .await
                .map_err(McpToolError::Storage)?;
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
                    trigger_kind: format!("{:?}", e.trigger_kind),
                    trigger_id: e.trigger_id,
                    label: e.label,
                    enabled: e.enabled,
                    instructions: e.instructions,
                    probability_promille: e.probability_promille,
                    goal_scope: e.goal_scope.as_str().to_string(),
                    required_produced_schema_ids: e.required_produced_schema_ids,
                    max_rounds: e.max_rounds,
                    intervention_policy: e.intervention_policy,
                })
                .collect();
            Ok(ListWakeEntriesOutput { wake_entries })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authz::{AuthPath, AuthzContext};
    use crate::mcp::HandleTable;
    use crate::mcp::OutputMode;
    use crate::verbs::query::MemoryStore;
    use crate::{Engine, FlavorRegistry, McpAuthorContext, OrgId, Owner, Principal, UserId};
    use std::sync::Arc;

    #[tokio::test]
    async fn list_wake_entries_unknown_handle_errs() {
        let owner = Owner {
            principal: Principal::User(UserId::new(uuid::Uuid::now_v7())),
            org_id: OrgId::new(uuid::Uuid::now_v7()),
        };
        let engine = Arc::new(Engine::new(
            FlavorRegistry::new().freeze(),
            MemoryStore::new(),
        ));
        let ctx = McpToolCtx {
            pool: sqlx::PgPool::connect_lazy("postgres://x/x").expect("lazy"),
            owner: owner.clone(),
            authz: AuthzContext::single_owner(&owner, AuthPath::System),
            handles: Some(Arc::new(HandleTable::new())),
            mode: OutputMode::Handles,
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
        };
        let err = ListWakeEntriesTool::call(
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
