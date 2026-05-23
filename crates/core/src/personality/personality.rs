//! Personality identity and reference types.
//!
//! This module contains the core identity types for personalities:
//! - `PersonalityInstanceId` - Unique identifier for a personality instance
//! - `PersonalityRef` - Reference to a personality instance
//! - Constants for root personality perspective schema

use uuid::Uuid;

/// Canonical schema id for the Root-Perspective sidecar that backs every
/// personality after Phase 2 Step 1. Stamped on the memory + change_event
/// rows minted by `instantiate_personality`.
pub const ROOT_PERSONALITY_PERSPECTIVE_SCHEMA_ID: &str =
    "proxima-core/root-personality-perspective-v1";

/// Sidecar table backing [`ROOT_PERSONALITY_PERSPECTIVE_SCHEMA_ID`].
pub const ROOT_PERSONALITY_PERSPECTIVE_SIDECAR_TABLE: &str =
    "proxima_core.root_personality_perspective_v1";

/// Maximum wake chain depth constant.
pub const MAX_WAKE_CHAIN_DEPTH: u16 = 10;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, specta::Type,
)]
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

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type)]
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
