use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use proxima_core::auth::NoAuth;
use proxima_core::harness::{
    HarnessAdapter, HarnessContext, HarnessOutcomeKind, HarnessProgram, ProviderTarget,
};
use proxima_core::mcp::{
    HarnessSubstrateBridge, HarnessSubstrateCall, HarnessSubstrateError, HarnessSubstrateToolSpec,
};
use proxima_core::personality::PersonalityInstanceId;
use proxima_core::verbs::query::MemoryStore;
use proxima_core::{Engine, FlavorRegistry, MemoryId, OrgId, Owner, Principal, UserId};
use proxima_harness::HarnessLoop;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use uuid::Uuid;

const FINAL_TEXT_SSE: &str = include_str!("fixtures/chatgpt_codex_final_text.sse");
const AUTH_JSON: &str = include_str!("fixtures/chatgpt_codex_auth.json");

#[derive(Debug)]
struct EmptyBridge;

#[async_trait]
impl HarnessSubstrateBridge for EmptyBridge {
    fn list_harness_tools(&self, _palette: &[String]) -> Vec<HarnessSubstrateToolSpec> {
        Vec::new()
    }

    async fn call_harness_tool(
        &self,
        _call: HarnessSubstrateCall,
    ) -> Result<serde_json::Value, HarnessSubstrateError> {
        Err(HarnessSubstrateError::ToolNotFound("no tools".into()))
    }
}

fn owner() -> Owner {
    Owner {
        principal: Principal::User(UserId::new(Uuid::now_v7())),
        org_id: OrgId::new(Uuid::now_v7()),
    }
}

async fn write_auth_json(tmp: &tempfile::TempDir) -> PathBuf {
    let auth_path = tmp.path().join(".codex/auth.json");
    tokio::fs::create_dir_all(auth_path.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(&auth_path, AUTH_JSON).await.unwrap();
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
    format!("http://{addr}")
}

#[tokio::test]
async fn codex_wake_round_completes_without_provider_not_yet_supported() {
    let tmp = tempfile::tempdir().unwrap();
    let auth_json = write_auth_json(&tmp).await;
    let base_url = spawn_mock(FINAL_TEXT_SSE, "200 OK").await;
    let owner = owner();
    let engine = Arc::new(Engine::new(
        FlavorRegistry::new().freeze(),
        MemoryStore::new(),
        Box::new(NoAuth::new(owner.principal.clone(), owner.clone())),
    ));
    let loop_ = HarnessLoop {
        engine,
        substrate_bridge: Arc::new(EmptyBridge),
        jsonl_cap_bytes: 64 * 1024,
    };

    let program = HarnessProgram {
        system_prompt: "system".into(),
        instructions: "reply".into(),
        context_params: HashMap::new(),
        tool_projection: Vec::new(),
        substrate_tool_palette: Vec::new(),
        workspace_root: None,
        max_rounds: 1,
        provider: ProviderTarget::ChatGPTCodex {
            base_url,
            model_id: "gpt-5.5".into(),
            reasoning_effort: None,
            auth_json,
        },
    };
    let ctx = HarnessContext {
        owner,
        invocation_id: Uuid::now_v7(),
        wake_entry_id: Uuid::now_v7(),
        personality_instance_id: PersonalityInstanceId::new(Uuid::now_v7()),
        change_event_seq: Uuid::now_v7(),
        root_perspective_memory_id: MemoryId::new(Uuid::now_v7()),
        wake_token: Uuid::now_v7(),
        invocation_timeout: Duration::from_secs(5),
    };

    let outcome = loop_.run(program, ctx).await.expect("harness run");
    assert_eq!(outcome.kind, HarnessOutcomeKind::Succeeded);
    assert!(
        !outcome
            .failure_reason
            .as_deref()
            .unwrap_or_default()
            .contains("provider_not_yet_supported")
    );
}
