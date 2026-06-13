//! M2 acceptance: a `proxima-goal/goal-activated-v1` Fact wakes a
//! temporary `SubstrateOnly` Executor and the Executor emits one
//! Perspective through the wake-token MCP substrate path.

#![allow(clippy::too_many_lines)]

use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use proxima_code::{CodeDevelopmentPerspectiveV1, build_engine_with};
use proxima_core::harness::{ErrorClass, FinishReason};
use proxima_core::llm::{EmbeddingClient, LlmError};
use proxima_core::mcp::McpAuthorContext;
use proxima_core::personality::{
    InstantiatePersonalityRequest, SetWakeEntriesRequest, WakeEntryDraft,
};
use proxima_core::storage::Storage;
use proxima_core::wake::target_adapter::{
    TargetAdapter, TargetAdapterError, TargetContext, TargetInvocation, TargetOutcome,
    TargetOutcomeKind,
};
use proxima_core::{
    BindInferenceTierRequest, CORE_AUTHORED_RELATION, CORE_DERIVED_FROM_RELATION,
    InferenceTargetConfig, MistralChatConfig, ModelTier, OrgId, Owner, PerspectivePayload,
    Principal, RegisterInferenceTargetRequest, UserId, WakeEntryAuthoredBy, WakeEntryTriggerKind,
};
use proxima_mcp_server::{McpEdgeAuth, McpToolHost};
use proxima_pg_testkit::{create_db, db_url, drop_db, unique_db_name};
use proxima_storage_pg::PgStorage;
use sqlx::Row;
use uuid::Uuid;

const GOAL_TITLE: &str = "m2 wake smoke";
const EMITTED_TEXT: &str = "I would do X.";

#[derive(Debug)]
struct FakeEmbedding;

#[async_trait]
impl EmbeddingClient for FakeEmbedding {
    async fn embed(&self, _text: &str) -> Result<Vec<f32>, LlmError> {
        Ok(vec![0.0; 8])
    }

    fn model_id(&self) -> &'static str {
        "fake-embed"
    }

    fn dim(&self) -> usize {
        8
    }
}

#[derive(Debug, Clone)]
struct ScriptedExecutorAdapter {
    server: McpToolHost,
    auth_store: Arc<McpEdgeAuth>,
}

impl ScriptedExecutorAdapter {
    async fn emit_perspective(&self, ctx: TargetContext) -> Result<(), String> {
        let auth = self
            .auth_store
            .resolve(&format!("pxw_{}", ctx.wake_token))
            .await
            .ok_or_else(|| "wake token did not resolve".to_string())?;
        let output = self
            .server
            .call_tool(
                "core/emit_perspective",
                serde_json::json!({
                    "schema_id": <CodeDevelopmentPerspectiveV1 as PerspectivePayload>::SCHEMA_ID,
                    "schema_version": 1,
                    "payload": {
                        "repo_id": null,
                        "summary": EMITTED_TEXT,
                        "pattern": "goal activation noticed",
                        "risk": "low",
                        "recommended_posture": "plan next step",
                        "confidence": 0.9
                    },
                    "text": EMITTED_TEXT
                }),
                McpAuthorContext {
                    model_id: "scripted-executor".into(),
                    client_name: "m2-smoke-adapter".into(),
                    client_version: "1".into(),
                    caller_self_perspective: Some(ctx.root_perspective_memory_id),
                },
                Some(auth),
            )
            .await
            .map_err(|err| err.to_string())?;
        if let Some(error) = output.get("error").and_then(serde_json::Value::as_str) {
            return Err(error.to_string());
        }
        if output.get("memory").is_none() {
            return Err(format!(
                "emit_perspective returned no memory handle: {output}"
            ));
        }
        Ok(())
    }
}

