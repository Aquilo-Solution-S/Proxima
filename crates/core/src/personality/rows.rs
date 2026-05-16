//! Personality row types.
//!
//! This module contains database row types for personality-related data:
//! - `PersonalityInstanceRow` - Full personality instance row
//! - `PersonalityRuntimeRow` - Runtime projection of personality
//! - `RootPersonalityPerspectiveRow` - Root perspective sidecar row
//! - `WakeEntryRow` - Wake entry configuration row
//! - `WakeDispatchEntryRow` - Wake dispatch entry row
//! - `ChangeEventForWake` - Change event with wake context

use uuid::Uuid;

use crate::outbox::ChangeEvent;
use crate::personality::types::{
    PersonalityStatus, WakeChainDepth, WakeEntryAuthoredBy, WakeEntryExecutionMode,
    WakeEntryGoalScope, WakeEntryTriggerKind,
};
use crate::{MemoryId, Owner};

use super::drafts::WakeEntryDraft;
use super::personality::PersonalityInstanceId;

/// Owner-scoped projection of one row from `proxima_core.personality`.
/// Returned by `Storage::fetch_personality_runtime`. Carries just the
/// columns the wake-context assembler needs to read fresh per wake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonalityRuntimeRow {
    pub owner: Owner,
    pub personality_instance_id: PersonalityInstanceId,
    pub current_root_perspective_memory_id: MemoryId,
    pub display_name: String,
    pub status: PersonalityStatus,
}

/// Sidecar row backing a Root-Perspective memory. Returned by
/// `Storage::fetch_root_personality_perspective`. Lives next to
/// `PersonalityRuntimeRow` because both feed the wake-context
/// `root_perspective` envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootPersonalityPerspectiveRow {
    pub memory_id: MemoryId,
    pub display_name: String,
    pub purpose: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct WakeEntryRow {
    pub wake_entry_id: Uuid,
    pub trigger_kind: WakeEntryTriggerKind,
    pub trigger_id: String,
    pub label: String,
    pub enabled: bool,
    pub execution_mode: WakeEntryExecutionMode,
    pub authored_by: WakeEntryAuthoredBy,
    pub probability_promille: u16,
    pub goal_scope: WakeEntryGoalScope,
    pub instructions: String,
    pub model_tier: crate::ModelTier,
    pub inference_target_ref: Option<String>,
    pub substrate_tool_palette: Vec<String>,
    pub workspace_tool_palette: Vec<String>,
    pub max_rounds: u16,
    pub disabled_reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PersonalityInstanceRow {
    pub owner: Owner,
    pub personality_instance_id: PersonalityInstanceId,
    pub current_root_perspective_memory_id: MemoryId,
    pub display_name: String,
    pub status: PersonalityStatus,
    pub wake_entries: Vec<WakeEntryRow>,
}

#[derive(Debug, Clone)]
pub struct WakeDispatchEntryRow {
    pub owner: Owner,
    pub personality_instance_id: PersonalityInstanceId,
    pub current_root_perspective_memory_id: MemoryId,
    pub max_wake_chain_depth: u16,
    pub last_considered_seq: Uuid,
    pub wake_entry: WakeEntryDraft,
}

#[derive(Debug, Clone)]
pub struct ChangeEventForWake {
    pub event: ChangeEvent,
    pub authoring_personality_instance_id: Option<PersonalityInstanceId>,
    pub wake_chain_depth: WakeChainDepth,
}
