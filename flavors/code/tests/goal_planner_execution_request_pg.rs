//! M4 planner handoff: an accepted Goal assigned to a configured
//! planner wakes that planner, and the planner emits a repo-scoped
//! `proxima-code/execution-request-v1` Fact with provenance.

#![allow(clippy::too_many_lines)]

use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use proxima_code::{CommitV1, ExecutionRequestV1, build_engine_with, ingest_commit, register_repo};
use proxima_core::auth::NoAuth;
use proxima_core::llm::{EmbeddingClient, LlmError};
use proxima_core::mcp::McpAuthorContext;
use proxima_core::personality::{
    InstantiatePersonalityRequest, SetWakeEntriesRequest, WakeEntryDraft,
};
use proxima_core::storage::Storage;
use proxima_core::wake::target_adapter::{
    TargetAdapter, TargetAdapterError, TargetInvocation, TargetOutcome, TargetOutcomeKind,
};
use proxima_core::{
    BindInferenceTierRequest, CORE_AUTHORED_RELATION, CORE_DERIVED_FROM_RELATION, FactPayload,
    InferenceTargetConfig, LocalCliConfig, ModelTier, OrgId, Owner, Principal,
    RegisterInferenceTargetRequest, SourceBatchId, UserId, WakeEntryAuthoredBy, WakeEntryGoalScope,
    WakeEntryTriggerKind,
};
use proxima_mcp_server::{DevMcpServer, McpAuthStore};
use proxima_storage_pg::PgStorage;
use sqlx::{Connection, Executor, PgConnection, Row};
use tempfile::TempDir;
use uuid::Uuid;

const ADMIN_URL: &str = "postgres://proxima:proxima@localhost/postgres";
const GOAL_TITLE: &str = "m4 planner request";
const REQUEST_TITLE: &str = "Implement the M4 planner handoff";
const REQUEST_KEY: &str = "m4-planner-request:test-repo";

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
struct ScriptedPlannerAdapter {
    server: DevMcpServer,
    auth_store: Arc<McpAuthStore>,
}

impl ScriptedPlannerAdapter {
    async fn emit_execution_request(&self, invocation: TargetInvocation) -> Result<(), String> {
        let token = invocation
            .env
            .get("PROXIMA_WAKE_TOKEN")
            .ok_or_else(|| "missing PROXIMA_WAKE_TOKEN".to_string())
            .and_then(|raw| Uuid::parse_str(raw).map_err(|err| err.to_string()))?;
        let auth = self
            .auth_store
            .resolve(token)
            .await
            .ok_or_else(|| "wake token did not resolve".to_string())?;
        let planner_root = auth
            .wake
            .as_ref()
            .ok_or_else(|| "missing wake context".to_string())?
            .current_root_perspective_memory_id;
        let author = McpAuthorContext {
            model_id: "scripted-planner".into(),
            client_name: "m4-planner-adapter".into(),
            client_version: "1".into(),
            caller_self_perspective: Some(planner_root),
        };
        let search = self
            .server
            .call_tool(
                "proxima-code/code_search_commits",
                serde_json::json!({
                    "query": "planner seed",
                    "limit": 1
                }),
                author.clone(),
                Some(auth.clone()),
            )
            .await
            .map_err(|err| err.to_string())?;
        let first_commit = search
            .get("commits")
            .and_then(serde_json::Value::as_array)
            .and_then(|items| items.first())
            .ok_or_else(|| format!("no commit match: {search}"))?;
        let commit_handle = first_commit
            .get("handle")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| format!("missing commit handle: {first_commit}"))?;
        let goal_activated_memory = invocation
            .params
            .get("triggering_memory")
            .and_then(|value| value.get("memory_id"))
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "missing triggering_memory.memory_id".to_string())?;

        let output = self
            .server
            .call_tool(
                "proxima-code/code_emit_execution_request",
                serde_json::json!({
                    "repo_handle": "planner-test",
                    "title": REQUEST_TITLE,
                    "instructions": "Make the smallest code change that satisfies the accepted goal.",
                    "request_key": REQUEST_KEY,
                    "idempotency_key": REQUEST_KEY,
                    "goal_activated_memory": goal_activated_memory,
                    "evidence": [commit_handle]
                }),
                author,
                Some(auth),
            )
            .await
            .map_err(|err| err.to_string())?;
        if output.get("handle").is_none() {
            return Err(format!("emit returned no handle: {output}"));
        }
        Ok(())
    }
}

