//! Substrate-shipped MCP tools for personality config CRUD. Registered
//! into `FlavorRegistry::default()` so they are available in every
//! composite binary.
//!
//! See docs/superpowers/specs/2026-05-10-personality-mcp-crud-design.md.

pub mod audit;
pub mod payload;

pub mod get_personality;
pub mod list_edge_types;
pub mod list_inference_targets;
pub mod list_inference_tier_bindings;
pub mod list_personalities;
pub mod list_recipes;
pub mod list_schemas;
pub mod list_substrate_tools;
pub mod list_wake_entries;
pub mod list_workspace_tools;

pub use audit::{AuditEmit, emit_personality_config_changed};
pub use get_personality::GetPersonalityTool;
pub use list_edge_types::ListEdgeTypesTool;
pub use list_inference_targets::ListInferenceTargetsTool;
pub use list_inference_tier_bindings::ListInferenceTierBindingsTool;
pub use list_personalities::ListPersonalitiesTool;
pub use list_recipes::ListRecipesTool;
pub use list_schemas::ListSchemasTool;
pub use list_substrate_tools::ListSubstrateToolsTool;
pub use list_wake_entries::ListWakeEntriesTool;
pub use list_workspace_tools::ListWorkspaceToolsTool;
pub use payload::{
    PersonalityConfigChangedCaller, PersonalityConfigChangedSubject,
    PersonalityConfigChangedV1, PersonalityConfigChangedVerb,
};
