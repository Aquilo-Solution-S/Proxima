use proxima_harness::providers::{ProviderError, RoundResult, classify_chat_completions_fixture};
use serde_json::json;

const STOP: &str = include_str!("fixtures/mistral_chat/stop.json");
const TOOL_CALLS: &str = include_str!("fixtures/mistral_chat/tool_calls.json");
const LENGTH: &str = include_str!("fixtures/mistral_chat/length.json");
const AUTH_401: &str = include_str!("fixtures/mistral_chat/auth_401.json");
const RATE_LIMIT_429: &str = include_str!("fixtures/mistral_chat/rate_limit_429.json");
const CONTEXT_LENGTH_400: &str = include_str!("fixtures/mistral_chat/context_length_400.json");
const UNSUPPORTED_FINISH: &str = include_str!("fixtures/mistral_chat/unsupported_finish.json");

#[tokio::test]
async fn stop_fixture_returns_final() {
    let result = run_fixture(200, STOP).await;
    match result {
        Ok(RoundResult::Final { text, .. }) => assert_eq!(text, "done"),
        other => panic!("expected final result, got {other:?}"),
    }
}

#[tokio::test]
async fn tool_calls_fixture_preserves_provider_safe_tool_name() {
    let result = run_fixture(200, TOOL_CALLS).await;
    match result {
        Ok(RoundResult::ToolCalls { calls, .. }) => {
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].call_id, "call_1");
            assert_eq!(calls[0].tool_name, "core_emit_perspective");
            assert_eq!(calls[0].arguments, json!({"text": "hello"}));
        }
        other => panic!("expected tool calls result, got {other:?}"),
    }
}

#[tokio::test]
async fn length_fixture_returns_length_cap() {
    let result = run_fixture(200, LENGTH).await;
    match result {
        Ok(RoundResult::LengthCap { partial_text, .. }) => {
            assert_eq!(partial_text.as_deref(), Some("partial"));
        }
        other => panic!("expected length cap result, got {other:?}"),
    }
}

#[tokio::test]
async fn auth_status_returns_auth_error() {
    let result = run_fixture(401, AUTH_401).await;
    assert!(matches!(result, Err(ProviderError::Auth)));

    let result = run_fixture(403, AUTH_401).await;
    assert!(matches!(result, Err(ProviderError::Auth)));
}

#[tokio::test]
async fn rate_limit_status_returns_rate_limited_error() {
    let result = run_fixture(429, RATE_LIMIT_429).await;
    assert!(matches!(result, Err(ProviderError::RateLimited { .. })));
}

#[tokio::test]
async fn context_length_400_returns_context_length_error() {
    let result = run_fixture(400, CONTEXT_LENGTH_400).await;
    assert!(matches!(result, Err(ProviderError::ContextLength)));
}

#[tokio::test]
async fn unsupported_finish_returns_deserialize_error() {
    let result = run_fixture(200, UNSUPPORTED_FINISH).await;
    assert!(matches!(result, Err(ProviderError::Deserialize(_))));
}

async fn run_fixture(status: u16, body: &'static str) -> Result<RoundResult, ProviderError> {
    let resp = http::Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(body)
        .expect("fixture response");
    classify_chat_completions_fixture(resp.into()).await
}
