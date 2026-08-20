use crate::flavor::FLAVOR_0;
use crate::flavor::contract::ResourceContract;
use crate::protocol::tool as protocol_tool;

use super::core_tools;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, schemars::JsonSchema)]
pub struct McpToolAnnotations {
    pub read_only: Option<bool>,
    pub destructive: Option<bool>,
    pub idempotent: Option<bool>,
    pub open_world: Option<bool>,
}

impl McpToolAnnotations {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            read_only: None,
            destructive: None,
            idempotent: None,
            open_world: None,
        }
    }

    #[must_use]
    pub const fn read_only(mut self, value: bool) -> Self {
        self.read_only = Some(value);
        self
    }

    #[must_use]
    pub const fn destructive(mut self, value: bool) -> Self {
        self.destructive = Some(value);
        self
    }

    #[must_use]
    pub const fn idempotent(mut self, value: bool) -> Self {
        self.idempotent = Some(value);
        self
    }

    #[must_use]
    pub const fn open_world(mut self, value: bool) -> Self {
        self.open_world = Some(value);
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoreActionMeta {
    pub tool: &'static str,
    pub action: &'static str,
    pub scope_key: &'static str,
    pub description: &'static str,
    pub produces_schema_ids: &'static [&'static str],
}

/// Flavor #0's `proxima://` resources, in declaration order.
///
/// The list is the contract's, not a second copy of it. Before the flavor
/// contract landed this module held its own ten-entry `CORE_RESOURCES`
/// table whose `uri_template`s pointed at a third table in
/// `protocol::resource_uri`, so advertising a resource and dispatching it
/// were two acts of remembering.
#[must_use = "iterators are lazy and must be consumed"]
pub fn all_core_resources() -> impl Iterator<Item = &'static ResourceContract> {
    FLAVOR_0.resources.iter()
}

#[must_use = "iterators are lazy and must be consumed"]
pub fn all_core_actions() -> impl Iterator<Item = &'static CoreActionMeta> {
    core_tools::goal::CORE_GOAL_ACTIONS
        .iter()
        .chain(core_tools::fact::CORE_FACT_ACTIONS.iter())
        .chain(core_tools::membership::CORE_MEMBERSHIP_ACTIONS.iter())
        .chain(core_tools::transfer::CORE_TRANSFER_ACTIONS.iter())
        .chain(core_tools::upload::CORE_UPLOAD_ACTIONS.iter())
}

#[must_use]
pub fn core_action_meta(tool: &str, action: &str) -> Option<&'static CoreActionMeta> {
    all_core_actions().find(|meta| meta.tool == tool && meta.action == action)
}

/// Every scope key a `ToolScope::Palette` can be asked about, for a frozen
/// registry.
///
/// The scope gate is flat string membership ([`crate::ToolScope::allows`]), and
/// `read_resource` funnels through that same gate with the resource's scope key
/// standing in for a tool name — so a palette assembled from tools alone denies
/// every `proxima://` read rather than merely not advertising it. Resource keys
/// are therefore part of the canonical enumeration, not an optional extra a
/// caller remembers to append.
///
/// Flat tools contribute their id; dispatchers contribute one `tool:action`
/// leaf per action, because the gate authorizes them at that granularity.
#[must_use]
pub fn canonical_scope_keys(registry: &crate::FlavorRegistryFrozen) -> Vec<String> {
    canonical_scope_keys_excluding(registry, &[])
}

/// [`canonical_scope_keys`] minus every id in `exclude`.
///
/// Exclusion is applied to the *tool name* before its actions are expanded, so
/// naming a dispatcher removes all of its leaves in one step and an action
/// added to it later cannot silently re-enter the palette. Resource keys are
/// excluded by their exact scope key.
#[must_use]
pub fn canonical_scope_keys_excluding(
    registry: &crate::FlavorRegistryFrozen,
    exclude: &[&str],
) -> Vec<String> {
    let excluded: std::collections::HashSet<&str> = exclude.iter().copied().collect();
    let mut keys = Vec::new();
    for tool in registry.list_mcp_tools() {
        if excluded.contains(tool.name) {
            continue;
        }
        if tool.action_arg_specs.is_empty() {
            keys.push(tool.name.to_string());
        } else {
            keys.extend(
                tool.action_arg_specs
                    .iter()
                    .map(|action| format!("{}:{}", tool.name, action.action)),
            );
        }
    }
    keys.extend(
        all_core_resources()
            .map(|resource| resource.scope_key)
            .filter(|scope_key| !excluded.contains(scope_key))
            .map(ToString::to_string),
    );
    keys.sort();
    keys.dedup();
    keys
}

/// MCP behavior hints for substrate tools, keyed by registered tool name.
#[must_use]
pub fn core_tool_annotations(canonical_name: &str) -> Option<McpToolAnnotations> {
    let base = McpToolAnnotations::new().open_world(false);
    let annotations = match canonical_name {
        protocol_tool::CORE_SEARCH_MEMORIES
        | protocol_tool::CORE_RECALL
        | protocol_tool::CORE_THINK
        | protocol_tool::CORE_MEMORY_SPACES => base.read_only(true),

        // Idempotent by content: the interpretation's memory id is
        // folded from the claim, so re-asserting it lands on one memory.
        protocol_tool::CORE_DERIVE | protocol_tool::CORE_INTERPRET => {
            base.read_only(false).destructive(false).idempotent(true)
        }

        protocol_tool::CORE_REMEMBER
        | protocol_tool::CORE_RECORD_UTTERANCE
        | protocol_tool::CORE_EPISODE_COMMIT => {
            base.read_only(false).destructive(false).idempotent(false)
        }

        protocol_tool::CORE_FORGET => base.read_only(false).destructive(true).idempotent(false),

        _ => return None,
    };
    Some(annotations)
}