#[async_trait]
impl TargetAdapter for ScriptedPlannerAdapter {
    async fn run(&self, invocation: TargetInvocation) -> Result<TargetOutcome, TargetAdapterError> {
        let started = Instant::now();
        let result = self.emit_execution_request(invocation).await;
        let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let (kind, stderr_tail) = match result {
            Ok(()) => (TargetOutcomeKind::Succeeded, String::new()),
            Err(err) => (TargetOutcomeKind::Failed, err),
        };
        Ok(TargetOutcome {
            kind,
            turn_count: Some(1),
            exit_code: Some(i32::from(!matches!(kind, TargetOutcomeKind::Succeeded))),
            duration_ms,
            stdout_tail: String::new(),
            stderr_tail,
            stdout_truncated: false,
            stderr_truncated: false,
            session_log_error: None,
        })
    }
}

async fn create_db(name: &str) -> Result<(), sqlx::Error> {
    let admin = std::env::var("PROXIMA_TEST_PG_URL").unwrap_or_else(|_| ADMIN_URL.into());
    let mut conn = PgConnection::connect(&admin).await?;
    conn.execute(format!("CREATE DATABASE \"{name}\"").as_str())
        .await?;
    conn.close().await?;
    Ok(())
}

async fn drop_db(name: &str) -> Result<(), sqlx::Error> {
    let admin = std::env::var("PROXIMA_TEST_PG_URL").unwrap_or_else(|_| ADMIN_URL.into());
    let mut conn = PgConnection::connect(&admin).await?;
    conn.execute(format!("DROP DATABASE IF EXISTS \"{name}\"").as_str())
        .await?;
    conn.close().await?;
    Ok(())
}

fn db_url(db_name: &str) -> String {
    let admin = std::env::var("PROXIMA_TEST_PG_URL").unwrap_or_else(|_| ADMIN_URL.into());
    match admin.rfind('/') {
        Some(idx) => format!("{}/{}", &admin[..idx], db_name),
        None => format!("{admin}/{db_name}"),
    }
}

async fn migrated_db() -> Option<(String, PgStorage)> {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if create_db(&db_name).await.is_err() {
        eprintln!("skipping (no admin PG)");
        return None;
    }
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
        eprintln!("skipping (migration failed): {err}");
        drop(pg);
        let _ = drop_db(&db_name).await;
        return None;
    }
    Some((db_name, pg))
}

fn test_owner() -> Owner {
    Owner {
        principal: Principal::User(UserId::new(Uuid::now_v7())),
        org_id: OrgId::new(Uuid::now_v7()),
    }
}

fn init_repo(root: &TempDir) -> Result<std::path::PathBuf, String> {
    let repo = root.path().join("repo");
    std::fs::create_dir(&repo).map_err(|err| err.to_string())?;
    git(&repo, &["init", "-b", "main"])?;
    std::fs::write(repo.join("README.md"), "initial\n").map_err(|err| err.to_string())?;
    git(&repo, &["add", "README.md"])?;
    git(
        &repo,
        &[
            "-c",
            "user.name=Proxima Test",
            "-c",
            "user.email=proxima@example.test",
            "commit",
            "-m",
            "initial",
        ],
    )?;
    Ok(repo)
}

