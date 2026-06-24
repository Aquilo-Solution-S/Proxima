//! Personality wake/decide/write substrate.
//!
//! Personalities are build-time flavor declarations. Runtime instances
//! are addressed by `personality_instance_id` and point at a Root
//! Perspective plus `WakeEntry` rows in storage.

pub mod drafts;
pub mod requests;
pub mod rows;
pub mod types;
pub mod wake_validation;

#[allow(clippy::module_inception)]
pub mod personality;

// Re-export from personality submodule
pub use personality::{
    MAX_WAKE_CHAIN_DEPTH, PersonalityInstanceId, PersonalityRef,
    ROOT_PERSONALITY_PERSPECTIVE_SCHEMA_ID,
};

// Re-export from types submodule
pub use types::{
    PersonalityMemoryKind, PersonalityStatus, WakeChainDepth, WakeEntryAuthoredBy,
    WakeEntryGoalScope, WakeEntryTriggerKind,
};

// Re-export from drafts submodule
pub use drafts::{
    AbstractionRow, FactRow, MemorySnapshot, PersonalityMemoryDraft, PersonalityWriteOutcome,
    PersonalityWriteRequest, SidecarSpec, WakeEntryDraft,
};

// Re-export from rows submodule
pub use rows::{ActiveGoalSummary, ChangeEventForWake, PersonalityInstanceRow, WakeEntryRow};

// Re-export from requests submodule
pub use requests::{
    InstantiatePersonalityRequest, InstantiatePersonalityResponse, ListReadScopeRequest,
    ListReadScopeResponse, SetReadScopeRequest, SetReadScopeResponse, SetWakeEntriesRequest,
    SetWakeEntriesResponse, TombstonePersonalityRequest, TombstonePersonalityResponse,
};

pub use wake_validation::validate_wake_entries_detect_config;

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn wake_entry_accepts_promille_probability() {
        let entry = WakeEntryDraft::new(
            Uuid::from_u128(10),
            PersonalityInstanceId::new(Uuid::from_u128(1)),
            WakeEntryTriggerKind::OnMemory,
            "proxima-test/fact-v1",
            "on_test_fact",
            WakeEntryAuthoredBy::Any,
            250,
        )
        .unwrap();
        assert_eq!(entry.trigger_kind, WakeEntryTriggerKind::OnMemory);
        assert_eq!(entry.trigger_id, "proxima-test/fact-v1");
        assert_eq!(entry.probability_promille, 250);
    }

    #[test]
    fn wake_entry_rejects_probability_above_promille_ceiling() {
        let err = WakeEntryDraft::new(
            Uuid::from_u128(11),
            PersonalityInstanceId::new(Uuid::from_u128(2)),
            WakeEntryTriggerKind::OnMemory,
            "proxima-test/fact-v1",
            "on_test_fact",
            WakeEntryAuthoredBy::Any,
            1001,
        )
        .unwrap_err();
        assert!(err.to_string().contains("probability_promille"));
    }
}
