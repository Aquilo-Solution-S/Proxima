//! Substrate-shipped MCP tools for personality config CRUD. Registered
//! into `FlavorRegistry::default()` so they are available in every
//! composite binary.
//!
//! See docs/superpowers/specs/2026-05-10-personality-mcp-crud-design.md.

pub mod audit;
pub mod payload;

pub mod get_personality;
pub mod list_personalities;
pub mod list_wake_entries;

pub use audit::{AuditEmit, emit_personality_config_changed};
pub use get_personality::GetPersonalityTool;
pub use list_personalities::ListPersonalitiesTool;
pub use list_wake_entries::ListWakeEntriesTool;
pub use payload::{
    PersonalityConfigChangedCaller, PersonalityConfigChangedSubject,
    PersonalityConfigChangedV1, PersonalityConfigChangedVerb,
};
