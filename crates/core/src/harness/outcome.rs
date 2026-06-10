//! Outcome classifier: derives `HarnessOutcomeKind` from structural provider,
//! loop, dispatcher, and tool-dispatch signals.

use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HarnessOutcomeKind {
    Succeeded,
    Truncated,
    Failed,
}

/// Structural reason the final provider or loop step stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinishReason {
    /// Model emitted a final assistant message with no tool calls.
    Stop,
    /// Harness observed the required durable fulfillment artifact.
    Fulfilled,
    /// Model wants to call one or more tools.
    ToolCalls,
    /// Provider returned a completion-token length cap.
    Length,
    /// Harness ran out of `max_rounds` before the model emitted `Stop`.
    MaxRounds,
    /// Dispatcher cancelled the invocation, usually during shutdown.
    Cancelled,
    /// Provider returned a finish reason the harness does not recognise.
    Unknown(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorClass {
    None,
    Auth,
    RateLimited,
    ContextLength,
    InvalidRequest,
    ServerError,
    Network,
    Timeout,
    Deserialize,
    InvocationTimeout,
    Cancelled,
    ToolErrorStreak,
    FulfillmentStalled,
    ToolDispatchFatal,
}

#[derive(Debug, Clone)]
pub struct HarnessOutcome {
    pub kind: HarnessOutcomeKind,
    pub finish_reason: FinishReason,
    pub error_class: ErrorClass,
    pub failure_reason: Option<String>,
    pub rounds_used: u32,
    pub duration_ms: u64,
    pub total_prompt_tokens: Option<u64>,
    pub total_completion_tokens: Option<u64>,
    pub tool_call_count: u32,
    /// Capped in-memory JSONL bytes. Phase 8 hands these to the wake-trace
    /// `CitedObject` emitter.
    pub jsonl_bytes: Vec<u8>,
    pub jsonl_truncated: bool,
    /// Per-wake egress proxy log (`docker logs` of the logging proxy).
    /// `None` for host-mode wakes or when the sandbox carried no proxy.
    pub network_log: Option<String>,
}

/// Exhaustive classifier over the spec's structural outcome rows.
#[must_use]
pub fn classify_outcome(
    finish_reason: FinishReason,
    error_class: ErrorClass,
    rounds_used: u32,
    max_rounds: u32,
) -> HarnessOutcomeKind {
    use ErrorClass as E;
    use FinishReason as F;

    match (finish_reason, error_class) {
        (F::Stop, E::None) => HarnessOutcomeKind::Succeeded,
        (F::Fulfilled, E::None) => HarnessOutcomeKind::Succeeded,
        (F::Length, E::None) => HarnessOutcomeKind::Truncated,
        (F::MaxRounds, E::None) if max_rounds > 0 && rounds_used >= max_rounds => {
            HarnessOutcomeKind::Truncated
        }
        (_, E::None) => HarnessOutcomeKind::Failed,
        (_, E::Auth) => HarnessOutcomeKind::Failed,
        (_, E::RateLimited) => HarnessOutcomeKind::Failed,
        (_, E::ContextLength) => HarnessOutcomeKind::Failed,
        (_, E::InvalidRequest) => HarnessOutcomeKind::Failed,
        (_, E::ServerError) => HarnessOutcomeKind::Failed,
        (_, E::Network) => HarnessOutcomeKind::Failed,
        (_, E::Timeout) => HarnessOutcomeKind::Failed,
        (_, E::Deserialize) => HarnessOutcomeKind::Failed,
        (_, E::InvocationTimeout) => HarnessOutcomeKind::Failed,
        (_, E::Cancelled) => HarnessOutcomeKind::Failed,
        (_, E::ToolErrorStreak) => HarnessOutcomeKind::Failed,
        (_, E::FulfillmentStalled) => HarnessOutcomeKind::Failed,
        (_, E::ToolDispatchFatal) => HarnessOutcomeKind::Failed,
    }
}

#[must_use]
pub fn duration_ms(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}
