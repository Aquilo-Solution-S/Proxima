use crate::mcp::{Replay, ToolEffect};
use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::access::EntityId;
use crate::mcp::{
    CoreActionMeta, McpActionArgSpec, McpTool, McpToolAudience, McpToolCtx, McpToolError,
};
use crate::owner::parse_external_key;
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
    /// The memory to move: `F:<uuid>`, `A:<uuid>` or `P:<uuid>`. Goals do not
    /// transfer.
    pub entity: String,
    /// Destination owner key, `group:<uuid>`: a group the caller manages.
    pub to_owner: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct TransferOutput {
    pub ok: bool,
}

impl McpTool for CoreTransferTool {
    const NAME: &'static str = protocol_tool::CORE_TRANSFER;
    const DESCRIPTION: &'static str =
        "Move a memory into a group you manage. Not a share: it leaves its current space.";
    const ACTION_ARG_SPECS: &'static [McpActionArgSpec] = &[McpActionArgSpec {
        action: "transfer_to_owner",
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
                    let entity = resolve_transferable_entity(&ctx, &args.entity)?;
                    let to_owner = parse_external_key(&args.to_owner)
                        .map_err(|err| McpToolError::InvalidInput(format!("to_owner: {err}")))?;
                    engine
                        .transfer_to_owner(&ctx.authz, entity, to_owner)
                        .await?;
                    Ok(TransferOutput { ok: true })
                }
            }
        })
    }
}

/// Resolve a wire reference to a memory or goal entity. Goal references
/// still resolve here so `Engine::transfer_to_owner` can refuse them with
/// the typed "goals do not transfer" error instead of a parse error.
fn resolve_transferable_entity(ctx: &McpToolCtx, raw: &str) -> Result<EntityId, McpToolError> {
    match ctx.resolve_memory(raw) {
        Ok(memory_id) => Ok(EntityId::Memory(memory_id)),
        Err(memory_err) => match ctx.resolve_goal(raw) {
            Ok(goal_id) => Ok(EntityId::Goal(goal_id)),
            Err(_) => Err(memory_err),
        },
    }
}

#[cfg(test)]
mod tests {
    use crate::mcp::{McpTool, McpToolError, validate_action_args};

    use super::CoreTransferTool;

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
                "entity": "F:00000000-0000-0000-0000-000000000001"
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
                "entity": "F:00000000-0000-0000-0000-000000000001",
                "to_owner": "group:00000000-0000-0000-0000-000000000002",
                "group": "group:00000000-0000-0000-0000-000000000002"
            }),
        )
        .expect_err("extra fields are rejected");

        assert!(matches!(err, McpToolError::InvalidInput(_)));
    }
}