#[async_trait]
impl TargetAdapter for ScriptedExecutorAdapter {
    async fn run(
        &self,
        _invocation: TargetInvocation,
        ctx: TargetContext,
    ) -> Result<TargetOutcome, TargetAdapterError> {
        let started = Instant::now();
        let result = self.emit_perspective(ctx).await;
        let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let (kind, error_class, failure_reason) = match result {
            Ok(()) => (TargetOutcomeKind::Succeeded, ErrorClass::None, None),
            Err(err) => (
                TargetOutcomeKind::Failed,
                ErrorClass::ToolDispatchFatal,
                Some(err),
            ),
        };
        Ok(TargetOutcome {
            kind,
            finish_reason: FinishReason::Stop,
            error_class,
            failure_reason,
            rounds_used: 1,
            duration_ms,
            total_prompt_tokens: None,
            total_completion_tokens: None,
            tool_call_count: 1,
            jsonl_bytes: br#"{"record":"scripted-executor"}"#.to_vec(),
            jsonl_truncated: false,
            network_log: None,
        })
    }
}

async fn migrated_db() -> Option<(String, PgStorage)> {
    let db_name = unique_db_name("proxima_test");
    create_db(&db_name).await.expect("PG required for tests");
    let pg = PgStorage::connect(&db_url(&db_name))
        .await
        .expect("connect test db");
    if let Err(err) = async {
        pg.run_migrations().await?;
        proxima_code::migrator().run(pg.pool()).await?;
        proxima_flavor_goal::migrator().run(pg.pool()).await?;
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await
    {
        drop(pg);
        let _ = drop_db(&db_name).await;
        panic!("migration failed: {err}");
    }
    Some((db_name, pg))
}

fn test_owner() -> Owner {
    Owner {
        principal: Principal::User(UserId::new(Uuid::now_v7())),
        org_id: OrgId::new(Uuid::now_v7()),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn goal_activated_fact_wakes_substrate_executor_and_emits_perspective()
-> Result<(), Box<dyn std::error::Error>> {
    let Some((db_name, pg)) = migrated_db().await else {
        return Ok(());
    };

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = test_owner();

        let engine = Arc::new(
            build_engine_with(pg.clone(), proxima_flavor_goal::register)
                .with_embed(Arc::new(FakeEmbedding)),
        );
        engine
            .set_mcp_url("http://127.0.0.1:1/mcp".to_string())
            .await;

        let mut server_registry = proxima_core::FlavorRegistry::new();
        proxima_flavor_goal::register(&mut server_registry);
        proxima_code::register(&mut server_registry);
        let server = McpToolHost::from_pool(
            pg.pool().clone(),
            owner.clone(),
            Arc::new(server_registry.freeze()),
        )
        .with_engine(engine.clone());
        let auth_store = Arc::new(McpEdgeAuth::engine_hosted(engine.wake_token_store()));
        let master_token = Uuid::now_v7();
        auth_store
            .replace_local_master_token(master_token, owner.clone())
            .await;
        let master_auth = auth_store
            .resolve(&format!("pxm_{master_token}"))
            .await
            .expect("master token resolves");
        engine
            .set_target_adapter(Arc::new(ScriptedExecutorAdapter {
                server: server.clone(),
                auth_store: auth_store.clone(),
            }))
            .await;

        pg.register_inference_target(&RegisterInferenceTargetRequest {
            owner: owner.clone(),
            target_ref: "test/scripted-executor".into(),
            config: InferenceTargetConfig::MistralChat(MistralChatConfig {
                base_url: "http://127.0.0.1:9".into(),
                model_id: "test-model".into(),
                api_key_env: "PATH".into(),
                temperature: None,
                max_completion_tokens: None,
                reasoning_effort: None,

                context_window_tokens: None,
            }),
        })
        .await?;
        pg.bind_inference_tier(&BindInferenceTierRequest {
            owner: owner.clone(),
            tier: ModelTier::Standard,
            target_ref: "test/scripted-executor".into(),
        })
        .await?;

        let executor = engine
            .instantiate_personality(InstantiatePersonalityRequest {
                owner: owner.clone(),
                display_name: "M2 Executor".into(),
                purpose: "Smoke-test goal activation wake plumbing".into(),
            })
            .await?;
        let wake_entry = WakeEntryDraft::new(
            Uuid::now_v7(),
            executor.instance_id,
            WakeEntryTriggerKind::OnMemory,
            "proxima-goal/goal-activated-v1",
            "goal-activated-smoke",
            WakeEntryAuthoredBy::Other,
            1000,
            ModelTier::Standard,
            None,
            vec!["core/emit_perspective".into()],
            1,
        )?;
        pg.set_wake_entries(&SetWakeEntriesRequest {
            owner: owner.clone(),
            personality_instance_id: executor.instance_id,
            entries: vec![wake_entry.clone()],
        })
        .await?;
        let runtime = pg
            .fetch_personality_runtime(&owner, executor.instance_id)
            .await?
            .expect("executor runtime row");
        let executor_root = runtime.current_root_perspective_memory_id;

        let author = McpAuthorContext {
            model_id: "test-model".into(),
            client_name: "m2-smoke".into(),
            client_version: "1".into(),
            caller_self_perspective: None,
        };
        let proposed = server
            .call_tool(
                "proxima-goal/goal_propose",
                serde_json::json!({
                    "payload": {
                        "schema_id": "proxima-goal/simple-text-v1",
                        "body": {
                            "title": GOAL_TITLE,
                            "text": "Prove M2 wake plumbing."
                        }
                    },
                    "evidence": [],
                    "idempotency_key": "m2-wake-smoke-propose"
                }),
                author.clone(),
                Some(master_auth.clone()),
            )
            .await?;
        let proposal = proposed
            .get("handle")
            .and_then(serde_json::Value::as_str)
            .expect("proposal handle");
        server
            .call_tool(
                "proxima-goal/goal_accept",
                serde_json::json!({
                    "proposal": proposal,
                    "idempotency_key": "m2-wake-smoke-accept"
                }),
                author,
                Some(master_auth),
            )
            .await?;

        let fired = engine.run_dispatcher_tick().await?;
        assert_eq!(fired, 1, "goal-activated Fact fires the Executor wake");

        let invocation_status: proxima_core::WakeInvocationStatus = sqlx::query_scalar(
            "SELECT status
             FROM proxima_core.personality_wake_invocations
             WHERE personality_instance_id = $1
               AND wake_entry_id = $2",
        )
        .bind(executor.instance_id.into_inner())
        .bind(wake_entry.wake_entry_id)
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(
            invocation_status,
            proxima_core::WakeInvocationStatus::Succeeded
        );

        let activated_fact: Uuid = sqlx::query_scalar(
            "SELECT memory_id
             FROM proxima_goal.goal_activated_v1
             WHERE title = $1",
        )
        .bind(GOAL_TITLE)
        .fetch_one(pg.pool())
        .await?;

        let emitted = sqlx::query(
            "SELECT m.memory_id, m.personality_instance_id, d.summary
             FROM proxima_core.memories m
             JOIN proxima_code.development_perspective_v1 d USING (memory_id)
             WHERE m.schema_id = $1
               AND d.summary = $2",
        )
        .bind(<CodeDevelopmentPerspectiveV1 as PerspectivePayload>::SCHEMA_ID)
        .bind(EMITTED_TEXT)
        .fetch_one(pg.pool())
        .await?;
        let emitted_memory_id: Uuid = emitted.try_get("memory_id")?;
        let emitted_author: Option<Uuid> = emitted.try_get("personality_instance_id")?;
        let emitted_summary: String = emitted.try_get("summary")?;
        assert_eq!(emitted_author, Some(executor.instance_id.into_inner()));
        assert_eq!(emitted_summary, EMITTED_TEXT);

        let authored_edges: i64 = sqlx::query_scalar(
            "SELECT count(*)
             FROM proxima_core.edges
             WHERE relation = $1
               AND source_kind = 'Perspective'
               AND source_memory_id = $2
               AND target_kind = 'Perspective'
               AND target_memory_id = $3",
        )
        .bind(CORE_AUTHORED_RELATION)
        .bind(executor_root.into_inner())
        .bind(emitted_memory_id)
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(
            authored_edges, 1,
            "Executor Root Perspective authors emitted Perspective"
        );

        let provenance_edges: i64 = sqlx::query_scalar(
            "SELECT count(*)
             FROM proxima_core.edges
             WHERE relation = $1
               AND source_kind = 'Perspective'
               AND source_memory_id = $2
               AND target_kind = 'Fact'
               AND target_memory_id = $3",
        )
        .bind(CORE_DERIVED_FROM_RELATION)
        .bind(emitted_memory_id)
        .bind(activated_fact)
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(
            provenance_edges, 1,
            "emitted Perspective derives from the goal-activated Fact"
        );

        Ok(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db_name).await;
    result
}
