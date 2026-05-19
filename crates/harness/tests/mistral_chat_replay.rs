use proxima_harness::conversation::{Conversation, ToolSpec};
use proxima_harness::providers::mistral_chat::MistralChatClient;
use proxima_harness::providers::{ProviderClient, RoundResult as ProviderRoundResult};
use proxima_harness::providers::{ProviderError, RoundResult, classify_chat_completions_fixture};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

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

#[tokio::test]
async fn mistral_client_sends_strict_single_tool_policy() {
    let (url, request_body) = spawn_capture_mock(STOP).await;
    let client = MistralChatClient {
        http: reqwest::Client::new(),
        base_url: url,
        model_id: "mistral-medium-latest".into(),
        api_key: "test".into(),
        temperature: Some(0.2),
        max_completion_tokens: Some(256),
    };

    let result = client
        .tool_round(
            &Conversation {
                system_prompt: "system".into(),
                user_seed: "user".into(),
                turns: Vec::new(),
            },
            &[ToolSpec {
                canonical: "workspace_shell".into(),
                provider_safe: "workspace_shell".into(),
                description: "Run a bounded command.".into(),
                input_schema: json!({
                    "$schema": "https://json-schema.org/draft/2020-12/schema",
                    "type": "object",
                    "properties": {
                        "command": { "type": "string" }
                    },
                    "required": ["command"]
                }),
            }],
            CancellationToken::new(),
        )
        .await;
    assert!(matches!(result, Ok(ProviderRoundResult::Final { .. })));

    let body = request_body.await.expect("captured request body");
    assert_eq!(body["tool_choice"], "auto");
    assert_eq!(body["parallel_tool_calls"], false);
    assert_eq!(body["tools"][0]["function"]["strict"], true);
    assert_eq!(
        body["tools"][0]["function"]["parameters"]["additionalProperties"],
        false
    );
    assert!(body["tools"][0]["function"]["parameters"]["$schema"].is_null());
}

async fn run_fixture(status: u16, body: &'static str) -> Result<RoundResult, ProviderError> {
    let resp = http::Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(body)
        .expect("fixture response");
    classify_chat_completions_fixture(resp.into()).await
}

async fn spawn_capture_mock(
    response_body: &'static str,
) -> (String, oneshot::Receiver<serde_json::Value>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = oneshot::channel();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let request = read_http_request(&mut stream).await;
        let body = request
            .split_once("\r\n\r\n")
            .map(|(_, body)| body)
            .unwrap_or("");
        let value = serde_json::from_str(body).unwrap_or_else(|e| {
            panic!("request body must be JSON: {e}; body was {body:?}");
        });
        let _ = tx.send(value);
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{response_body}",
            response_body.len()
        );
        stream.write_all(response.as_bytes()).await.unwrap();
    });
    (format!("http://{addr}"), rx)
}

async fn read_http_request(stream: &mut tokio::net::TcpStream) -> String {
    let mut bytes = Vec::new();
    let mut buf = [0_u8; 4096];
    loop {
        let n = stream.read(&mut buf).await.unwrap();
        if n == 0 {
            break;
        }
        bytes.extend_from_slice(&buf[..n]);
        if request_body_complete(&bytes) {
            break;
        }
    }
    String::from_utf8(bytes).expect("HTTP request is UTF-8")
}

fn request_body_complete(bytes: &[u8]) -> bool {
    let Some(header_end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") else {
        return false;
    };
    let headers = String::from_utf8_lossy(&bytes[..header_end]);
    let content_length = headers
        .lines()
        .find_map(|line| line.split_once(':'))
        .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.trim().parse::<usize>().ok())
        .unwrap_or(0);
    bytes.len() >= header_end + 4 + content_length
}
