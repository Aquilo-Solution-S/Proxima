//! Personality row types.
//!
//! This module contains database row types for personality-related data:
//! - `PersonalityInstanceRow` - Full personality instance row
//! - `WakeEntryRow` - Wake entry configuration row
//! - `ChangeEventForWake` - Change event with wake context

use uuid::Uuid;

use crate::change_event::ChangeEvent;
use crate::personality::types::{
    PersonalityStatus, WakeChainDepth, WakeEntryAuthoredBy, WakeEntryGoalScope,
    WakeEntryTriggerKind,
};
use crate::{GoalId, MemoryId, Owner};

use super::personality::PersonalityInstanceId;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WakeEntryRow {
    pub wake_entry_id: Uuid,
    pub trigger_kind: WakeEntryTriggerKind,
    pub trigger_id: String,
    pub label: String,
    pub enabled: bool,
    pub authored_by: WakeEntryAuthoredBy,
    pub probability_promille: u16,
    pub goal_scope: WakeEntryGoalScope,
    pub instructions: String,
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
pub struct ChangeEventForWake {
    pub event: ChangeEvent,
    pub authoring_personality_instance_id: Option<PersonalityInstanceId>,
    pub wake_chain_depth: WakeChainDepth,
}

/// Triage-level summary of one active goal. Detail is reachable through
/// the goal read surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveGoalSummary {
    pub goal_id: GoalId,
    pub goal_activated_memory_id: Option<MemoryId>,
    pub title: String,
}
