//! Outcome types and conversion for wake fire.

use crate::harness::{HarnessError, HarnessOutcome, HarnessOutcomeKind};

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
pub fn wake_outcome_from_harness_outcome(
    input: &FireWakeEntryInput,
    outcome_result: Result<HarnessOutcome, HarnessError>,
) -> WakeInvocationFinalizeOutcome {
    match outcome_result {
        Ok(HarnessOutcome {
            kind,
            rounds_used,
            duration_ms,
            failure_reason,
            ..
        }) => match kind {
            HarnessOutcomeKind::Succeeded => WakeInvocationFinalizeOutcome {
                status: crate::personality::WakeInvocationStatus::Succeeded,
                turn_count: u16::try_from(rounds_used).ok(),
                cost_usd: None,
                failure_reason: None,
                exit_code: Some(0),
                duration_ms: Some(duration_ms),
                stdout_tail: None,
                stderr_tail: None,
                stdout_truncated: false,
                stderr_truncated: false,
            },
            HarnessOutcomeKind::Truncated => WakeInvocationFinalizeOutcome {
                status: crate::personality::WakeInvocationStatus::Truncated,
                turn_count: u16::try_from(rounds_used)
                    .ok()
                    .or(Some(input.wake_entry.max_rounds)),
                cost_usd: None,
                failure_reason: failure_reason.or_else(|| Some("max_rounds_reached".to_string())),
                exit_code: Some(0),
                duration_ms: Some(duration_ms),
                stdout_tail: None,
                stderr_tail: None,
                stdout_truncated: false,
                stderr_truncated: false,
            },
            HarnessOutcomeKind::Failed => WakeInvocationFinalizeOutcome {
                status: crate::personality::WakeInvocationStatus::Failed,
                turn_count: u16::try_from(rounds_used).ok(),
                cost_usd: None,
                failure_reason,
                exit_code: Some(1),
                duration_ms: Some(duration_ms),
                stdout_tail: None,
                stderr_tail: None,
                stdout_truncated: false,
                stderr_truncated: false,
            },
        },
        Err(e) => WakeInvocationFinalizeOutcome::failed(format!("harness_error:{e}")),
    }
}
