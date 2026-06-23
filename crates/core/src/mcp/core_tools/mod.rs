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
pub mod list_edge_types;
pub mod list_events;
pub mod list_personalities;
pub mod list_read_scope;
pub mod list_schemas;
pub mod list_substrate_tools;
pub mod list_wake_entries;
pub mod memory;
pub mod personality;
pub mod set_read_scope;
pub mod tombstone_fact;
pub mod tombstone_personality;
pub mod wake;
pub mod walk_memory_lineage;

pub use add_wake_entry::AddWakeEntryTool;
pub use audit::{AuditEmit, emit_personality_config_changed};
pub use citation_of_fact::{CitationOfEntityHeadTool, CitationOfFactTool};
pub use fact::CoreFactTool;
pub use facts_citing_object::FactsCitingObjectTool;
pub use get_graph::GetGraphTool;
pub use get_memory::GetMemoryTool;
pub use get_personality::GetPersonalityTool;
pub use goal::CoreGoalTool;
pub use instantiate_personality::InstantiatePersonalityTool;
pub use list_edge_types::ListEdgeTypesTool;
pub use list_events::ListEventsTool;
pub use list_personalities::ListPersonalitiesTool;
pub use list_read_scope::ListReadScopeTool;
pub use list_schemas::ListSchemasTool;
pub use list_substrate_tools::ListSubstrateToolsTool;
pub use list_wake_entries::ListWakeEntriesTool;
pub use memory::{DeriveTool, LinkTool, RecordUtteranceTool, RememberTool};
pub use payload::{
    PersonalityConfigChangedCaller, PersonalityConfigChangedSubject, PersonalityConfigChangedV1,
    PersonalityConfigChangedVerb,
};
pub use personality::CorePersonalityTool;
pub use remove_wake_entry::RemoveWakeEntryTool;
pub use search_memories::SearchMemoriesTool;
pub use set_read_scope::SetReadScopeTool;
pub use set_wake_entries::SetWakeEntriesTool;
pub use tombstone_personality::TombstonePersonalityTool;
pub use update_wake_entry::{UpdateWakeEntryTool, WakeEntryPatch};
pub use wake::CoreWakeTool;
pub use wake_entry_input::WakeEntryDraftInput;
pub use walk_memory_lineage::WalkMemoryLineageTool;

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
    registry.add_substrate_mcp_tool::<GetGraphTool>();
    registry.add_substrate_mcp_tool::<GetMemoryTool>();
    registry.add_substrate_mcp_tool::<SearchMemoriesTool>();
    registry.add_substrate_mcp_tool::<WalkMemoryLineageTool>();
    registry.add_substrate_mcp_tool::<ListSubstrateToolsTool>();
    registry.add_substrate_mcp_tool::<ListSchemasTool>();
    registry.add_substrate_mcp_tool::<ListEdgeTypesTool>();
    registry.add_substrate_mcp_tool::<ListEventsTool>();
    registry.add_substrate_mcp_tool::<RememberTool>();
    registry.add_substrate_mcp_tool::<RecordUtteranceTool>();
    registry.add_substrate_mcp_tool::<DeriveTool>();
    registry.add_substrate_mcp_tool::<LinkTool>();
    registry.add_substrate_mcp_tool::<CoreGoalTool>();
    registry.add_substrate_mcp_tool::<CoreWakeTool>();
    registry.add_substrate_mcp_tool::<CorePersonalityTool>();
    registry.add_substrate_mcp_tool::<CoreFactTool>();
}
