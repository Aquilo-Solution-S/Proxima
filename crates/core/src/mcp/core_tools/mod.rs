//! Substrate-shipped MCP tools for personality config CRUD. Registered
//! into `FlavorRegistry::default()` so they are available in every
//! composite binary.
//!
//! See docs/superpowers/specs/2026-05-10-personality-mcp-crud-design.md.

pub mod payload;

pub use payload::{
    PersonalityConfigChangedCaller, PersonalityConfigChangedSubject,
    PersonalityConfigChangedV1, PersonalityConfigChangedVerb,
};
