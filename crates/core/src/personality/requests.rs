//! Personality request and response types.
//!
//! This module contains request/response types for personality operations:
//! - `InstantiatePersonalityRequest/Response` - Create a new personality
//! - `SetWakeEntriesRequest/Response` - Configure wake entries
//! - `TombstonePersonalityRequest/Response` - Delete a personality

use crate::{Owner, Principal};

use super::drafts::WakeEntryDraft;
use super::personality::PersonalityInstanceId;

#[derive(Debug, Clone)]
pub struct InstantiatePersonalityRequest {
    pub principal: Principal,
    pub display_name: String,
}

impl InstantiatePersonalityRequest {
    /// The storage `Owner` (= principal) for this request.
    #[must_use]
    pub fn owner(&self) -> Owner {
        self.principal.clone()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct InstantiatePersonalityResponse {
    pub instance_id: PersonalityInstanceId,
}

#[derive(Debug, Clone)]
pub struct SetWakeEntriesRequest {
    pub principal: Principal,
    pub personality_instance_id: PersonalityInstanceId,
    pub entries: Vec<WakeEntryDraft>,
}

impl SetWakeEntriesRequest {
    /// The storage `Owner` (= principal) for this request.
    #[must_use]
    pub fn owner(&self) -> Owner {
        self.principal.clone()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SetWakeEntriesResponse {
    pub active_entries: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListReadScopeRequest {
    pub principal: Principal,
    pub reader_personality_instance_id: PersonalityInstanceId,
}

impl ListReadScopeRequest {
    /// The storage `Owner` (= principal) for this request.
    #[must_use]
    pub fn owner(&self) -> Owner {
        self.principal.clone()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ListReadScopeResponse {
    pub readable_personality_instance_ids: Vec<PersonalityInstanceId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetReadScopeRequest {
    pub principal: Principal,
    pub reader_personality_instance_id: PersonalityInstanceId,
    pub readable_personality_instance_ids: Vec<PersonalityInstanceId>,
}

impl SetReadScopeRequest {
    /// The storage `Owner` (= principal) for this request.
    #[must_use]
    pub fn owner(&self) -> Owner {
        self.principal.clone()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SetReadScopeResponse {
    pub readable_count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TombstonePersonalityRequest {
    pub principal: Principal,
    pub personality_instance_id: PersonalityInstanceId,
}

impl TombstonePersonalityRequest {
    /// The storage `Owner` (= principal) for this request.
    #[must_use]
    pub fn owner(&self) -> Owner {
        self.principal.clone()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TombstonePersonalityResponse {
    pub status: String,
    pub idempotent_replay: bool,
}
