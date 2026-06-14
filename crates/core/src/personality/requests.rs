//! Personality request and response types.
//!
//! This module contains request/response types for personality operations:
//! - `InstantiatePersonalityRequest/Response` - Create a new personality
//! - `SetWakeEntriesRequest/Response` - Configure wake entries
//! - `TombstonePersonalityRequest/Response` - Delete a personality

use crate::{OrgId, Owner, Principal};

use super::drafts::WakeEntryDraft;
use super::personality::PersonalityInstanceId;

#[derive(Debug, Clone)]
pub struct InstantiatePersonalityRequest {
    pub principal: Principal,
    pub org_id: Option<OrgId>,
    pub display_name: String,
    pub purpose: String,
}

impl InstantiatePersonalityRequest {
    /// Reconstructs the storage `Owner` after verb-layer stamping.
    ///
    /// # Panics
    ///
    /// Panics if `stamp_owner` has not populated `org_id` before storage or hash use.
    #[must_use]
    pub fn owner(&self) -> Owner {
        Owner {
            principal: self.principal.clone(),
            org_id: self
                .org_id
                .expect("InstantiatePersonalityRequest org_id must be stamped before storage use"),
        }
    }

    pub fn stamp_owner(&mut self, stamped: Owner) {
        self.principal = stamped.principal;
        self.org_id = Some(stamped.org_id);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct InstantiatePersonalityResponse {
    pub instance_id: PersonalityInstanceId,
}

#[derive(Debug, Clone)]
pub struct SetWakeEntriesRequest {
    pub principal: Principal,
    pub org_id: Option<OrgId>,
    pub personality_instance_id: PersonalityInstanceId,
    pub entries: Vec<WakeEntryDraft>,
}

impl SetWakeEntriesRequest {
    /// Reconstructs the storage `Owner` after verb-layer stamping.
    ///
    /// # Panics
    ///
    /// Panics if `stamp_owner` has not populated `org_id` before storage or hash use.
    #[must_use]
    pub fn owner(&self) -> Owner {
        Owner {
            principal: self.principal.clone(),
            org_id: self
                .org_id
                .expect("SetWakeEntriesRequest org_id must be stamped before storage use"),
        }
    }

    pub fn stamp_owner(&mut self, stamped: Owner) {
        self.principal = stamped.principal;
        self.org_id = Some(stamped.org_id);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct SetWakeEntriesResponse {
    pub active_entries: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListReadScopeRequest {
    pub principal: Principal,
    pub org_id: Option<OrgId>,
    pub reader_personality_instance_id: PersonalityInstanceId,
}

impl ListReadScopeRequest {
    /// Reconstructs the storage `Owner` after verb-layer stamping.
    ///
    /// # Panics
    ///
    /// Panics if `stamp_owner` has not populated `org_id` before storage or hash use.
    #[must_use]
    pub fn owner(&self) -> Owner {
        Owner {
            principal: self.principal.clone(),
            org_id: self
                .org_id
                .expect("ListReadScopeRequest org_id must be stamped before storage use"),
        }
    }

    pub fn stamp_owner(&mut self, stamped: Owner) {
        self.principal = stamped.principal;
        self.org_id = Some(stamped.org_id);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct ListReadScopeResponse {
    pub readable_personality_instance_ids: Vec<PersonalityInstanceId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetReadScopeRequest {
    pub principal: Principal,
    pub org_id: Option<OrgId>,
    pub reader_personality_instance_id: PersonalityInstanceId,
    pub readable_personality_instance_ids: Vec<PersonalityInstanceId>,
}

impl SetReadScopeRequest {
    /// Reconstructs the storage `Owner` after verb-layer stamping.
    ///
    /// # Panics
    ///
    /// Panics if `stamp_owner` has not populated `org_id` before storage or hash use.
    #[must_use]
    pub fn owner(&self) -> Owner {
        Owner {
            principal: self.principal.clone(),
            org_id: self
                .org_id
                .expect("SetReadScopeRequest org_id must be stamped before storage use"),
        }
    }

    pub fn stamp_owner(&mut self, stamped: Owner) {
        self.principal = stamped.principal;
        self.org_id = Some(stamped.org_id);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct SetReadScopeResponse {
    pub readable_count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TombstonePersonalityRequest {
    pub principal: Principal,
    pub org_id: Option<OrgId>,
    pub personality_instance_id: PersonalityInstanceId,
}

impl TombstonePersonalityRequest {
    /// Reconstructs the storage `Owner` after verb-layer stamping.
    ///
    /// # Panics
    ///
    /// Panics if `stamp_owner` has not populated `org_id` before storage or hash use.
    #[must_use]
    pub fn owner(&self) -> Owner {
        Owner {
            principal: self.principal.clone(),
            org_id: self
                .org_id
                .expect("TombstonePersonalityRequest org_id must be stamped before storage use"),
        }
    }

    pub fn stamp_owner(&mut self, stamped: Owner) {
        self.principal = stamped.principal;
        self.org_id = Some(stamped.org_id);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct TombstonePersonalityResponse {
    pub status: String,
    pub idempotent_replay: bool,
}
