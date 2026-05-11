//! Outcome types and conversion for wake fire.

use crate::wake::target_adapter::{TargetAdapterError, TargetOutcome, TargetOutcomeKind};

use super::input::FireWakeEntryInput;

/// Internal mirror of `WakeInvocationFinalize` that elides the routing
/// keys (owner / instance / entry / seq) the call site already knows.
#[derive(Debug, Clone)]
pub struct WakeInvocationFinalizeOutcome {
    pub status: crate::personality::WakeInvocationStatus,
    pub turn_count: Option<u16>,
    pub cost_usd: Option<f64>,
    pub failure_reason: Option<String>,
    pub exit_code: Option<i32>,
    pub duration_ms: Option<u64>,
    pub stdout_tail: Option<String>,
    pub stderr_tail: Option<String>,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
}

impl WakeInvocationFinalizeOutcome {
    /// Create a failed outcome with the given reason.
    pub fn failed(failure_reason: String) -> Self {
        Self {
            status: crate::personality::WakeInvocationStatus::Failed,
            turn_count: None,
            cost_usd: None,
            failure_reason: Some(failure_reason),
            exit_code: None,
            duration_ms: None,
            stdout_tail: None,
            stderr_tail: None,
            stdout_truncated: false,
            stderr_truncated: false,
        }
    }
}

/// Convert a target outcome result to a wake invocation finalize outcome.
pub fn wake_outcome_from_target_result(
    input: &FireWakeEntryInput,
    outcome_result: Result<TargetOutcome, TargetAdapterError>,
) -> WakeInvocationFinalizeOutcome {
    match outcome_result {
        Ok(TargetOutcome {
            kind,
            turn_count,
            exit_code,
            duration_ms,
            stdout_tail,
            stderr_tail,
            stdout_truncated,
            stderr_truncated,
            session_log_error: _,
        }) => match kind {
            TargetOutcomeKind::Succeeded => WakeInvocationFinalizeOutcome {
                status: crate::personality::WakeInvocationStatus::Succeeded,
                turn_count: turn_count.and_then(|c| u16::try_from(c.max(0)).ok()),
                cost_usd: None,
                failure_reason: None,
                exit_code,
                duration_ms: Some(duration_ms),
                stdout_tail: Some(stdout_tail),
                stderr_tail: Some(stderr_tail),
                stdout_truncated,
                stderr_truncated,
            },
            TargetOutcomeKind::Truncated => WakeInvocationFinalizeOutcome {
                status: crate::personality::WakeInvocationStatus::Truncated,
                turn_count: turn_count
                    .and_then(|c| u16::try_from(c.max(0)).ok())
                    .or(Some(input.wake_entry.max_rounds)),
                cost_usd: None,
                failure_reason: Some("max_rounds_reached".to_string()),
                exit_code,
                duration_ms: Some(duration_ms),
                stdout_tail: Some(stdout_tail),
                stderr_tail: Some(stderr_tail),
                stdout_truncated,
                stderr_truncated,
            },
            TargetOutcomeKind::Failed => WakeInvocationFinalizeOutcome {
                status: crate::personality::WakeInvocationStatus::Failed,
                turn_count: turn_count.and_then(|c| u16::try_from(c.max(0)).ok()),
                cost_usd: None,
                failure_reason: Some(stderr_tail.clone()),
                exit_code,
                duration_ms: Some(duration_ms),
                stdout_tail: Some(stdout_tail),
                stderr_tail: Some(stderr_tail),
                stdout_truncated,
                stderr_truncated,
            },
        },
        Err(e) => WakeInvocationFinalizeOutcome::failed(format!("adapter_error: {e}")),
    }
}
