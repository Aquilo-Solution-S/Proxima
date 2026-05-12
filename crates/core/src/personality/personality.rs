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

/// Canonical workspace tool catalog. `set_wake_entries` validates
/// declared palettes against this list, and the harness builds its
/// `HarnessProgram` from the palette so only listed tools reach the
/// provider.
pub const WORKSPACE_TOOL_CATALOG: &[(&str, &str)] = &[
    (
        "proxima-workspace/shell",
        "Run shell commands (build, test, git, package managers)",
    ),
    (
        "proxima-workspace/text_editor",
        "View and edit files in the workspace",
    ),
    ("proxima-workspace/list_files", "List files and directories"),
];

/// Returns the set of workspace tool IDs from the catalog.
#[must_use]
pub fn workspace_tool_ids() -> std::collections::HashSet<String> {
    WORKSPACE_TOOL_CATALOG
        .iter()
        .map(|(id, _)| (*id).to_string())
        .collect()
}

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
