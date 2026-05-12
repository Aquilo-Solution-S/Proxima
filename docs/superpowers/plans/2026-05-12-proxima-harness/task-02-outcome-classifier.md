# Task 1.2 — Exhaustive classifier test

> Part of [Proxima Harness Implementation Plan](README.md). Subagent execution: implement steps in order, commit at the end of the task.

**Files:**
- Create: `crates/core/tests/harness_outcome_classifier.rs`

- [ ] **Step 1: Write the table-driven test**

```rust
//! Exhaustive coverage of the (FinishReason, ErrorClass) classification
//! table. One assertion per documented row in spec §"Outcome
//! classification (exhaustive)".

use proxima_core::harness::{
    ErrorClass, FinishReason, HarnessOutcomeKind, classify_outcome,
};

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
fn stop_with_no_error_succeeds() {
    case(
        FinishReason::Stop,
        ErrorClass::None,
        3,
        30,
        HarnessOutcomeKind::Succeeded,
        "stop/none",
    );
}

#[test]
fn length_truncates() {
    case(
        FinishReason::Length,
        ErrorClass::None,
        5,
        30,
        HarnessOutcomeKind::Truncated,
        "length/none",
    );
}

#[test]
fn context_length_error_truncates_regardless_of_finish_reason() {
    case(
        FinishReason::Stop,
        ErrorClass::ContextLength,
        2,
        30,
        HarnessOutcomeKind::Truncated,
        "stop/context_length",
    );
    case(
        FinishReason::ToolCalls,
        ErrorClass::ContextLength,
        2,
        30,
        HarnessOutcomeKind::Truncated,
        "tool_calls/context_length",
    );
}

#[test]
fn max_rounds_hit_truncates() {
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
fn max_rounds_without_cap_fails() {
    // max_rounds == 0 means "no model-imposed cap"; reaching
    // MaxRounds in that mode is a harness bug.
    case(
        FinishReason::MaxRounds,
        ErrorClass::None,
        0,
        0,
        HarnessOutcomeKind::Failed,
        "max_rounds with no cap",
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
        "auth",
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
        "rate_limited",
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
        "invalid_request",
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
        "server_error",
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
        "network",
    );
}

#[test]
fn timeout_fails() {
    case(
        FinishReason::Stop,
        ErrorClass::Timeout,
        4,
        30,
        HarnessOutcomeKind::Failed,
        "timeout",
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
        "deserialize",
    );
}

#[test]
fn tool_dispatch_fatal_fails() {
    case(
        FinishReason::ToolCalls,
        ErrorClass::ToolDispatchFatal,
        2,
        30,
        HarnessOutcomeKind::Failed,
        "tool_dispatch_fatal",
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

#[test]
fn tool_calls_with_no_error_is_treated_as_mid_loop_exit() {
    // Should not happen normally; classifier treats it as Failed
    // because a clean loop never exits with finish_reason == ToolCalls.
    case(
        FinishReason::ToolCalls,
        ErrorClass::None,
        3,
        30,
        HarnessOutcomeKind::Failed,
        "tool_calls/none (mid-loop exit)",
    );
}
```

- [ ] **Step 2: Run the test**

Run: `cargo test -p proxima-core --test harness_outcome_classifier`
Expected: all 14 tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/core/tests/harness_outcome_classifier.rs
git commit -m "core(harness): exhaustive outcome-classifier table tests"
```
