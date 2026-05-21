use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use proxima_core::auth::NoAuth;
use proxima_core::harness::{
    ErrorClass, FinishReason, HarnessAdapter, HarnessContext, HarnessOutcomeKind, HarnessProgram,
    HarnessToolDispatch, HarnessToolProjection, ProviderTarget,
};
use proxima_core::mcp::{
    HarnessSubstrateBridge, HarnessSubstrateCall, HarnessSubstrateError, HarnessSubstrateToolSpec,
    provider_safe_tool_name,
};
use proxima_core::personality::PersonalityInstanceId;
use proxima_core::verbs::query::MemoryStore;
use proxima_core::{Engine, FlavorRegistry, MemoryId, OrgId, Owner, Principal, UserId};
use proxima_harness::HarnessLoop;
use proxima_harness::conversation::{AssistantTurn, Conversation, ToolCall, ToolSpec};
use proxima_harness::providers::{ProviderClient, ProviderError, RoundResult};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

#[derive(Default)]
struct StubProvider {
    round: AtomicUsize,
}

#[async_trait]
impl ProviderClient for StubProvider {
    async fn tool_round(
        &self,
        _conversation: &Conversation,
        _tools: &[ToolSpec],
        _cancel: CancellationToken,
    ) -> Result<RoundResult, ProviderError> {
        let round = self.round.fetch_add(1, Ordering::SeqCst);
        Ok(match round {
            0 => RoundResult::ToolCalls {
                calls: vec![ToolCall {
                    call_id: "call_0".into(),
                    tool_name: "workspace_list_files".into(),
                    arguments: json!({"path": ".", "recursive": false}),
                }],
                raw_assistant: AssistantTurn::default(),
            },
            _ => RoundResult::Final {
                text: "Done.".into(),
                raw_assistant: AssistantTurn {
                    text: "Done.".into(),
                    ..Default::default()
                },
            },
        })
    }
}

#[tokio::test]
async fn stub_provider_returns_two_rounds() {
    let provider = StubProvider::default();
    let conversation = Conversation {
        system_prompt: "test".into(),
        user_seed: "go".into(),
        turns: vec![],
    };
    let tools = Vec::new();

    let first = provider
        .tool_round(&conversation, &tools, CancellationToken::new())
        .await
        .unwrap();
    assert!(matches!(first, RoundResult::ToolCalls { .. }));

    let second = provider
        .tool_round(&conversation, &tools, CancellationToken::new())
        .await
        .unwrap();
    assert!(matches!(second, RoundResult::Final { .. }));
}

#[tokio::test]
async fn harness_stops_after_typed_emit_fulfills_required_schema() {
    let canonical = "core/emit_abstraction::test/derivation-v1::v1";
    let base_url = spawn_sequence(vec![tool_call_body(
        "call_emit",
        &provider_safe_tool_name(canonical),
        json!({"title": "done"}),
    )])
    .await;
    let loop_ = harness_loop(BridgeMode::Ok);
    let outcome = loop_
        .run(
            program(
                base_url,
                vec![typed_emit_projection(canonical, "test/derivation-v1")],
                vec![canonical.into()],
                0,
            ),
            hctx(),
        )
        .await
        .expect("harness run");

    assert_eq!(outcome.kind, HarnessOutcomeKind::Succeeded);
    assert_eq!(outcome.finish_reason, FinishReason::Fulfilled);
    assert_eq!(outcome.error_class, ErrorClass::None);
    let jsonl = String::from_utf8(outcome.jsonl_bytes).expect("jsonl utf8");
    assert!(jsonl.contains("\"record\":\"fulfillment_satisfied\""));
}

