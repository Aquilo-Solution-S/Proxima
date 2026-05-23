//! Personality wake/decide/write substrate.
//!
//! Personalities are build-time flavor declarations. Runtime instances
//! are addressed by `personality_instance_id` and point at a Root
//! Perspective plus WakeEntry rows in storage.

pub mod authorization;
pub mod context;
pub mod drafts;
pub mod emit_palette;
pub mod invocation;
pub mod produces;
pub mod requests;
pub mod rows;
pub mod tool;
pub mod tools;
pub mod types;

#[allow(clippy::module_inception)]
pub mod personality;

// Re-export from personality submodule
pub use personality::{
    MAX_WAKE_CHAIN_DEPTH, PersonalityInstanceId, PersonalityRef,
    ROOT_PERSONALITY_PERSPECTIVE_SCHEMA_ID, ROOT_PERSONALITY_PERSPECTIVE_SIDECAR_TABLE,
};

// Re-export from types submodule
pub use types::{
    PersonalityMemoryKind, PersonalityStatus, WakeChainDepth, WakeEntryAuthoredBy,
    WakeEntryExecutionMode, WakeEntryGoalScope, WakeEntryTriggerKind, WakeExecutionMode,
    WakeInvocationLogStatus, WakeInvocationStatus, WakeTraceOutcomeKind,
};

// Re-export from drafts submodule
pub use drafts::{
    AbstractionRow, FactRow, MemorySnapshot, PersonalityMemoryDraft, PersonalityWriteOutcome,
    PersonalityWriteRequest, SidecarSpec, WakeEntryDraft,
};

// Re-export from rows submodule
pub use rows::{
    ChangeEventForWake, PersonalityInstanceRow, PersonalityRuntimeRow,
    RootPersonalityPerspectiveRow, WakeDispatchEntryRow, WakeEntryRow,
};

// Re-export from context submodule
pub use context::PersonalityToolContext;

// Re-export from invocation submodule
pub use invocation::{
    WakeInvocationContinuation, WakeInvocationFinalize, WakeInvocationLogDraft,
    WakeInvocationLogRow, WakeInvocationRow, WakeInvocationStart,
};

// Re-export from requests submodule
pub use requests::{
    InstantiatePersonalityRequest, InstantiatePersonalityResponse, ListReadScopeRequest,
    ListReadScopeResponse, ListWakeInvocationsRequest, ReplayWakeEventsOutcome,
    ReplayWakeEventsRequest, SetReadScopeRequest, SetReadScopeResponse, SetWakeEntriesRequest,
    SetWakeEntriesResponse, TombstonePersonalityRequest, TombstonePersonalityResponse,
};

// Re-export from tool submodule
pub use tool::{PersonalityTool, PersonalityToolResult};

// Re-export from produces submodule
pub use produces::{writeable_relations_for_palette, writeable_schemas_for_palette};

// Re-export from emit_palette submodule
pub use emit_palette::{
    EMIT_ABSTRACTION_TOOL_ID, EMIT_PERSPECTIVE_TOOL_ID, ScopedEmitToolId, ScopedEmitToolIdError,
    broad_emit_kind, palette_authorizes_internal_tool, parse_scoped_emit_tool_id,
    scoped_emit_tool_id,
};

// Re-export from tools submodule
#[doc(hidden)]
pub use tools::__test_only_model_id_from_wake_invocation;
pub use tools::{ActiveGoalSummary, substrate_pack};

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    use crate::ModelTier;

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
            ModelTier::Fast,
            Some("local-cli:codex-spark".to_string()),
            vec!["core/query".to_string()],
            4,
        )
        .unwrap();
        assert_eq!(entry.trigger_kind, WakeEntryTriggerKind::OnMemory);
        assert_eq!(entry.trigger_id, "proxima-test/fact-v1");
        assert_eq!(entry.probability_promille, 250);
        assert_eq!(entry.model_tier, ModelTier::Fast);
        assert_eq!(
            entry.inference_target_ref.as_deref(),
            Some("local-cli:codex-spark")
        );
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
            ModelTier::Standard,
            None,
            Vec::new(),
            4,
        )
        .unwrap_err();
        assert!(err.to_string().contains("probability_promille"));
    }
}
