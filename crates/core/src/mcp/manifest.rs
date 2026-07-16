use crate::protocol::tool as protocol_tool;
use crate::protocol::{resource as protocol_resource, resource_uri as protocol_resource_uri};

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
    pub annotations: McpToolAnnotations,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoreResourceMeta {
    pub uri_template: &'static str,
    pub name: &'static str,
    pub title: &'static str,
    pub scope_key: &'static str,
    pub description: &'static str,
    pub is_template: bool,
}

pub const CORE_RESOURCES: &[CoreResourceMeta] = &[
    CoreResourceMeta {
        uri_template: protocol_resource_uri::SCHEMAS,
        name: "proxima-schemas",
        title: "Proxima Schemas",
        scope_key: protocol_resource::SCHEMAS,
        description: "Registered core and flavor schema catalog, optionally filtered by payload kind.",
        is_template: false,
    },
    CoreResourceMeta {
        uri_template: protocol_resource_uri::EDGE_TYPES,
        name: "proxima-edge-types",
        title: "Proxima Edge Types",
        scope_key: protocol_resource::EDGE_TYPES,
        description: "Registered relation descriptors and relation classes.",
        is_template: false,
    },
    CoreResourceMeta {
        uri_template: protocol_resource_uri::TOOLS,
        name: "proxima-tools",
        title: "Proxima Tools",
        scope_key: protocol_resource::TOOLS,
        description: "Registered substrate and flavor MCP tool catalog visible to the caller.",
        is_template: false,
    },
    CoreResourceMeta {
        uri_template: protocol_resource_uri::GRAPH,
        name: "proxima-graph",
        title: "Proxima Graph",
        scope_key: protocol_resource::GRAPH,
        description: "Owner-scoped memory graph plus schema, edge-type, and tool catalogs.",
        is_template: false,
    },
    CoreResourceMeta {
        uri_template: protocol_resource_uri::MEMORY,
        name: "proxima-memory",
        title: "Proxima Memory",
        scope_key: protocol_resource::MEMORY,
        description: "Owner-scoped memory by prefixed id (`F:`/`A:`/`P:`).",
        is_template: true,
    },
    CoreResourceMeta {
        uri_template: protocol_resource_uri::MEMORY_LINEAGE,
        name: "proxima-memory-lineage",
        title: "Proxima Memory Lineage",
        scope_key: protocol_resource::MEMORY_LINEAGE,
        description: "Owner-scoped Provenance/Supersession lineage from a prefixed memory id.",
        is_template: true,
    },
    CoreResourceMeta {
        uri_template: protocol_resource_uri::CHANGE_EVENTS,
        name: "proxima-change-events",
        title: "Proxima Change Events",
        scope_key: protocol_resource::CHANGE_EVENTS,
        description: "Owner-scoped change-event pull log.",
        is_template: true,
    },
    CoreResourceMeta {
        uri_template: protocol_resource_uri::WAKE_CANDIDATES,
        name: "proxima-wake-candidates",
        title: "Proxima Wake Candidates",
        scope_key: protocol_resource::WAKE_CANDIDATES,
        description: "Armed Active Goals admitted for wake planning by a trigger Fact.",
        is_template: true,
    },
    CoreResourceMeta {
        uri_template: protocol_resource_uri::GOALS,
        name: "proxima-goals",
        title: "Proxima Goals",
        scope_key: protocol_resource::GOALS,
        description: "Owner-scoped goal listing with state filter, keyset cursor, and wake-config read-back.",
        is_template: true,
    },
    CoreResourceMeta {
        uri_template: protocol_resource_uri::GOAL,
        name: "proxima-goal",
        title: "Proxima Goal",
        scope_key: protocol_resource::GOAL,
        description: "Single-goal read by G:<uuid> reference, including stored wake configuration.",
        is_template: true,
    },
];

#[must_use = "iterators are lazy and must be consumed"]
pub fn all_core_resources() -> impl Iterator<Item = &'static CoreResourceMeta> {
    CORE_RESOURCES.iter()
}

#[must_use = "iterators are lazy and must be consumed"]
pub fn all_core_actions() -> impl Iterator<Item = &'static CoreActionMeta> {
    core_tools::goal::CORE_GOAL_ACTIONS
        .iter()
        .chain(core_tools::fact::CORE_FACT_ACTIONS.iter())
        .chain(core_tools::membership::CORE_MEMBERSHIP_ACTIONS.iter())
        .chain(core_tools::publish::CORE_PUBLISH_ACTIONS.iter())
}

#[must_use]
pub fn core_action_meta(tool: &str, action: &str) -> Option<&'static CoreActionMeta> {
    all_core_actions().find(|meta| meta.tool == tool && meta.action == action)
}

#[must_use]
pub fn core_tool_has_actions(tool: &str) -> bool {
    all_core_actions().any(|meta| meta.tool == tool)
}

/// MCP behavior hints for substrate tools, keyed by registered tool name.
#[must_use]
pub fn core_tool_annotations(canonical_name: &str) -> Option<McpToolAnnotations> {
    let base = McpToolAnnotations::new().open_world(false);
    let annotations = match canonical_name {
        protocol_tool::CORE_SEARCH_MEMORIES | protocol_tool::CORE_MEMORY_SPACES => {
            base.read_only(true)
        }

        protocol_tool::CORE_DERIVE => base.read_only(false).destructive(false).idempotent(true),

        protocol_tool::CORE_REMEMBER
        | protocol_tool::CORE_RECORD_UTTERANCE
        | protocol_tool::CORE_GOAL
        | protocol_tool::CORE_LINK => base.read_only(false).destructive(false).idempotent(false),

        protocol_tool::CORE_MEMBERSHIP | protocol_tool::CORE_PUBLISH => {
            base.read_only(false).destructive(true).idempotent(false)
        }

        protocol_tool::CORE_FACT => base.read_only(true).destructive(false).idempotent(true),

        _ => return None,
    };
    Some(annotations)
}
