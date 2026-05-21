//! Substrate-shipped MCP tools for personality config CRUD. Registered
//! into `FlavorRegistry::default()` so they are available in every
//! composite binary.
//!
//! See docs/superpowers/specs/2026-05-10-personality-mcp-crud-design.md.

pub mod add_wake_entry;
pub mod approval;
pub mod audit;
pub mod intervention;
pub mod payload;
pub mod remove_wake_entry;
pub mod replay_wake_events;
pub mod set_wake_entries;
pub mod update_wake_entry;
pub mod wake_entry_input;

pub mod bind_inference_tier;
pub mod chat;
pub mod embedding_models;
pub mod get_graph;
pub mod get_personality;
pub mod instantiate_personality;
pub mod list_edge_types;
pub mod list_inference_targets;
pub mod list_inference_tier_bindings;
pub mod list_personalities;
pub mod list_read_scope;
pub mod list_schemas;
pub mod list_substrate_tools;
pub mod list_wake_entries;
pub mod list_wake_invocations;
pub mod list_workspace_tools;
pub mod register_inference_target;
pub mod remove_inference_target;
pub mod set_read_scope;
pub mod tombstone_personality;

pub use add_wake_entry::AddWakeEntryTool;
pub use approval::{EmitApprovalPolicyTool, EmitApprovalVoteTool, TryEmitApprovalDecisionTool};
pub use audit::{AuditEmit, emit_personality_config_changed};
pub use bind_inference_tier::BindInferenceTierTool;
pub use chat::{
    CompactChatThreadTool, EmitChatMessageTool, EmitChatReplyTool, EndChatTool, GetChatThreadTool,
    ListChatTargetsTool, RequestEndChatTool, StartChatTool,
};
pub use embedding_models::{
    ClearEmbeddingActiveTool, DeleteEmbeddingModelTool, GetEmbeddingActiveTool,
    ListEmbeddingModelsTool, RegisterEmbeddingModelTool, SetEmbeddingActiveTool,
};
pub use get_graph::GetGraphTool;
pub use get_personality::GetPersonalityTool;
pub use instantiate_personality::InstantiatePersonalityTool;
pub use intervention::EmitInterventionDecisionTool;
pub use list_edge_types::ListEdgeTypesTool;
pub use list_inference_targets::ListInferenceTargetsTool;
pub use list_inference_tier_bindings::ListInferenceTierBindingsTool;
pub use list_personalities::ListPersonalitiesTool;
pub use list_read_scope::ListReadScopeTool;
pub use list_schemas::ListSchemasTool;
pub use list_substrate_tools::ListSubstrateToolsTool;
pub use list_wake_entries::ListWakeEntriesTool;
pub use list_wake_invocations::{ListWakeInvocationsArgs, ListWakeInvocationsTool};
pub use list_workspace_tools::ListWorkspaceToolsTool;
pub use payload::{
    PersonalityConfigChangedCaller, PersonalityConfigChangedSubject, PersonalityConfigChangedV1,
    PersonalityConfigChangedVerb,
};
pub use register_inference_target::RegisterInferenceTargetTool;
pub use remove_inference_target::RemoveInferenceTargetTool;
pub use remove_wake_entry::RemoveWakeEntryTool;
pub use replay_wake_events::ReplayWakeEventsTool;
pub use set_read_scope::SetReadScopeTool;
pub use set_wake_entries::SetWakeEntriesTool;
pub use tombstone_personality::TombstonePersonalityTool;
pub use update_wake_entry::{UpdateWakeEntryTool, WakeEntryPatch};
pub use wake_entry_input::WakeEntryDraftInput;

/// Register every substrate-shipped MCP tool into the FlavorRegistry.
/// Called from `FlavorRegistry::default()`.
pub(crate) fn register_all(registry: &mut crate::FlavorRegistry) {
    registry.add_substrate_mcp_tool::<ListPersonalitiesTool>();
    registry.add_substrate_mcp_tool::<GetPersonalityTool>();
    registry.add_substrate_mcp_tool::<GetGraphTool>();
    registry.add_substrate_mcp_tool::<InstantiatePersonalityTool>();
    registry.add_substrate_mcp_tool::<TombstonePersonalityTool>();
    registry.add_substrate_mcp_tool::<ListWakeEntriesTool>();
    registry.add_substrate_mcp_tool::<ListWakeInvocationsTool>();
    registry.add_substrate_mcp_tool::<SetWakeEntriesTool>();
    registry.add_substrate_mcp_tool::<ListReadScopeTool>();
    registry.add_substrate_mcp_tool::<SetReadScopeTool>();
    registry.add_substrate_mcp_tool::<AddWakeEntryTool>();
    registry.add_substrate_mcp_tool::<UpdateWakeEntryTool>();
    registry.add_substrate_mcp_tool::<RemoveWakeEntryTool>();
    registry.add_substrate_mcp_tool::<ReplayWakeEventsTool>();
    registry.add_substrate_mcp_tool::<ListInferenceTargetsTool>();
    registry.add_substrate_mcp_tool::<ListInferenceTierBindingsTool>();
    registry.add_substrate_mcp_tool::<RegisterInferenceTargetTool>();
    registry.add_substrate_mcp_tool::<RemoveInferenceTargetTool>();
    registry.add_substrate_mcp_tool::<BindInferenceTierTool>();
    registry.add_substrate_mcp_tool::<ListEmbeddingModelsTool>();
    registry.add_substrate_mcp_tool::<GetEmbeddingActiveTool>();
    registry.add_substrate_mcp_tool::<RegisterEmbeddingModelTool>();
    registry.add_substrate_mcp_tool::<DeleteEmbeddingModelTool>();
    registry.add_substrate_mcp_tool::<SetEmbeddingActiveTool>();
    registry.add_substrate_mcp_tool::<ClearEmbeddingActiveTool>();
    registry.add_substrate_mcp_tool::<ListSubstrateToolsTool>();
    registry.add_substrate_mcp_tool::<ListWorkspaceToolsTool>();
    registry.add_substrate_mcp_tool::<ListSchemasTool>();
    registry.add_substrate_mcp_tool::<ListEdgeTypesTool>();
    registry.add_substrate_mcp_tool::<EmitApprovalPolicyTool>();
    registry.add_substrate_mcp_tool::<EmitApprovalVoteTool>();
    registry.add_substrate_mcp_tool::<TryEmitApprovalDecisionTool>();
    registry.add_substrate_mcp_tool::<EmitInterventionDecisionTool>();
    registry.add_substrate_mcp_tool::<ListChatTargetsTool>();
    registry.add_substrate_mcp_tool::<GetChatThreadTool>();
    registry.add_substrate_mcp_tool::<StartChatTool>();
    registry.add_substrate_mcp_tool::<EmitChatMessageTool>();
    registry.add_substrate_mcp_tool::<EmitChatReplyTool>();
    registry.add_substrate_mcp_tool::<CompactChatThreadTool>();
    registry.add_substrate_mcp_tool::<RequestEndChatTool>();
    registry.add_substrate_mcp_tool::<EndChatTool>();
}
