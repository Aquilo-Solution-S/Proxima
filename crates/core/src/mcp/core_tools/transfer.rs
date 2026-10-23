use crate::mcp::{Replay, ToolEffect};
use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::access::EntityId;
use crate::mcp::{
    CoreActionMeta, McpActionArgSpec, McpTool, McpToolAudience, McpToolCtx, McpToolError,
};
use crate::protocol::{action as protocol_action, tool as protocol_tool};

pub const CORE_TRANSFER_ACTIONS: &[CoreActionMeta] = &[CoreActionMeta {
    tool: CoreTransferTool::NAME,
    action: "transfer_to_owner",
    scope_key: protocol_action::CORE_TRANSFER_TO_OWNER,
    produces_schema_ids: &[],
}];

#[derive(Debug, Default)]
pub struct CoreTransferTool;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum CoreTransferArgs {
    /// Needs admin on the memory's current space and manage rights on the destination group.
    TransferToOwner(TransferToOwnerArgs),
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TransferToOwnerArgs {
    /// The memory to move: memory UUIDs. Goals do not
    /// transfer.
    pub entity: String,
    /// Destination owner id, a bare `<uuid>`: a group the caller manages.
    pub to_owner: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct TransferOutput {
    pub ok: bool,
}

impl McpTool for CoreTransferTool {
    const NAME: &'static str = protocol_tool::CORE_TRANSFER;
    const TITLE: &'static str = "Transfer ownership";
    const DESCRIPTION: &'static str =
        "Move a memory into a group you manage. Not a share: it leaves its current space.";
    const ACTION_ARG_SPECS: &'static [McpActionArgSpec] = &[McpActionArgSpec {
        action: "transfer_to_owner",
        title: "Transfer to an owner",
        allowed_fields: &["entity", "to_owner"],
        required_fields: &["entity", "to_owner"],
        effect: ToolEffect::Destructive(Replay::NonIdempotent),
        audience: McpToolAudience::Shared,
    }];
    type Args = CoreTransferArgs;
    type Output = TransferOutput;

    fn call(
        ctx: McpToolCtx,
        args: CoreTransferArgs,
    ) -> BoxFuture<'static, Result<TransferOutput, McpToolError>> {
        Box::pin(async move {
            let engine = ctx.require_engine()?;
            match args {
                CoreTransferArgs::TransferToOwner(args) => {
                    let entity = resolve_transferable_entity(&ctx, &args.entity).await?;
                    let to_owner = parse_to_owner(&args.to_owner)?;
                    engine
                        .transfer_to_owner(&ctx.authz, entity, to_owner)
                        .await?;
                    Ok(TransferOutput { ok: true })
                }
            }
        })
    }
}

/// The destination owner from the wire: the bare id, and nothing else.
fn parse_to_owner(raw: &str) -> Result<crate::OwnerId, McpToolError> {
    raw.parse::<crate::OwnerId>()
        .map_err(|err| McpToolError::InvalidInput(format!("to_owner: {err}")))
}

/// Resolve a wire reference to a memory or goal entity. Goal references
/// still resolve here so `Engine::transfer_to_owner` can refuse them with
/// the typed "goals do not transfer" error instead of a parse error.
async fn resolve_transferable_entity(
    ctx: &McpToolCtx,
    raw: &str,
) -> Result<EntityId, McpToolError> {
    let memory_id = ctx.resolve_memory(raw)?;
    let engine = ctx.require_engine()?;
    let memories = engine
        .get_memories(
            &ctx.authz,
            &crate::GetMemoriesReadRequest {
                memory_ids: vec![memory_id],
            },
        )
        .await?;
    if !memories.memories.is_empty() {
        return Ok(EntityId::Memory(memory_id));
    }
    let goal_id = crate::GoalId::new(memory_id.into_inner());
    let mut request = crate::verbs::query::QueryRequest::readable();
    request.entity_kind = Some(crate::EntityKind::Goal);
    request.goal_ids = vec![goal_id];
    request.limit = 1;
    request.include_payloads = false;
    let mut goals = engine.query(&ctx.authz, &request).await?;
    if goals.goals.is_empty() {
        request.goal_ids.clear();
        request.goal_handles = vec![goal_id.into_inner()];
        goals = engine.query(&ctx.authz, &request).await?;
    }
    if goals.goals.is_empty() {
        Ok(EntityId::Memory(memory_id))
    } else {
        Ok(EntityId::Goal(goal_id))
    }
}

#[cfg(test)]
mod tests {
    use crate::mcp::{McpTool, McpToolError, validate_action_args};

    use super::{CoreTransferTool, parse_to_owner};

    #[test]
    fn an_old_kind_prefixed_to_owner_is_refused() {
        let id = uuid::Uuid::now_v7();
        for raw in [format!("group:{id}"), format!("personal:{id}")] {
            assert!(
                matches!(parse_to_owner(&raw), Err(McpToolError::InvalidInput(_))),
                "{raw}"
            );
        }
        assert_eq!(
            parse_to_owner(&id.to_string()).expect("bare id"),
            crate::OwnerId::new(id)
        );
    }

    #[test]
    fn transfer_requires_entity_and_destination() {
        let err = validate_action_args(
            CoreTransferTool::NAME,
            CoreTransferTool::ACTION_ARG_SPECS,
            &serde_json::json!({"action": "transfer_to_owner"}),
        )
        .expect_err("entity and to_owner are required");

        assert!(matches!(err, McpToolError::InvalidInput(_)));

        let err = validate_action_args(
            CoreTransferTool::NAME,
            CoreTransferTool::ACTION_ARG_SPECS,
            &serde_json::json!({
                "action": "transfer_to_owner",
                "entity": "00000000-0000-0000-0000-000000000001"
            }),
        )
        .expect_err("to_owner is required");

        assert!(matches!(err, McpToolError::InvalidInput(_)));
    }

    #[test]
    fn transfer_rejects_extra_fields() {
        let err = validate_action_args(
            CoreTransferTool::NAME,
            CoreTransferTool::ACTION_ARG_SPECS,
            &serde_json::json!({
                "action": "transfer_to_owner",
                "entity": "00000000-0000-0000-0000-000000000001",
                "to_owner": "00000000-0000-0000-0000-000000000002",
                "group": "00000000-0000-0000-0000-000000000002"
            }),
        )
        .expect_err("extra fields are rejected");

        assert!(matches!(err, McpToolError::InvalidInput(_)));
    }
}
