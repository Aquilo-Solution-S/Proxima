//! Finalization and logging utilities for wake fire.

use std::path::PathBuf;

use uuid::Uuid;

use crate::Owner;
use crate::engine::Engine;
use crate::personality::{WakeInvocationFinalize, WakeInvocationLogDraft, WakeInvocationLogStatus};

use super::input::FireWakeEntryInput;
use super::outcome::WakeInvocationFinalizeOutcome;

/// Finalize a wake invocation by writing the outcome to storage.
pub async fn finalize(
    engine: &Engine,
    input: &FireWakeEntryInput,
    outcome: WakeInvocationFinalizeOutcome,
) -> Result<(), crate::error::ProtocolError> {
    engine
        .storage()
        .finalize_wake_invocation(&WakeInvocationFinalize {
            owner: input.owner.clone(),
            personality_instance_id: input.personality_instance_id,
            wake_entry_id: input.wake_entry.wake_entry_id,
            change_event_seq: input.change_event_seq,
            status: outcome.status,
            turn_count: outcome.turn_count,
            cost_usd: outcome.cost_usd,
            failure_reason: outcome.failure_reason,
            exit_code: outcome.exit_code,
            duration_ms: outcome.duration_ms,
            stdout_tail: outcome.stdout_tail,
            stderr_tail: outcome.stderr_tail,
            stdout_truncated: outcome.stdout_truncated,
            stderr_truncated: outcome.stderr_truncated,
        })
        .await
        .map_err(|e| {
            crate::error::ProtocolError::internal(format!("finalize_wake_invocation: {e}"))
        })
}

/// Append a session artifact log entry.
pub async fn append_session_artifact_log(
    engine: &Engine,
    input: &FireWakeEntryInput,
    status: WakeInvocationLogStatus,
    message_tail: String,
) {
    if let Err(err) = engine
        .storage()
        .append_wake_invocation_log(&WakeInvocationLogDraft {
            owner: input.owner.clone(),
            personality_instance_id: input.personality_instance_id,
            wake_entry_id: input.wake_entry.wake_entry_id,
            change_event_seq: input.change_event_seq,
            phase: "session_artifact".to_string(),
            tool_id: None,
            status,
            duration_ms: None,
            message_tail: Some(message_tail),
        })
        .await
    {
        tracing::warn!(
            personality_instance_id = %input.personality_instance_id.into_inner(),
            wake_entry_id = %input.wake_entry.wake_entry_id,
            change_event_seq = %input.change_event_seq,
            error = %err,
            "failed to append wake session artifact log"
        );
    }
}

/// Append session log error if present in the outcome.
pub async fn append_session_log_error_if_present(
    engine: &Engine,
    input: &FireWakeEntryInput,
    outcome_result: &Result<crate::harness::HarnessOutcome, crate::harness::HarnessError>,
) {
    if let Err(error) = outcome_result {
        append_session_artifact_log(engine, input, WakeInvocationLogStatus::Failed, error.to_string()).await;
    }
}

/// Generate the wake session log path.
pub fn wake_session_log_path(owner: &Owner, invocation_id: Uuid) -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join(".proxima/wake-runs")
        .join(owner_principal_segment(owner))
        .join(invocation_id.to_string())
        .join("worker-session.jsonl")
}

/// Generate the owner principal segment for paths.
pub fn owner_principal_segment(owner: &Owner) -> String {
    match &owner.principal {
        crate::Principal::User(user) => format!("user-{}", user.into_inner()),
        crate::Principal::Group(group) => format!("group-{}", group.into_inner()),
    }
}