fn git(cwd: &std::path::Path, args: &[&str]) -> Result<String, String> {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|err| err.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[tokio::test(flavor = "multi_thread")]
async fn accepted_goal_wakes_planner_and_emits_execution_request()
-> Result<(), Box<dyn std::error::Error>> {
    let Some((db_name, pg)) = migrated_db().await else {
        return Ok(());
    };

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = test_owner();
        let repo_root = tempfile::tempdir()?;
        let repo_path = init_repo(&repo_root)?;
        let repo_id = Uuid::now_v7();
        register_repo(
            pg.pool(),
            &owner,
            repo_id,
            repo_path.to_str().ok_or("repo path is not valid UTF-8")?,
            "planner-test",
        )
        .await?;

        let now = time::OffsetDateTime::now_utc();
        let commit = ingest_commit(
            pg.pool(),
            &owner,
            SourceBatchId::new(Uuid::now_v7()),
            &CommitV1 {
                repo_id,
                sha: "plannerseed".into(),
                parents: Vec::new(),
                author_name: "Planner".into(),
                author_email: "planner@example.test".into(),
                author_time: now,
                committer_name: "Planner".into(),
                committer_email: "planner@example.test".into(),
                committer_time: now,
                message: "planner seed commit".into(),
            },
            now,
        )
        .await?;

        let engine = Arc::new(
            build_engine_with(
                pg.clone(),
                Box::new(NoAuth::new(owner.principal.clone(), owner.clone())),
                proxima_flavor_goal::register,
            )
            .with_embed(Arc::new(FakeEmbedding)),
        );
        engine
            .set_mcp_url("http://127.0.0.1:1/mcp".to_string())
            .await;

        let mut server_registry = proxima_core::FlavorRegistry::new();
        proxima_flavor_goal::register(&mut server_registry);
        proxima_code::register(&mut server_registry);
        let server = DevMcpServer::from_pool(
            pg.pool().clone(),
            owner.clone(),
            Arc::new(server_registry.freeze()),
        )
        .with_engine(engine.clone());
        let auth_store = Arc::new(McpAuthStore::new(engine.wake_token_store()));
        let master_token = Uuid::now_v7();
        auth_store
            .replace_local_master_token(master_token, owner.clone())
            .await;
        let master_auth = auth_store
            .resolve(master_token)
            .await
            .expect("master token resolves");
        engine
            .set_target_adapter(Arc::new(ScriptedPlannerAdapter {
                server: server.clone(),
                auth_store: auth_store.clone(),
            }))
            .await;

        pg.register_inference_target(&RegisterInferenceTargetRequest {
            owner: owner.clone(),
            target_ref: "test/scripted-planner".into(),
            config: InferenceTargetConfig::LocalCli(LocalCliConfig {
                command: "scripted-planner".into(),
                profile: None,
                env_overrides: Vec::new(),
            }),
        })
        .await?;
        pg.bind_inference_tier(&BindInferenceTierRequest {
            owner: owner.clone(),
            tier: ModelTier::Standard,
            target_ref: "test/scripted-planner".into(),
        })
        .await?;

        let planner = engine
            .instantiate_personality(InstantiatePersonalityRequest {
                owner: owner.clone(),
                display_name: "Configured Planner".into(),
                purpose: "Plan execution requests for accepted goals".into(),
            })
            .await?;
        let mut wake_entry = WakeEntryDraft::new(
            Uuid::now_v7(),
            planner.instance_id,
            WakeEntryTriggerKind::OnMemory,
            "proxima-goal/goal-activated-v1",
            "plan-execution-requests",
            WakeEntryAuthoredBy::Other,
            1000,
            "bundled:proxima-code/plan_execution_requests",
            ModelTier::Standard,
            None,
            vec![
                "proxima-code/code_search_commits".into(),
                "proxima-code/code_emit_execution_request".into(),
            ],
            1,
        )?;
        wake_entry.goal_scope = WakeEntryGoalScope::TriggerGoalAssigned;
        pg.set_wake_entries(&SetWakeEntriesRequest {
            owner: owner.clone(),
            personality_instance_id: planner.instance_id,
            entries: vec![wake_entry.clone()],
        })
        .await?;
        let unassigned_planner = engine
            .instantiate_personality(InstantiatePersonalityRequest {
                owner: owner.clone(),
                display_name: "Unassigned Planner".into(),
                purpose: "Must not wake for goals assigned elsewhere".into(),
            })
            .await?;
        let mut unassigned_wake_entry = WakeEntryDraft::new(
            Uuid::now_v7(),
            unassigned_planner.instance_id,
            WakeEntryTriggerKind::OnMemory,
            "proxima-goal/goal-activated-v1",
            "plan-execution-requests-unassigned",
            WakeEntryAuthoredBy::Other,
            1000,
            "bundled:proxima-code/plan_execution_requests",
            ModelTier::Standard,
            None,
            vec![
                "proxima-code/code_search_commits".into(),
                "proxima-code/code_emit_execution_request".into(),
            ],
            1,
        )?;
        unassigned_wake_entry.goal_scope = WakeEntryGoalScope::TriggerGoalAssigned;
        pg.set_wake_entries(&SetWakeEntriesRequest {
            owner: owner.clone(),
            personality_instance_id: unassigned_planner.instance_id,
            entries: vec![unassigned_wake_entry.clone()],
        })
        .await?;
        let runtime = pg
            .fetch_personality_runtime(&owner, planner.instance_id)
            .await?
            .expect("planner runtime row");
        let planner_root = runtime.current_root_perspective_memory_id;

        let listed = server
            .call_tool(
                "core/list_personalities",
                serde_json::json!({}),
                McpAuthorContext {
                    model_id: "test-model".into(),
                    client_name: "m4-test".into(),
                    client_version: "1".into(),
                    caller_self_perspective: None,
                },
                Some(master_auth.clone()),
            )
            .await?;
        let planner_handle = listed
            .get("personalities")
            .and_then(serde_json::Value::as_array)
            .and_then(|items| {
                items.iter().find_map(|item| {
                    (item.get("display_name")?.as_str()? == "Configured Planner")
                        .then(|| item.get("handle")?.as_str())
                        .flatten()
                })
            })
            .ok_or_else(|| format!("planner handle not listed: {listed}"))?;

        let author = McpAuthorContext {
            model_id: "test-model".into(),
            client_name: "m4-test".into(),
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
                            "text": "Produce a Code execution request for this repo."
                        }
                    },
                    "target_personality": planner_handle,
                    "evidence": [],
                    "idempotency_key": "m4-planner-propose"
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
                    "idempotency_key": "m4-planner-accept"
                }),
                author,
                Some(master_auth),
            )
            .await?;

        let fired = engine.run_dispatcher_tick().await?;
        assert_eq!(fired, 1, "goal activation fires the configured planner");
        let unassigned_invocations: i64 = sqlx::query_scalar(
            "SELECT count(*)
             FROM proxima_core.personality_wake_invocations
             WHERE personality_instance_id = $1
               AND wake_entry_id = $2",
        )
        .bind(unassigned_planner.instance_id.into_inner())
        .bind(unassigned_wake_entry.wake_entry_id)
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(
            unassigned_invocations, 0,
            "assignment-scoped unassigned planner must not record an invocation",
        );

        let invocation = sqlx::query(
            "SELECT status, failure_reason
             FROM proxima_core.personality_wake_invocations
             WHERE personality_instance_id = $1
               AND wake_entry_id = $2",
        )
        .bind(planner.instance_id.into_inner())
        .bind(wake_entry.wake_entry_id)
        .fetch_one(pg.pool())
        .await?;
        let invocation_status: String = invocation.try_get("status")?;
        let failure_reason: Option<String> = invocation.try_get("failure_reason")?;
        assert_eq!(
            invocation_status, "succeeded",
            "planner wake failed: {failure_reason:?}",
        );

        let request = sqlx::query(
            "SELECT m.memory_id, r.repo_id, r.title, r.instructions, r.request_key
             FROM proxima_core.memories m
             JOIN proxima_code.execution_request_v1 r USING (memory_id)
             WHERE m.schema_id = $1",
        )
        .bind(ExecutionRequestV1::SCHEMA_ID)
        .fetch_one(pg.pool())
        .await?;
        let request_memory_id: Uuid = request.try_get("memory_id")?;
        let request_repo_id: Uuid = request.try_get("repo_id")?;
        let request_title: String = request.try_get("title")?;
        let request_key: String = request.try_get("request_key")?;
        assert_eq!(request_repo_id, repo_id);
        assert_eq!(request_title, REQUEST_TITLE);
        assert_eq!(request_key, REQUEST_KEY);

        let activated_fact: Uuid = sqlx::query_scalar(
            "SELECT memory_id
             FROM proxima_goal.goal_activated_v1
             WHERE title = $1",
        )
        .bind(GOAL_TITLE)
        .fetch_one(pg.pool())
        .await?;

        let authored_edges: i64 = sqlx::query_scalar(
            "SELECT count(*)
             FROM proxima_core.edges
             WHERE relation = $1
               AND source_kind = 'Perspective'
               AND source_memory_id = $2
               AND target_kind = 'Fact'
               AND target_memory_id = $3",
        )
        .bind(CORE_AUTHORED_RELATION)
        .bind(planner_root.into_inner())
        .bind(request_memory_id)
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(authored_edges, 1, "planner authors request Fact");

        let goal_edges: i64 = sqlx::query_scalar(
            "SELECT count(*)
             FROM proxima_core.edges
             WHERE relation = $1
               AND source_kind = 'Fact'
               AND source_memory_id = $2
               AND target_kind = 'Fact'
               AND target_memory_id = $3",
        )
        .bind(CORE_DERIVED_FROM_RELATION)
        .bind(request_memory_id)
        .bind(activated_fact)
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(goal_edges, 1, "request derives from goal activation Fact");

        let evidence_edges: i64 = sqlx::query_scalar(
            "SELECT count(*)
             FROM proxima_core.edges
             WHERE relation = $1
               AND source_kind = 'Fact'
               AND source_memory_id = $2
               AND target_kind = 'Fact'
               AND target_memory_id = $3",
        )
        .bind(CORE_DERIVED_FROM_RELATION)
        .bind(request_memory_id)
        .bind(commit.memory_id.into_inner())
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(evidence_edges, 1, "request derives from selected evidence");

        Ok(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db_name).await;
    result
}
