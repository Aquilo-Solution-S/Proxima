//! Personality request and response types.
//!
//! This module contains request/response types for personality operations:
//! - `InstantiatePersonalityRequest/Response` - Create a new personality
//! - `SetWakeEntriesRequest/Response` - Configure wake entries
//! - `TombstonePersonalityRequest/Response` - Delete a personality
//! - `ListWakeInvocationsRequest` - List wake invocations
//! - `ReplayWakeEventsRequest/Outcome` - Replay wake events

use uuid::Uuid;

use crate::Owner;

use super::drafts::WakeEntryDraft;
use super::personality::PersonalityInstanceId;

#[derive(Debug, Clone)]
pub struct InstantiatePersonalityRequest {
    pub owner: Owner,
    pub display_name: String,
    pub purpose: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct InstantiatePersonalityResponse {
    pub instance_id: PersonalityInstanceId,
}

#[derive(Debug, Clone)]
pub struct SetWakeEntriesRequest {
    pub owner: Owner,
    pub personality_instance_id: PersonalityInstanceId,
    pub entries: Vec<WakeEntryDraft>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct SetWakeEntriesResponse {
    pub active_entries: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListReadScopeRequest {
    pub owner: Owner,
    pub reader_personality_instance_id: PersonalityInstanceId,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct ListReadScopeResponse {
    pub readable_personality_instance_ids: Vec<PersonalityInstanceId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetReadScopeRequest {
    pub owner: Owner,
    pub reader_personality_instance_id: PersonalityInstanceId,
    pub readable_personality_instance_ids: Vec<PersonalityInstanceId>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct SetReadScopeResponse {
    pub readable_count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TombstonePersonalityRequest {
    pub owner: Owner,
    pub personality_instance_id: PersonalityInstanceId,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct TombstonePersonalityResponse {
    pub status: String,
    pub idempotent_replay: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListWakeInvocationsRequest {
    pub owner: Owner,
    pub personality_instance_id: PersonalityInstanceId,
    pub wake_entry_id: Option<Uuid>,
    pub limit: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayWakeEventsRequest {
    pub owner: Owner,
    pub personality_instance_id: PersonalityInstanceId,
    pub wake_entry_id: Option<Uuid>,
    pub after_seq: Option<Uuid>,
    pub until_seq: Option<Uuid>,
    pub event_limit: u16,
    pub max_invocations: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct ReplayWakeEventsOutcome {
    pub considered_events: u32,
    pub eligible_events: u32,
    pub started_invocations: u32,
    pub already_recorded: u32,
    pub skipped: u32,
    pub complete: bool,
    pub next_after_seq: Option<Uuid>,
}
