use proxima_harness::conversation::{Conversation, ToolSpec};
use proxima_harness::providers::openai_responses::OpenAIResponsesClient;
use proxima_harness::providers::{ProviderClient, ProviderError, RoundResult};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

const STOP: &str = include_str!("fixtures/openai_responses/stop.json");
const FUNCTION_CALL: &str = include_str!("fixtures/openai_responses/function_call.json");
const INCOMPLETE: &str = include_str!("fixtures/openai_responses/incomplete.json");
const AUTH_401: &str = include_str!("fixtures/openai_responses/auth_401.json");
const CONTEXT_LENGTH_400: &str = include_str!("fixtures/openai_responses/context_length_400.json");

#[tokio::test]
async fn responses_stop_returns_final() {
    let result = run_fixture("200 OK", STOP).await;
    match result {
        Ok(RoundResult::Final { text, .. }) => assert_eq!(text, "Done."),
        other => panic!("expected final result, got {other:?}"),
    }
}

#[tokio::test]
async fn responses_function_call_returns_tool_calls() {
    let result = run_fixture("200 OK", FUNCTION_CALL).await;
    match result {
        Ok(RoundResult::ToolCalls {
            calls,
            raw_assistant,
        }) => {
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].call_id, "fc_1");
            assert_eq!(calls[0].tool_name, "workspace_shell");
            assert_eq!(calls[0].arguments, json!({"command": "ls"}));
            let raw = raw_assistant.raw.expect("raw output");
            assert_eq!(raw[0]["id"], "fc_item_1");
            assert_eq!(raw[0]["status"], "completed");
        }
        other => panic!("expected tool calls result, got {other:?}"),
    }
}

#[tokio::test]
async fn responses_incomplete_returns_length_cap() {
    let result = run_fixture("200 OK", INCOMPLETE).await;
    match result {
        Ok(RoundResult::LengthCap { partial_text, .. }) => {
            assert_eq!(partial_text.as_deref(), Some("Partial..."));
        }
        other => panic!("expected length cap result, got {other:?}"),
    }
}

#[tokio::test]
async fn responses_auth_status_returns_auth_error() {
    let result = run_fixture("401 Unauthorized", AUTH_401).await;
    assert!(matches!(result, Err(ProviderError::Auth)));
}

#[tokio::test]
async fn responses_context_length_400_returns_context_length_error() {
    let result = run_fixture("400 Bad Request", CONTEXT_LENGTH_400).await;
    assert!(matches!(result, Err(ProviderError::ContextLength)));
}

async fn run_fixture(
    status: &'static str,
    body: &'static str,
) -> Result<RoundResult, ProviderError> {
    let url = spawn_mock(body, status).await;
    let client = OpenAIResponsesClient::new(url, "gpt-5-codex".into(), "test".into());
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
