//! Substrate-shipped MCP tools for personality config CRUD. Registered
//! into `FlavorRegistry::default()` so they are available in every
//! composite binary.
//!
//! See docs/superpowers/specs/2026-05-10-personality-mcp-crud-design.md.

pub mod add_wake_entry;
pub mod audit;
pub mod citation_of_fact;
pub mod cleanup_facts;
pub mod clear_fact_retention;
pub mod facts_citing_object;
pub mod fetch_memory;
pub mod payload;
pub mod remove_wake_entry;
pub mod search_memories;
pub mod set_wake_entries;
pub mod update_wake_entry;
pub mod wake_entry_input;

pub mod get_fact_retention;
pub mod get_graph;
pub mod get_memory;
pub mod get_personality;
pub mod instantiate_personality;
pub mod list_edge_types;
pub mod list_personalities;
pub mod list_read_scope;
pub mod list_schemas;
pub mod list_substrate_tools;
pub mod list_wake_entries;
pub mod memory;
pub mod set_fact_retention;
pub mod set_read_scope;
pub mod tombstone_personality;
pub mod walk_memory_lineage;

pub use add_wake_entry::AddWakeEntryTool;
pub use audit::{AuditEmit, emit_personality_config_changed};
pub use citation_of_fact::CitationOfFactTool;
pub use cleanup_facts::CleanupFactsTool;
pub use clear_fact_retention::ClearFactRetentionTool;
pub use facts_citing_object::FactsCitingObjectTool;
pub use fetch_memory::FetchMemoryTool;
pub use get_fact_retention::GetFactRetentionTool;
pub use get_graph::GetGraphTool;
pub use get_memory::GetMemoryTool;
pub use get_personality::GetPersonalityTool;
pub use instantiate_personality::InstantiatePersonalityTool;
pub use list_edge_types::ListEdgeTypesTool;
pub use list_personalities::ListPersonalitiesTool;
pub use list_read_scope::ListReadScopeTool;
pub use list_schemas::ListSchemasTool;
pub use list_substrate_tools::ListSubstrateToolsTool;
pub use list_wake_entries::ListWakeEntriesTool;
pub use memory::{
    DeriveTool, LinkTool, OpenTool, RecordUtteranceTool, RememberTool, SearchGraphTool,
};
pub use payload::{
    PersonalityConfigChangedCaller, PersonalityConfigChangedSubject, PersonalityConfigChangedV1,
    PersonalityConfigChangedVerb,
};
pub use remove_wake_entry::RemoveWakeEntryTool;
pub use search_memories::SearchMemoriesTool;
pub use set_fact_retention::SetFactRetentionTool;
pub use set_read_scope::SetReadScopeTool;
pub use set_wake_entries::SetWakeEntriesTool;
pub use tombstone_personality::TombstonePersonalityTool;
pub use update_wake_entry::{UpdateWakeEntryTool, WakeEntryPatch};
pub use wake_entry_input::WakeEntryDraftInput;
pub use walk_memory_lineage::WalkMemoryLineageTool;

/// Register every substrate-shipped MCP tool into the `FlavorRegistry`.
/// Called from `FlavorRegistry::default()`.
pub(crate) fn register_all(registry: &mut crate::FlavorRegistry) {
    registry.add_substrate_mcp_tool::<ListPersonalitiesTool>();
    registry.add_substrate_mcp_tool::<GetPersonalityTool>();
    registry.add_substrate_mcp_tool::<GetGraphTool>();
    registry.add_substrate_mcp_tool::<GetMemoryTool>();
    registry.add_substrate_mcp_tool::<FetchMemoryTool>();
    registry.add_substrate_mcp_tool::<SearchMemoriesTool>();
    registry.add_substrate_mcp_tool::<FactsCitingObjectTool>();
    registry.add_substrate_mcp_tool::<CitationOfFactTool>();
    registry.add_substrate_mcp_tool::<WalkMemoryLineageTool>();
    registry.add_substrate_mcp_tool::<InstantiatePersonalityTool>();
    registry.add_substrate_mcp_tool::<TombstonePersonalityTool>();
    registry.add_substrate_mcp_tool::<ListWakeEntriesTool>();
    registry.add_substrate_mcp_tool::<SetWakeEntriesTool>();
    registry.add_substrate_mcp_tool::<ListReadScopeTool>();
    registry.add_substrate_mcp_tool::<SetReadScopeTool>();
    registry.add_substrate_mcp_tool::<GetFactRetentionTool>();
    registry.add_substrate_mcp_tool::<SetFactRetentionTool>();
    registry.add_substrate_mcp_tool::<ClearFactRetentionTool>();
    registry.add_substrate_mcp_tool::<CleanupFactsTool>();
    registry.add_substrate_mcp_tool::<AddWakeEntryTool>();
    registry.add_substrate_mcp_tool::<UpdateWakeEntryTool>();
    registry.add_substrate_mcp_tool::<RemoveWakeEntryTool>();
    registry.add_substrate_mcp_tool::<ListSubstrateToolsTool>();
    registry.add_substrate_mcp_tool::<ListSchemasTool>();
    registry.add_substrate_mcp_tool::<ListEdgeTypesTool>();
    registry.add_substrate_mcp_tool::<SearchGraphTool>();
    registry.add_substrate_mcp_tool::<OpenTool>();
    registry.add_substrate_mcp_tool::<RememberTool>();
    registry.add_substrate_mcp_tool::<RecordUtteranceTool>();
    registry.add_substrate_mcp_tool::<DeriveTool>();
    registry.add_substrate_mcp_tool::<LinkTool>();
}
