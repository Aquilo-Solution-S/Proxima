use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::access::EntityId;
use crate::mcp::{
    CoreActionMeta, McpActionArgSpec, McpTool, McpToolAudience, McpToolCtx, McpToolError,
};
use crate::owner::parse_external_key;
use crate::protocol::{action as protocol_action, tool as protocol_tool};

use super::DESTRUCTIVE_NON_IDEMPOTENT;

pub const CORE_TRANSFER_ACTIONS: &[CoreActionMeta] = &[CoreActionMeta {
    tool: CoreTransferTool::NAME,
    action: "transfer_to_owner",
    scope_key: protocol_action::CORE_TRANSFER_TO_OWNER,
    description: "Transfer a memory's owner to another owner — a deliberate owner move, not a share or ACL flag. The series leaves the prior owner's view entirely. Requires admin on the source owner and group-manage on the destination. Goals do not transfer.",
    produces_schema_ids: &[],
}];

#[derive(Debug, Default)]
pub struct CoreTransferTool;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum CoreTransferArgs {
    TransferToOwner(TransferToOwnerArgs),
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TransferToOwnerArgs {
    /// Memory reference: `F:<uuid>`, `A:<uuid>`, or `P:<uuid>`. The
    /// current owner (looked up from storage, not trusted from the
    /// caller) must grant the caller write/manage (`Relation::Admin`)
    /// authority. Goal references (`G:<uuid>`) are refused: goals do not
    /// transfer.
    pub entity: String,
    /// Destination owner as an external owner key — the same
    /// `group:<uuid>` spelling the `X-Proxima-Owner` header takes. Must
    /// be a group the caller manages: group-manage is the receiving
    /// side's consent.
    pub to_owner: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct TransferOutput {
    pub ok: bool,
}

impl McpTool for CoreTransferTool {
    const NAME: &'static str = protocol_tool::CORE_TRANSFER;
    const DESCRIPTION: &'static str = "Owner-transfer dispatcher — transfer_to_owner.";
    const ACTION_ARG_SPECS: &'static [McpActionArgSpec] = &[McpActionArgSpec {
        action: "transfer_to_owner",
        allowed_fields: &["entity", "to_owner"],
        required_fields: &["entity", "to_owner"],
        annotations: Some(DESTRUCTIVE_NON_IDEMPOTENT),
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
