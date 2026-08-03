//! `core/list_substrate_tools` — dispatchable substrate and flavor MCP tools.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::mcp::{
    McpToolAnnotations, McpToolCtx, McpToolDescriptor, McpToolError, McpToolOrigin,
    core_action_meta,
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
/// This projection is infallible today; the `Result` shape matches the tool
/// dispatch contract.
pub async fn list_substrate_tools(
    ctx: McpToolCtx,
    _args: ListSubstrateToolsArgs,
) -> Result<ListSubstrateToolsOutput, McpToolError> {
    let mut tools = Vec::new();
    for desc in ctx.registry.list_mcp_tools() {
        if !ctx.authz.tool_scope().allows_group_advertisement(desc.name) {
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
/// action gets and a flavor action does not — scope key, prose, produced
/// schema ids, per-action annotations — so it is looked up per already-known
/// action rather than iterated. Driving the loop from `all_core_actions()`
/// meant a flavor dispatcher listed no actions at all in `proxima://tools`:
/// present in the catalog, described as if it were flat.
///
/// A flavor action's `description` is empty and its annotations come from
/// the tool, both known gaps with a stated fix direction in docs/12.
pub(super) fn substrate_tool_actions(
    ctx: &McpToolCtx,
    desc: &McpToolDescriptor,
) -> Vec<SubstrateToolActionItem> {
    desc.action_arg_specs
        .iter()
        .filter(|spec| action_visible(ctx, desc.name, spec.action))
        .map(|spec| {
            let meta = core_action_meta(desc.name, spec.action);
            SubstrateToolActionItem {
                action: spec.action.to_string(),
                scope_key: meta.map_or_else(
                    || format!("{}:{}", desc.name, spec.action),
                    |meta| meta.scope_key.to_string(),
                ),
                description: meta
                    .map(|meta| meta.description)
                    .unwrap_or_default()
                    .to_string(),
                produces_schema_ids: meta
                    .map(|meta| meta.produces_schema_ids)
                    .unwrap_or_default()
                    .iter()
                    .map(|id| (*id).to_string())
                    .collect(),
                // The same fallback REST and the OpenAPI generator apply: a
                // flavor dispatcher has no per-action override, so the tool's
                // own annotations decide.
                annotations: meta.map_or_else(
                    || desc.resolved_annotations().unwrap_or_default(),
                    |meta| meta.annotations,
                ),
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

fn action_visible(ctx: &McpToolCtx, tool: &str, action: &str) -> bool {
    scope_permits_action(ctx.authz.tool_scope(), tool, action)
}

/// Whether `scope` advertises `action` of dispatcher `tool`: either the whole
/// tool is in the palette, or its specific `tool:action` leaf is. Shared by the
/// substrate tool catalog and the MCP server's scope-projected `tools/list`.
#[must_use]
pub fn scope_permits_action(scope: &crate::authz::ToolScope, tool: &str, action: &str) -> bool {
    match scope {
        crate::authz::ToolScope::All => true,
        crate::authz::ToolScope::Palette(allowed) => {
            allowed.iter().any(|entry| entry == tool) || scope.allows_action(tool, action)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::{McpAuthorContext, McpToolExtensions};
    use crate::protocol::{action as protocol_action, tool as protocol_tool};
    use crate::{AuthPath, AuthzContext, FlavorRegistry, OwnerRef, UserId};
    use std::sync::Arc;

    #[test]
    fn default_substrate_tools_classify_as_substrate() {
        let registry = FlavorRegistry::new().freeze_or_panic_for_tests();

        for desc in registry.list_mcp_tools() {
            assert!(
                matches!(desc.origin, McpToolOrigin::Substrate),
                "default tool {} must be substrate-origin",
                desc.name
            );
            assert_eq!(substrate_tool_source(desc), "substrate");
        }
    }

    #[tokio::test]
    async fn tool_catalog_exposes_action_level_metadata() {
        let owner = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));
        let ctx = McpToolCtx {
            owner,
            authz: AuthzContext::single_owner(&owner, AuthPath::HostBearer),
            registry: Arc::new(FlavorRegistry::new().freeze_or_panic_for_tests()),
            author: McpAuthorContext {
                model_id: "test".into(),
                client_name: "test".into(),
                client_version: "0".into(),
                caller_self_perspective: None,
            },
            caller_self_perspective: None,
            extensions: McpToolExtensions::default(),
            engine: None,
        };

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
    }
}
