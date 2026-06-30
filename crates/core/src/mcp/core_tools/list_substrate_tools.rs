//! `core/list_substrate_tools` — dispatchable substrate and flavor MCP tools.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::mcp::{
    McpActionArgSpec, McpToolAnnotations, McpToolCtx, McpToolDescriptor, McpToolError,
    McpToolOrigin, all_core_actions,
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

pub(super) fn substrate_tool_actions(
    ctx: &McpToolCtx,
    desc: &McpToolDescriptor,
) -> Vec<SubstrateToolActionItem> {
    all_core_actions()
        .filter(|meta| meta.tool == desc.name)
        .filter(|meta| action_visible(ctx, meta.tool, meta.action))
        .map(|meta| {
            let spec = action_spec(desc.action_arg_specs, meta.action);
            SubstrateToolActionItem {
                action: meta.action.to_string(),
                scope_key: meta.scope_key.to_string(),
                description: meta.description.to_string(),
                produces_schema_ids: meta
                    .produces_schema_ids
                    .iter()
                    .map(|id| (*id).to_string())
                    .collect(),
                annotations: meta.annotations,
                allowed_fields: spec
                    .map(|spec| {
                        spec.allowed_fields
                            .iter()
                            .map(|field| (*field).to_string())
                            .collect()
                    })
                    .unwrap_or_default(),
                required_fields: spec
                    .map(|spec| {
                        spec.required_fields
                            .iter()
                            .map(|field| (*field).to_string())
                            .collect()
                    })
                    .unwrap_or_default(),
            }
        })
        .collect()
}

fn action_spec(
    specs: &'static [McpActionArgSpec],
    action: &str,
) -> Option<&'static McpActionArgSpec> {
    specs.iter().find(|spec| spec.action == action)
}

fn action_visible(ctx: &McpToolCtx, tool: &str, action: &str) -> bool {
    match ctx.authz.tool_scope() {
        crate::authz::ToolScope::All => true,
        crate::authz::ToolScope::Palette(allowed) => {
            allowed.iter().any(|entry| entry == tool)
                || ctx.authz.tool_scope().allows_action(tool, action)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::{McpAuthorContext, McpToolExtensions, OutputMode};
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
            authz: AuthzContext::single_owner(&owner, AuthPath::System),
            handles: None,
            mode: OutputMode::PrefixedIds,
            registry: Arc::new(FlavorRegistry::new().freeze_or_panic_for_tests()),
            author: McpAuthorContext {
                model_id: "test".into(),
                client_name: "test".into(),
                client_version: "0".into(),
                caller_self_perspective: None,
            },
            caller_self_perspective: None,
            master_token_id: None,
            extensions: McpToolExtensions::default(),
            engine: None,
        };

        let output = list_substrate_tools(ctx, ListSubstrateToolsArgs::default())
            .await
            .expect("catalog lists");
        let core_goal = output
            .tools
            .iter()
            .find(|tool| tool.tool_id == "core_goal")
            .expect("core_goal catalog item");
        let decompose = core_goal
            .actions
            .iter()
            .find(|action| action.action == "decompose")
            .expect("decompose action metadata");
        assert_eq!(decompose.scope_key, "core_goal:decompose");
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
