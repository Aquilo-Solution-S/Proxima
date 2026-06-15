//! Personality identity and reference types.
//!
//! This module contains the core identity types for personalities:
//! - `PersonalityInstanceId` - Unique identifier for a personality instance
//! - `PersonalityRef` - Reference to a personality instance
//! - Constants for root personality perspective schema

use uuid::Uuid;

/// Canonical schema id marking a personality's root self-perspective.
/// Stamped on the `Perspective` memory + `change_event` rows minted by
/// `instantiate_personality`. Schema-id marker only — there is no typed
/// sidecar table: identity is the emergent result of the perspectives the
/// agent authors, not a hardwired charter, so the root row carries only its
/// `display_name` (in `memories.text`).
pub const ROOT_PERSONALITY_PERSPECTIVE_SCHEMA_ID: &str =
    "proxima-core/root-personality-perspective-v1";

/// Maximum wake chain depth constant.
pub const MAX_WAKE_CHAIN_DEPTH: u16 = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct PersonalityInstanceId(Uuid);

impl PersonalityInstanceId {
    #[must_use]
    pub const fn new(inner: Uuid) -> Self {
        Self(inner)
    }

    #[must_use]
    pub const fn into_inner(self) -> Uuid {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PersonalityRef {
    pub personality_instance_id: PersonalityInstanceId,
}

impl PersonalityRef {
    #[must_use]
    pub const fn new(personality_instance_id: PersonalityInstanceId) -> Self {
        Self {
            personality_instance_id,
        }
    }
}