#[tokio::test]
async fn harness_ignores_intermediate_producer_until_required_schema() {
    let intermediate = "test/intermediate_tool";
    let final_tool = "test/final_tool";
    let base_url = spawn_sequence(vec![
        tool_call_body(
            "call_intermediate",
            &provider_safe_tool_name(intermediate),
            json!({}),
        ),
        tool_call_body(
            "call_final",
            &provider_safe_tool_name(final_tool),
            json!({}),
        ),
    ])
    .await;
    let loop_ = harness_loop(BridgeMode::Ok);
    let outcome = loop_
        .run(
            program_with_required(
                base_url,
                vec![
                    direct_projection(intermediate, vec!["test/intermediate-v1".into()]),
                    direct_projection(final_tool, vec!["test/final-v1".into()]),
                ],
                vec![intermediate.into(), final_tool.into()],
                vec!["test/final-v1".into()],
                0,
            ),
            hctx(),
        )
        .await
        .expect("harness run");

    assert_eq!(outcome.kind, HarnessOutcomeKind::Succeeded);
    assert_eq!(outcome.finish_reason, FinishReason::Fulfilled);
    assert_eq!(outcome.rounds_used, 2);
    let jsonl = String::from_utf8(outcome.jsonl_bytes).expect("jsonl utf8");
    assert!(jsonl.contains("\"tool_name\":\"test/final_tool\""));
    assert!(!jsonl.contains("\"tool_name\":\"test/intermediate_tool\",\"produced_schema_ids\""));
}

#[tokio::test]
async fn harness_fails_after_repeated_identical_tool_error() {
    let canonical = "test/failing_tool";
    let base_url = spawn_sequence(vec![
        tool_call_body("call_1", &provider_safe_tool_name(canonical), json!({})),
        tool_call_body("call_2", &provider_safe_tool_name(canonical), json!({})),
        tool_call_body("call_3", &provider_safe_tool_name(canonical), json!({})),
    ])
    .await;
    let loop_ = harness_loop(BridgeMode::RecoverableError("same failure".into()));
    let outcome = loop_
        .run(
            program(
                base_url,
                vec![direct_projection(canonical, Vec::new())],
                vec![canonical.into()],
                0,
            ),
            hctx(),
        )
        .await
        .expect("harness run");

    assert_eq!(outcome.kind, HarnessOutcomeKind::Failed);
    assert_eq!(outcome.error_class, ErrorClass::ToolErrorStreak);
    assert!(
        outcome
            .failure_reason
            .as_deref()
            .unwrap_or_default()
            .contains("tool_error_streak")
    );
}

#[tokio::test]
async fn harness_fails_when_durable_fulfillment_stalls() {
    let producing = "core/emit_abstraction::test/derivation-v1::v1";
    let read_only = "core/fetch_memory";
    let bodies = (0..16)
        .map(|idx| {
            tool_call_body(
                &format!("call_{idx}"),
                &provider_safe_tool_name(read_only),
                json!({"memory": "F1"}),
            )
        })
        .collect();
    let base_url = spawn_sequence(bodies).await;
    let loop_ = harness_loop(BridgeMode::Ok);
    let outcome = loop_
        .run(
            program(
                base_url,
                vec![
                    typed_emit_projection(producing, "test/derivation-v1"),
                    direct_projection(read_only, Vec::new()),
                ],
                vec![producing.into(), read_only.into()],
                0,
            ),
            hctx(),
        )
        .await
        .expect("harness run");

    assert_eq!(outcome.kind, HarnessOutcomeKind::Failed);
    assert_eq!(outcome.error_class, ErrorClass::FulfillmentStalled);
    assert_eq!(outcome.rounds_used, 16);
    let jsonl = String::from_utf8(outcome.jsonl_bytes).expect("jsonl utf8");
    assert!(jsonl.contains("\"record\":\"fulfillment_reminder\""));
    assert!(jsonl.contains("\"record\":\"fulfillment_stalled\""));
}

#[derive(Clone)]
enum BridgeMode {
    Ok,
    RecoverableError(String),
}

#[derive(Clone)]
struct TestBridge {
    mode: BridgeMode,
}

#[async_trait]
impl HarnessSubstrateBridge for TestBridge {
    fn list_harness_tools(&self, palette: &[String]) -> Vec<HarnessSubstrateToolSpec> {
        let mut out = Vec::new();
        for id in palette {
            let canonical = id
                .split_once("::")
                .map_or(id.as_str(), |(base, _)| base)
                .to_string();
            if out
                .iter()
                .any(|tool: &HarnessSubstrateToolSpec| tool.canonical_name == canonical)
            {
                continue;
            }
            out.push(HarnessSubstrateToolSpec {
                canonical_name: canonical,
                description: "test tool".into(),
                args_schema: json!({"type": "object"}),
            });
        }
        out
    }

    async fn call_harness_tool(
        &self,
        _call: HarnessSubstrateCall,
    ) -> Result<serde_json::Value, HarnessSubstrateError> {
        match &self.mode {
            BridgeMode::Ok => Ok(json!({"memory": "A1"})),
            BridgeMode::RecoverableError(message) => {
                Err(HarnessSubstrateError::Tool(message.clone()))
            }
        }
    }
}

