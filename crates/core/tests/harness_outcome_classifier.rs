//! Exhaustive coverage of the spec outcome classification table.

use proxima_core::harness::{ErrorClass, FinishReason, HarnessOutcomeKind, classify_outcome};

fn case(
    fr: FinishReason,
    ec: ErrorClass,
    rounds_used: u32,
    max_rounds: u32,
    expect: HarnessOutcomeKind,
    label: &str,
) {
    let got = classify_outcome(fr, ec, rounds_used, max_rounds);
    assert_eq!(got, expect, "row {label}: got {got:?}, want {expect:?}");
}

#[test]
fn final_round_succeeds() {
    case(
        FinishReason::Stop,
        ErrorClass::None,
        3,
        30,
        HarnessOutcomeKind::Succeeded,
        "RoundResult::Final",
    );
}

#[test]
fn fulfilled_round_succeeds() {
    case(
        FinishReason::Fulfilled,
        ErrorClass::None,
        1,
        0,
        HarnessOutcomeKind::Succeeded,
        "durable fulfillment observed",
    );
}

#[test]
fn length_cap_truncates() {
    case(
        FinishReason::Length,
        ErrorClass::None,
        5,
        30,
        HarnessOutcomeKind::Truncated,
        "RoundResult::LengthCap",
    );
}

#[test]
fn max_rounds_while_model_still_requests_tools_truncates() {
    case(
        FinishReason::MaxRounds,
        ErrorClass::None,
        30,
        30,
        HarnessOutcomeKind::Truncated,
        "max_rounds reached",
    );
}

#[test]
fn auth_error_fails() {
    case(
        FinishReason::Stop,
        ErrorClass::Auth,
        0,
        30,
        HarnessOutcomeKind::Failed,
        "ProviderError::Auth",
    );
}

#[test]
fn rate_limited_fails() {
    case(
        FinishReason::Stop,
        ErrorClass::RateLimited,
        1,
        30,
        HarnessOutcomeKind::Failed,
        "ProviderError::RateLimited",
    );
}

#[test]
fn context_length_error_fails() {
    case(
        FinishReason::Stop,
        ErrorClass::ContextLength,
        2,
        30,
        HarnessOutcomeKind::Failed,
        "ProviderError::ContextLength",
    );
}

#[test]
fn invalid_request_fails() {
    case(
        FinishReason::Stop,
        ErrorClass::InvalidRequest,
        0,
        30,
        HarnessOutcomeKind::Failed,
        "ProviderError::InvalidRequest",
    );
}

#[test]
fn server_error_fails() {
    case(
        FinishReason::Stop,
        ErrorClass::ServerError,
        2,
        30,
        HarnessOutcomeKind::Failed,
        "ProviderError::ServerError",
    );
}

#[test]
fn network_error_fails() {
    case(
        FinishReason::Stop,
        ErrorClass::Network,
        2,
        30,
        HarnessOutcomeKind::Failed,
        "ProviderError::Network",
    );
}

#[test]
fn per_round_timeout_fails() {
    case(
        FinishReason::Stop,
        ErrorClass::Timeout,
        4,
        30,
        HarnessOutcomeKind::Failed,
        "ProviderError::Timeout",
    );
}

#[test]
fn deserialize_fails() {
    case(
        FinishReason::Stop,
        ErrorClass::Deserialize,
        0,
        30,
        HarnessOutcomeKind::Failed,
        "ProviderError::Deserialize",
    );
}

#[test]
fn dispatcher_wall_clock_timeout_fails() {
    case(
        FinishReason::Stop,
        ErrorClass::InvocationTimeout,
        4,
        30,
        HarnessOutcomeKind::Failed,
        "invocation_timeout",
    );
}

#[test]
fn cancellation_fails() {
    case(
        FinishReason::Cancelled,
        ErrorClass::Cancelled,
        4,
        30,
        HarnessOutcomeKind::Failed,
        "cancelled",
    );
}

#[test]
fn workspace_tool_error_streak_fails() {
    case(
        FinishReason::ToolCalls,
        ErrorClass::ToolErrorStreak,
        4,
        30,
        HarnessOutcomeKind::Failed,
        "tool_error_streak",
    );
}

#[test]
fn fulfillment_stall_fails() {
    case(
        FinishReason::ToolCalls,
        ErrorClass::FulfillmentStalled,
        16,
        0,
        HarnessOutcomeKind::Failed,
        "fulfillment stalled",
    );
}

#[test]
fn tool_calls_with_no_error_is_mid_loop_exit_and_fails() {
    case(
        FinishReason::ToolCalls,
        ErrorClass::None,
        3,
        30,
        HarnessOutcomeKind::Failed,
        "tool_calls/none",
    );
}

#[test]
fn unknown_finish_reason_fails() {
    case(
        FinishReason::Unknown("eos_garbage"),
        ErrorClass::None,
        1,
        30,
        HarnessOutcomeKind::Failed,
        "unknown finish reason",
    );
}
