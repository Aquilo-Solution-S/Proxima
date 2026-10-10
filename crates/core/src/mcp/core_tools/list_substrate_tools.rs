//! `core/list_substrate_tools` — dispatchable substrate and flavor MCP tools.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::AccessKind;
use crate::mcp::{
    McpToolAnnotations, McpToolCtx, McpToolDescriptor, McpToolError, McpToolOrigin,
    ToolDescriptorView, core_action_meta,
};

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ListSubstrateToolsArgs {}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SubstrateToolItem {
    pub tool_id: String,
    pub source: String,
    pub description: String,
    pub actions: Vec<SubstrateToolActionItem>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SubstrateToolActionItem {
    pub action: String,
    pub scope_key: String,
    pub description: String,
    pub produces_schema_ids: Vec<String>,
    pub annotations: McpToolAnnotations,
    pub argument_schema: serde_json::Value,
    pub allowed_fields: Vec<String>,
    pub required_fields: Vec<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ListSubstrateToolsOutput {
    pub tools: Vec<SubstrateToolItem>,
}

pub(super) fn substrate_tool_source(desc: &McpToolDescriptor) -> String {
    match &desc.origin {
        McpToolOrigin::Substrate => "substrate".into(),
        McpToolOrigin::Flavor(id) => format!("flavor:{id}"),
    }
}

#[allow(clippy::unused_async)]
/// # Errors
///
/// This projection is infallible; the `Result` shape matches the tool
/// dispatch contract.
pub async fn list_substrate_tools(
    ctx: McpToolCtx,
    _args: ListSubstrateToolsArgs,
) -> Result<ListSubstrateToolsOutput, McpToolError> {
    let mut tools = Vec::new();
    for desc in ctx.registry.list_mcp_tools() {
        if !tool_visible(&ctx, desc) {
            continue;
        }
        tools.push(SubstrateToolItem {
            tool_id: desc.name.to_string(),
            source: substrate_tool_source(desc),
            description: desc.description.to_string(),
            actions: substrate_tool_actions(&ctx, desc),
        });
    }
    Ok(ListSubstrateToolsOutput { tools })
}

/// The catalog's per-action rows for one tool.
///
/// Driven by the descriptor's `action_arg_specs`, which is THE enumeration
/// of a dispatcher's actions. `core_action_meta` is decoration a substrate
/// action gets and a flavor action does not — scope key and produced schema
/// ids — so it is looked up per already-known action rather than iterated.
/// Every action's prose is its enum-variant doc comment. Driving the loop from `all_core_actions()`
/// meant a flavor dispatcher listed no actions at all in `proxima://tools`:
/// present in the catalog, described as if it were flat.
pub(super) fn substrate_tool_actions(
    ctx: &McpToolCtx,
    desc: &McpToolDescriptor,
) -> Vec<SubstrateToolActionItem> {
    desc.action_arg_specs
        .iter()
        .filter(|spec| action_visible(ctx, desc, spec.action))
        .map(|spec| {
            let meta = core_action_meta(desc.name, spec.action);
            SubstrateToolActionItem {
                action: spec.action.to_string(),
                scope_key: meta.map_or_else(
                    || format!("{}:{}", desc.name, spec.action),
                    |meta| meta.scope_key.to_string(),
                ),
                description: desc
                    .action_description(spec.action)
                    .unwrap_or_default()
                    .to_string(),
                produces_schema_ids: meta
                    .map(|meta| meta.produces_schema_ids)
                    .unwrap_or_default()
                    .iter()
                    .map(|id| (*id).to_string())
                    .collect(),
                annotations: McpToolAnnotations::registered(spec.effect),
                argument_schema: desc
                    .action_argument_schema(spec.action)
                    .cloned()
                    .expect("try_freeze matched every spec to a derived action schema"),
                allowed_fields: spec
                    .allowed_fields
                    .iter()
                    .map(|field| (*field).to_string())
                    .collect(),
                required_fields: spec
                    .required_fields
                    .iter()
                    .map(|field| (*field).to_string())
                    .collect(),
            }
        })
        .collect()
}

/// Palette, owner role, then every request behavior's `visible`, which only
/// narrows.
fn action_visible(ctx: &McpToolCtx, tool: &McpToolDescriptor, action: &str) -> bool {
    tool.action_advertised_by(ctx.authz.tool_scope(), action)
        && owner_role_permits(ctx, tool.action_is_read_only(action))
        && ctx.behaviors_show(&ToolDescriptorView::registry(tool, Some(action)))
}

/// A dispatcher is visible when at least one of its actions is, whichever
/// vocabulary keys them: an argv dispatcher advertises `tool:action` leaves
/// exactly like an `action`-tagged one, so classifying it whole would hide a
/// mostly-read CLI tool from a read-capable-only owner because one of its
/// commands writes. Same rule the MCP server's `tools/list` applies.
fn tool_visible(ctx: &McpToolCtx, tool: &McpToolDescriptor) -> bool {
    if !tool.advertised_by(ctx.authz.tool_scope()) {
        return false;
    }
    if !tool.action_arg_specs.is_empty() {
        tool.action_arg_specs
            .iter()
            .any(|spec| action_visible(ctx, tool, spec.action))
    } else if !tool.argv_action_specs.is_empty() {
        tool.argv_action_specs
            .iter()
            .any(|spec| action_visible(ctx, tool, spec.action))
    } else {
        owner_role_permits(ctx, tool.is_read_only())
            && ctx.behaviors_show(&ToolDescriptorView::registry(tool, None))
    }
}

fn owner_role_permits(ctx: &McpToolCtx, read_only: bool) -> bool {
    if read_only {
        ctx.authz.may_read(&ctx.owner, AccessKind::Fact)
    } else {
        ctx.authz.may_write(&ctx.owner, AccessKind::Fact)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::McpAuthorContext;
    use crate::protocol::{action as protocol_action, tool as protocol_tool};
    use crate::{AuthPath, AuthzContext, FlavorRegistry, FlavorServices, OwnerRef, UserId};
    use std::sync::Arc;

    #[tokio::test]
    async fn tool_catalog_exposes_action_level_metadata() {
        let owner = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));
        let ctx = McpToolCtx {
            owner,
            authz: AuthzContext::single_owner(&owner, AuthPath::HostBearer),
            registry: Arc::new(FlavorRegistry::new().freeze_or_panic_for_tests()),
            author: McpAuthorContext {
                model_id: "test".into(),
                trusted_model_id: None,
                client_name: "test".into(),
                client_version: "0".into(),
                caller_self_perspective: None,
            },
            caller_self_perspective: None,
            services: FlavorServices::default(),
            engine: None,
        };

        let registry = ctx.registry.clone();
        let output = list_substrate_tools(ctx, ListSubstrateToolsArgs::default())
            .await
            .expect("catalog lists");
        let core_goal = output
            .tools
            .iter()
            .find(|tool| tool.tool_id == protocol_tool::CORE_GOAL)
            .expect("core_goal catalog item");
        let decompose = core_goal
            .actions
            .iter()
            .find(|action| action.action == "decompose")
            .expect("decompose action metadata");
        assert_eq!(decompose.scope_key, protocol_action::CORE_GOAL_DECOMPOSE);
        assert!(decompose.description.contains("child Goals"));
        assert_eq!(decompose.annotations.idempotent, Some(true));
        assert!(
            decompose
                .required_fields
                .contains(&"idempotency_key".to_string())
        );
        assert!(decompose.allowed_fields.contains(&"children".to_string()));
        let descriptor = registry
            .list_mcp_tools()
            .iter()
            .find(|tool| tool.name == protocol_tool::CORE_GOAL)
            .expect("core_goal descriptor");
        assert_eq!(
            decompose.argument_schema,
            *descriptor
                .action_argument_schema("decompose")
                .expect("decompose argument schema")
        );
    }
}
