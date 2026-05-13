use std::path::PathBuf;

use proxima_codex_auth::AuthDotJsonPath;
use proxima_harness::conversation::Conversation;
use proxima_harness::providers::chatgpt_codex::ChatGPTCodexClient;
use proxima_harness::providers::{ProviderClient, RoundResult};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

const FINAL_TEXT_SSE: &str = include_str!("fixtures/chatgpt_codex_final_text.sse");

async fn write_auth_json(tmp: &tempfile::TempDir) -> PathBuf {
    let auth_path = tmp.path().join(".codex/auth.json");
    tokio::fs::create_dir_all(auth_path.parent().unwrap())
        .await
        .unwrap();
    // Minimal stub that the resolver can read; access_token is a JWT
    // whose `chatgpt_account_id` claim parses out to "acct-test".
    // Pre-baked fixture (no real secrets).
    let body = include_str!("fixtures/chatgpt_codex_auth.json");
    tokio::fs::write(&auth_path, body).await.unwrap();
    auth_path
}

async fn spawn_mock(body: &'static str, status: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 4096];
        let _ = socket.read(&mut buf).await.unwrap();
        let resp = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Type: application/json\r\n\r\n{body}",
            body.len()
        );
        socket.write_all(resp.as_bytes()).await.unwrap();
    });
    format!("http://{}", addr)
}

#[tokio::test]
async fn final_text_round_returns_final() {
    let tmp = tempfile::tempdir().unwrap();
    let auth_path = write_auth_json(&tmp).await;
    let base_url = spawn_mock(FINAL_TEXT_SSE, "200 OK").await;

    let client = ChatGPTCodexClient::new(
        base_url,
        "gpt-5.5".into(),
        AuthDotJsonPath::from_explicit(auth_path),
    );
    let conv = Conversation {
        system_prompt: "you are a helpful assistant".into(),
        user_seed: "hello".into(),
        turns: vec![],
    };
    let result = client
        .tool_round(&conv, &[], CancellationToken::new())
        .await
        .expect("round ok");

    match result {
        RoundResult::Final { text, .. } => assert!(!text.is_empty()),
        other => panic!("expected Final, got {other:?}"),
    }
}
