use proxima_harness::conversation::{Conversation, ToolSpec};
use proxima_harness::providers::openai_chat::OpenAIChatClient;
use proxima_harness::providers::{ProviderClient, ProviderError, RoundResult};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

const STOP: &str = include_str!("fixtures/openai_chat/stop.json");
const TOOL_CALLS: &str = include_str!("fixtures/openai_chat/tool_calls.json");
const LENGTH: &str = include_str!("fixtures/openai_chat/length.json");
const AUTH_401: &str = include_str!("fixtures/openai_chat/auth_401.json");
const RATE_LIMIT_429: &str = include_str!("fixtures/openai_chat/rate_limit_429.json");
const CONTEXT_LENGTH_400: &str = include_str!("fixtures/openai_chat/context_length_400.json");
const UNSUPPORTED_FINISH: &str = include_str!("fixtures/openai_chat/unsupported_finish.json");
const MISSING_FINISH: &str = r#"{
  "id": "chatcmpl_missing_finish",
  "object": "chat.completion",
  "choices": [
    {
      "index": 0,
      "message": {
        "role": "assistant",
        "content": "done"
      }
    }
  ]
}"#;

#[tokio::test]
async fn openai_chat_stop_returns_final() {
    let result = run_fixture("200 OK", STOP).await;
    match result {
        Ok(RoundResult::Final { text, .. }) => assert_eq!(text, "done"),
        other => panic!("expected final result, got {other:?}"),
    }
}

#[tokio::test]
async fn openai_chat_tool_calls_returns_tool_calls() {
    let result = run_fixture("200 OK", TOOL_CALLS).await;
    match result {
        Ok(RoundResult::ToolCalls { calls, .. }) => {
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].call_id, "call_1");
            assert_eq!(calls[0].tool_name, "workspace_shell");
            assert_eq!(calls[0].arguments, json!({"command": "ls"}));
        }
        other => panic!("expected tool calls result, got {other:?}"),
    }
}

#[tokio::test]
async fn openai_chat_length_returns_length_cap() {
    let result = run_fixture("200 OK", LENGTH).await;
    match result {
        Ok(RoundResult::LengthCap { partial_text, .. }) => {
            assert_eq!(partial_text.as_deref(), Some("partial"));
        }
        other => panic!("expected length cap result, got {other:?}"),
    }
}

#[tokio::test]
async fn openai_chat_auth_status_returns_auth_error() {
    let result = run_fixture("401 Unauthorized", AUTH_401).await;
    assert!(matches!(result, Err(ProviderError::Auth)));

    let result = run_fixture("403 Forbidden", AUTH_401).await;
    assert!(matches!(result, Err(ProviderError::Auth)));
}

#[tokio::test]
async fn openai_chat_rate_limit_status_returns_rate_limited_error() {
    let result = run_fixture("429 Too Many Requests", RATE_LIMIT_429).await;
    assert!(matches!(result, Err(ProviderError::RateLimited { .. })));
}

#[tokio::test]
async fn openai_chat_context_length_400_returns_context_length_error() {
    let result = run_fixture("400 Bad Request", CONTEXT_LENGTH_400).await;
    assert!(matches!(result, Err(ProviderError::ContextLength)));
}

#[tokio::test]
async fn openai_chat_unsupported_finish_returns_deserialize_error() {
    let result = run_fixture("200 OK", UNSUPPORTED_FINISH).await;
    assert!(matches!(result, Err(ProviderError::Deserialize(_))));
}

#[tokio::test]
async fn openai_chat_missing_finish_returns_deserialize_error() {
    let result = run_fixture("200 OK", MISSING_FINISH).await;
    assert!(matches!(result, Err(ProviderError::Deserialize(_))));
}

async fn run_fixture(
    status: &'static str,
    body: &'static str,
) -> Result<RoundResult, ProviderError> {
    let url = spawn_mock(body, status).await;
    let client = OpenAIChatClient {
        http: reqwest::Client::new(),
        base_url: url,
        model_id: "gpt-4.1".into(),
        api_key: "test".into(),
        temperature: Some(0.2),
        max_completion_tokens: Some(256),
    };
    client
        .tool_round(
            &Conversation {
                system_prompt: "system".into(),
                user_seed: "user".into(),
                turns: Vec::new(),
            },
            &[ToolSpec {
                canonical: "workspace_shell".into(),
                provider_safe: "workspace_shell".into(),
                description: "shell".into(),
                input_schema: json!({"type": "object"}),
            }],
            CancellationToken::new(),
        )
        .await
}

async fn spawn_mock(body: &'static str, status: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = [0_u8; 8192];
        let _ = stream.read(&mut request).await.unwrap();
        let response = format!(
            "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(response.as_bytes()).await.unwrap();
    });
    format!("http://{addr}")
}
