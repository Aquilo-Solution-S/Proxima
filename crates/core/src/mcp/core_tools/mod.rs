//! Substrate-shipped MCP tools for personality config CRUD. Registered
//! into `FlavorRegistry::default()` so they are available in every
//! composite binary.
//!
//! See docs/superpowers/specs/2026-05-10-personality-mcp-crud-design.md.

pub mod add_wake_entry;
pub mod audit;
pub mod citation_of_fact;
pub mod fact;
pub mod facts_citing_object;
pub mod payload;
pub mod remove_wake_entry;
pub mod search_memories;
pub mod set_wake_entries;
pub mod update_wake_entry;
pub mod wake_entry_input;

pub mod get_graph;
pub mod get_memory;
pub mod get_personality;
pub mod goal;
pub mod instantiate_personality;
pub mod list_change_events;
pub mod list_edge_types;
pub mod list_personalities;
pub mod list_schemas;
pub mod list_substrate_tools;
pub mod list_wake_entries;
pub mod membership;
pub mod memory;
pub mod memory_spaces;
pub mod personality;
pub mod tombstone_fact;
pub mod tombstone_personality;
pub mod wake;
pub mod walk_memory_lineage;

pub use audit::AuditEmit;
pub use fact::CoreFactTool;
pub use goal::CoreGoalTool;
pub use membership::CoreMembershipTool;
pub use memory::{DeriveTool, LinkTool, RecordUtteranceTool, RememberTool};
pub use memory_spaces::MemorySpacesTool;
pub use payload::{
    PersonalityConfigChangedCaller, PersonalityConfigChangedSubject, PersonalityConfigChangedV1,
    PersonalityConfigChangedVerb,
};
pub use personality::CorePersonalityTool;
pub use search_memories::SearchMemoriesTool;
pub use update_wake_entry::WakeEntryPatch;
pub use wake::CoreWakeTool;
pub use wake_entry_input::WakeEntryDraftInput;

use crate::mcp::McpToolAnnotations;

const READ_ONLY: McpToolAnnotations = McpToolAnnotations::new().read_only(true).open_world(false);
const WRITE_NON_IDEMPOTENT: McpToolAnnotations = McpToolAnnotations::new()
    .read_only(false)
    .destructive(false)
    .idempotent(false)
    .open_world(false);
const WRITE_IDEMPOTENT: McpToolAnnotations = McpToolAnnotations::new()
    .read_only(false)
    .destructive(false)
    .idempotent(true)
    .open_world(false);
const DESTRUCTIVE_NON_IDEMPOTENT: McpToolAnnotations = McpToolAnnotations::new()
    .read_only(false)
    .destructive(true)
    .idempotent(false)
    .open_world(false);
const DESTRUCTIVE_IDEMPOTENT: McpToolAnnotations = McpToolAnnotations::new()
    .read_only(false)
    .destructive(true)
    .idempotent(true)
    .open_world(false);

/// Register every substrate-shipped MCP tool into the `FlavorRegistry`.
/// Called from `FlavorRegistry::default()`.
pub(crate) fn register_all(registry: &mut crate::FlavorRegistry) {
    registry.add_substrate_mcp_tool::<SearchMemoriesTool>();
    registry.add_substrate_mcp_tool::<MemorySpacesTool>();
    registry.add_substrate_mcp_tool::<RememberTool>();
    registry.add_substrate_mcp_tool::<RecordUtteranceTool>();
    registry.add_substrate_mcp_tool::<DeriveTool>();
    registry.add_substrate_mcp_tool::<LinkTool>();
    registry.add_substrate_mcp_tool::<CoreGoalTool>();
    registry.add_substrate_mcp_tool::<CoreWakeTool>();
    registry.add_substrate_mcp_tool::<CorePersonalityTool>();
    registry.add_substrate_mcp_tool::<CoreFactTool>();
    registry.add_substrate_mcp_tool::<CoreMembershipTool>();
}
