//! Personality request and response types.
//!
//! This module contains request/response types for personality operations:
//! - `InstantiatePersonalityRequest/Response` - Create a new personality
//! - `SetWakeEntriesRequest/Response` - Configure wake entries
//! - `TombstonePersonalityRequest/Response` - Delete a personality

use crate::{Owner, OwnerRef};

use super::drafts::WakeEntryDraft;
use super::personality::PersonalityInstanceId;

#[derive(Debug, Clone)]
pub struct InstantiatePersonalityRequest {
    pub principal: OwnerRef,
    pub display_name: String,
}

impl InstantiatePersonalityRequest {
    /// The storage `Owner` (= principal) for this request.
    #[must_use]
    pub fn owner(&self) -> Owner {
        self.principal
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct InstantiatePersonalityResponse {
    pub instance_id: PersonalityInstanceId,
}

#[derive(Debug, Clone)]
pub struct SetWakeEntriesRequest {
    pub principal: OwnerRef,
    pub personality_instance_id: PersonalityInstanceId,
    pub entries: Vec<WakeEntryDraft>,
}

impl SetWakeEntriesRequest {
    /// The storage `Owner` (= principal) for this request.
    #[must_use]
    pub fn owner(&self) -> Owner {
        self.principal
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SetWakeEntriesResponse {
    pub active_entries: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TombstonePersonalityRequest {
    pub principal: OwnerRef,
    pub personality_instance_id: PersonalityInstanceId,
}

impl TombstonePersonalityRequest {
    /// The storage `Owner` (= principal) for this request.
    #[must_use]
    pub fn owner(&self) -> Owner {
        self.principal
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TombstonePersonalityResponse {
    pub status: String,
    pub idempotent_replay: bool,
}