fn harness_loop(mode: BridgeMode) -> HarnessLoop {
    let owner = owner();
    let engine = Arc::new(Engine::new(
        FlavorRegistry::new().freeze(),
        MemoryStore::new(),
        Box::new(NoAuth::new(owner.principal.clone(), owner)),
    ));
    HarnessLoop {
        engine,
        substrate_bridge: Arc::new(TestBridge { mode }),
        jsonl_cap_bytes: 128 * 1024,
    }
}

fn owner() -> Owner {
    Owner {
        principal: Principal::User(UserId::new(Uuid::now_v7())),
        org_id: OrgId::new(Uuid::now_v7()),
    }
}

fn hctx() -> HarnessContext {
    HarnessContext {
        owner: owner(),
        invocation_id: Uuid::now_v7(),
        wake_entry_id: Uuid::now_v7(),
        personality_instance_id: PersonalityInstanceId::new(Uuid::now_v7()),
        change_event_seq: Uuid::now_v7(),
        root_perspective_memory_id: MemoryId::new(Uuid::now_v7()),
        wake_token: Uuid::now_v7(),
        invocation_timeout: Duration::from_secs(10),
    }
}

fn program(
    base_url: String,
    tool_projection: Vec<HarnessToolProjection>,
    substrate_tool_palette: Vec<String>,
    max_rounds: u32,
) -> HarnessProgram {
    program_with_required(
        base_url,
        tool_projection,
        substrate_tool_palette,
        Vec::new(),
        max_rounds,
    )
}

fn program_with_required(
    base_url: String,
    tool_projection: Vec<HarnessToolProjection>,
    substrate_tool_palette: Vec<String>,
    required_fulfillment_schema_ids: Vec<String>,
    max_rounds: u32,
) -> HarnessProgram {
    HarnessProgram {
        system_prompt: "system".into(),
        instructions: "do work".into(),
        context_params: HashMap::new(),
        tool_projection,
        required_fulfillment_schema_ids,
        substrate_tool_palette,
        workspace_root: None,
        workspace_tool_palette: Vec::new(),
        max_rounds,
        provider: ProviderTarget::OpenAIChat {
            base_url,
            model_id: "gpt-4.1".into(),
            api_key: "test".into(),
            temperature: None,
            max_completion_tokens: None,
            context_window_tokens: None,
        },
    }
}

fn typed_emit_projection(canonical: &str, schema_id: &str) -> HarnessToolProjection {
    HarnessToolProjection {
        palette_id: canonical.into(),
        canonical_name: canonical.into(),
        provider_name: provider_safe_tool_name(canonical),
        description: "Emit test derivation".into(),
        produces_schema_ids: vec![schema_id.into()],
        input_schema: json!({
            "type": "object",
            "additionalProperties": false,
            "properties": { "title": { "type": "string" } },
            "required": ["title"]
        }),
        dispatch: HarnessToolDispatch::TypedEmit {
            internal_canonical_name: "core/emit_abstraction".into(),
            schema_id: schema_id.into(),
            schema_version: 1,
            payload_kind: proxima_core::verbs::schema::PayloadKind::Abstraction,
        },
    }
}

fn direct_projection(canonical: &str, produces_schema_ids: Vec<String>) -> HarnessToolProjection {
    HarnessToolProjection {
        palette_id: canonical.into(),
        canonical_name: canonical.into(),
        provider_name: provider_safe_tool_name(canonical),
        description: "Direct test tool".into(),
        produces_schema_ids,
        input_schema: json!({"type": "object"}),
        dispatch: HarnessToolDispatch::DirectSubstrate {
            internal_canonical_name: canonical.into(),
        },
    }
}

fn tool_call_body(call_id: &str, tool_name: &str, args: serde_json::Value) -> String {
    json!({
        "id": format!("chatcmpl_{call_id}"),
        "object": "chat.completion",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": call_id,
                    "type": "function",
                    "function": {
                        "name": tool_name,
                        "arguments": args.to_string()
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }]
    })
    .to_string()
}

async fn spawn_sequence(bodies: Vec<String>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        for body in bodies {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 8192];
            let _ = stream.read(&mut request).await.unwrap();
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        }
    });
    format!("http://{addr}")
}
