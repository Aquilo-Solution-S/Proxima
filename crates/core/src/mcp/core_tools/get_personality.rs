//! `core/get_personality` — full read of one personality instance,
//! including all wake entries projected with W-handles.

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::McpTool;
use crate::mcp::{McpToolCtx, McpToolError};
use crate::personality::PersonalityStatus;

#[derive(Debug, Default)]
pub struct GetPersonalityTool;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetPersonalityArgs {
    /// `I`-prefixed handle previously returned by `list_personalities` or
    /// `instantiate_personality`.
    pub personality: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct GetPersonalityWakeEntry {
    /// `W`-prefixed handle. Pass as `wake_entry` to `update_wake_entry`,
    /// `remove_wake_entry`.
    pub wake_entry: String,
    pub trigger_kind: String,
    pub trigger_id: String,
    pub label: String,
    pub enabled: bool,
    pub instructions: String,
    pub authored_by: String,
    pub probability_promille: u16,
    pub goal_scope: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct GetPersonalityOutput {
    /// `I`-prefixed handle. Matches the `personality` argument that was
    /// passed in.
    pub personality: String,
    pub display_name: String,
    pub status: PersonalityStatus,
    pub root_perspective: String,
    pub wake_entries: Vec<GetPersonalityWakeEntry>,
}

impl McpTool for GetPersonalityTool {
    const NAME: &'static str = "core_get_personality";
    const DESCRIPTION: &'static str = "Read one personality with all wake entries. Args: \
         `{\"personality\": \"I1\"}` where the value is an I-handle from list_personalities. Each wake \
         entry in the response carries a `wake_entry` field (W-handle) — pass that to update_wake_entry, \
         remove_wake_entry.";
    type Args = GetPersonalityArgs;
    type Output = GetPersonalityOutput;

    fn call(
        ctx: McpToolCtx,
        args: GetPersonalityArgs,
    ) -> BoxFuture<'static, Result<GetPersonalityOutput, McpToolError>> {
        Box::pin(get_personality(ctx, args))
    }
}

pub(super) async fn get_personality(
    ctx: McpToolCtx,
    args: GetPersonalityArgs,
) -> Result<GetPersonalityOutput, McpToolError> {
    let target_id = ctx.resolve_personality(&args.personality)?;
    let storage = ctx
        .storage()
        .ok_or_else(|| McpToolError::Other("engine storage unavailable".into()))?;
    let rows = storage
        .list_personality_instances(&ctx.owner, true)
        .await
        .map_err(McpToolError::Storage)?;
    let row = rows
        .into_iter()
        .find(|r| r.personality_instance_id == target_id)
        .ok_or_else(|| {
            McpToolError::Other(format!(
                "personality {} not found for owner",
                args.personality
            ))
        })?;
    let personality = ctx.format_personality(row.personality_instance_id);
    let root_perspective = ctx.format_perspective_memory(row.current_root_perspective_memory_id);
    let wake_entries = row
        .wake_entries
        .into_iter()
        .map(|e| GetPersonalityWakeEntry {
            wake_entry: ctx.format_wake_entry(e.wake_entry_id),
            trigger_kind: e.trigger_kind.as_str().to_string(),
            trigger_id: e.trigger_id,
            label: e.label,
            enabled: e.enabled,
            instructions: e.instructions,
            authored_by: e.authored_by.as_str().to_string(),
            probability_promille: e.probability_promille,
            goal_scope: e.goal_scope.as_str().to_string(),
        })
        .collect();
    Ok(GetPersonalityOutput {
        personality,
        display_name: row.display_name,
        status: row.status,
        root_perspective,
        wake_entries,
    })
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
    async fn get_personality_unknown_handle_returns_unknown_handle_err() {
        let ctx = make_ctx();
        let err = GetPersonalityTool::call(
            ctx,
            GetPersonalityArgs {
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

    #[tokio::test]
    async fn get_personality_malformed_handle_returns_unknown_handle_err() {
        let ctx = make_ctx();
        let err = GetPersonalityTool::call(
            ctx,
            GetPersonalityArgs {
                personality: "not-a-handle".into(),
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
