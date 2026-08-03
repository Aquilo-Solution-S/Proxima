use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::access::EntityId;
use crate::mcp::{CoreActionMeta, McpActionArgSpec, McpTool, McpToolCtx, McpToolError};
use crate::protocol::{action as protocol_action, tool as protocol_tool};

use super::DESTRUCTIVE_NON_IDEMPOTENT;

pub const CORE_PUBLISH_ACTIONS: &[CoreActionMeta] = &[CoreActionMeta {
    tool: CorePublishTool::NAME,
    action: "publish_to_world",
    scope_key: protocol_action::CORE_PUBLISH_TO_WORLD,
    description: "Transfer a memory or goal's owner to World — a deliberate, irreversible publish. World is universally readable and never writable; this is an owner transfer, not a share or ACL flag.",
    produces_schema_ids: &[],
    annotations: DESTRUCTIVE_NON_IDEMPOTENT,
}];

#[derive(Debug, Default)]
pub struct CorePublishTool;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum CorePublishArgs {
    PublishToWorld(PublishToWorldArgs),
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PublishToWorldArgs {
    /// Memory or Goal reference: `F:<uuid>`, `A:<uuid>`, `P:<uuid>`, or
    /// `G:<uuid>`. The current owner (looked up from storage, not
    /// trusted from the caller) must grant the caller write/manage
    /// (`Relation::Admin`) authority.
    pub entity: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct PublishOutput {
    pub ok: bool,
}

impl McpTool for CorePublishTool {
    const NAME: &'static str = protocol_tool::CORE_PUBLISH;
    const DESCRIPTION: &'static str = "Owner-transfer dispatcher — publish_to_world.";
    const ACTION_ARG_SPECS: &'static [McpActionArgSpec] = &[McpActionArgSpec {
        action: "publish_to_world",
        allowed_fields: &["entity"],
        required_fields: &["entity"],
    }];
    type Args = CorePublishArgs;
    type Output = PublishOutput;

    fn call(
        ctx: McpToolCtx,
        args: CorePublishArgs,
    ) -> BoxFuture<'static, Result<PublishOutput, McpToolError>> {
        Box::pin(async move {
            let engine = ctx.require_engine()?;
            match args {
                CorePublishArgs::PublishToWorld(args) => {
                    let entity = resolve_publishable_entity(&ctx, &args.entity)?;
                    engine.publish_to_world(&ctx.authz, entity).await?;
                    Ok(PublishOutput { ok: true })
                }
            }
        })
    }
}

/// Resolve a wire reference to a memory or goal entity — the two row
/// families a publish-to-World transfer may target.
fn resolve_publishable_entity(ctx: &McpToolCtx, raw: &str) -> Result<EntityId, McpToolError> {
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

    use super::CorePublishTool;

    #[test]
    fn publish_requires_entity() {
        let err = validate_action_args(
            CorePublishTool::NAME,
            CorePublishTool::ACTION_ARG_SPECS,
            &serde_json::json!({"action": "publish_to_world"}),
        )
        .expect_err("entity is required");

        assert!(matches!(err, McpToolError::InvalidInput(_)));
    }

    #[test]
    fn publish_rejects_extra_fields() {
        let err = validate_action_args(
            CorePublishTool::NAME,
            CorePublishTool::ACTION_ARG_SPECS,
            &serde_json::json!({
                "action": "publish_to_world",
                "entity": "F:00000000-0000-0000-0000-000000000001",
                "group": "group:00000000-0000-0000-0000-000000000002"
            }),
        )
        .expect_err("extra fields are rejected");

        assert!(matches!(err, McpToolError::InvalidInput(_)));
    }
}
