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
        uri_template: "proxima://schemas{?kind}",
        name: "proxima-schemas",
        title: "Proxima Schemas",
        scope_key: "resource:schemas",
        description: "Registered core and flavor schema catalog, optionally filtered by payload kind.",
        is_template: false,
    },
    CoreResourceMeta {
        uri_template: "proxima://edge-types",
        name: "proxima-edge-types",
        title: "Proxima Edge Types",
        scope_key: "resource:edge-types",
        description: "Registered relation descriptors and relation classes.",
        is_template: false,
    },
    CoreResourceMeta {
        uri_template: "proxima://tools",
        name: "proxima-tools",
        title: "Proxima Tools",
        scope_key: "resource:tools",
        description: "Registered substrate and flavor MCP tool catalog visible to the caller.",
        is_template: false,
    },
    CoreResourceMeta {
        uri_template: "proxima://graph{?include_tombstoned}",
        name: "proxima-graph",
        title: "Proxima Graph",
        scope_key: "resource:graph",
        description: "Owner-scoped memory graph plus schema, edge-type, and tool catalogs.",
        is_template: false,
    },
    CoreResourceMeta {
        uri_template: "proxima://memory/{id}{?expand_neighbors}",
        name: "proxima-memory",
        title: "Proxima Memory",
        scope_key: "resource:memory",
        description: "Owner-scoped memory by prefixed id, raw id, or handle.",
        is_template: true,
    },
    CoreResourceMeta {
        uri_template: "proxima://memory/{id}/lineage{?direction,depth,limit}",
        name: "proxima-memory-lineage",
        title: "Proxima Memory Lineage",
        scope_key: "resource:memory-lineage",
        description: "Owner-scoped Provenance/Supersession lineage from a memory id or handle.",
        is_template: true,
    },
    CoreResourceMeta {
        uri_template: "proxima://change-events{?since,limit}",
        name: "proxima-change-events",
        title: "Proxima Change Events",
        scope_key: "resource:change-events",
        description: "Owner-scoped change-event pull log.",
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
        "core_search_memories" | "core_memory_spaces" => base.read_only(true),

        "core_derive" => base.read_only(false).destructive(false).idempotent(true),

        "core_remember" | "core_record_utterance" | "core_goal" | "core_link" => {
            base.read_only(false).destructive(false).idempotent(false)
        }

        "core_membership" => base.read_only(false).destructive(true).idempotent(false),

        "core_fact" => base.read_only(false).destructive(true).idempotent(true),

        _ => return None,
    };
    Some(annotations)
}
