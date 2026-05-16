//! M4 planner handoff: an accepted Goal assigned to a configured
//! planner wakes that planner, and the planner emits a repo-scoped
//! `proxima-code/execution-request-v1` Fact with provenance.

#![allow(clippy::too_many_lines)]

use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use proxima_code::{CommitV1, ExecutionRequestV1, build_engine_with, ingest_commit, register_repo};
use proxima_core::auth::NoAuth;
use proxima_core::harness::{ErrorClass, FinishReason};
use proxima_core::llm::{EmbeddingClient, LlmError};
use proxima_core::mcp::McpAuthorContext;
use proxima_core::personality::{
    InstantiatePersonalityRequest, SetWakeEntriesRequest, TombstonePersonalityRequest,
    WakeEntryDraft,
};
use proxima_core::storage::Storage;
use proxima_core::verbs::event_ingest::{CitationMappingHint, CitedObjectHint, EventDraft};
use proxima_core::wake::target_adapter::{
    TargetAdapter, TargetAdapterError, TargetContext, TargetInvocation, TargetOutcome,
    TargetOutcomeKind,
};
use proxima_core::{
    BindInferenceTierRequest, CORE_AUTHORED_RELATION, CORE_DERIVED_FROM_RELATION,
    EdgeAuthorshipKind, EntityKind, FactPayload, FlavorRegistry, FlavorRegistryFrozen,
    InferenceTargetConfig, MemoryId, MistralChatConfig, ModelTier, OrgId, Owner, Principal,
    RegisterInferenceTargetRequest, SchemaId, SchemaVersion, SourceBatchId, SourceId, UserId,
    WakeEntryAuthoredBy, WakeEntryGoalScope, WakeEntryTriggerKind,
};
use proxima_mcp_server::{McpAuthStore, McpToolHost};
use proxima_storage_pg::PgStorage;
use proxima_storage_pg::verbs::edge_append::{EdgeDraft, append_edge_in_tx};
use proxima_storage_pg::verbs::event_ingest::ingest_event_in_tx;
use sqlx::{Connection, Executor, PgConnection, Row};
use tempfile::TempDir;
use uuid::Uuid;

const ADMIN_URL: &str = "postgres://proxima:proxima@localhost/postgres";
const GOAL_TITLE: &str = "m4 planner request";
const REQUEST_TITLE: &str = "Implement the M4 planner handoff";
const REQUEST_KEY: &str = "m4-planner-request:test-repo";
const EXECUTION_REQUEST_SOURCE_ID: &str = "proxima-code/execution-request";
const EXECUTION_REQUEST_OBJECT_SCHEMA: &str = "proxima-code/execution-request-object-v1";
const EXECUTION_REQUEST_WHOLE_SCHEMA: &str = "proxima-code/execution-request-whole-v1";

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
    server: McpToolHost,
    auth_store: Arc<McpAuthStore>,
}

impl ScriptedPlannerAdapter {
    async fn emit_execution_request(
        &self,
        _invocation: TargetInvocation,
        ctx: TargetContext,
    ) -> Result<(), String> {
        let auth = self
            .auth_store
            .resolve(ctx.wake_token)
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
        let wake = auth
            .wake
            .as_ref()
            .ok_or_else(|| "missing wake context".to_string())?;
        let goal_activated_memory = wake
            .handles
            .assign_memory(wake.triggering_event_memory_id)
            .as_str()
            .to_string();

        let output = self
            .server
            .call_tool(
                "proxima-code/code_emit_execution_request",
                serde_json::json!({
                    "repo_handle": "planner-test",
                    "title": REQUEST_TITLE,
                    "instructions": "Make the smallest code change that satisfies the accepted goal.",
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
    async fn run(
        &self,
        invocation: TargetInvocation,
        ctx: TargetContext,
    ) -> Result<TargetOutcome, TargetAdapterError> {
        let started = Instant::now();
        let result = self.emit_execution_request(invocation, ctx).await;
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
            tool_call_count: 2,
            jsonl_bytes: br#"{"record":"scripted-planner"}"#.to_vec(),
            jsonl_truncated: false,
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

fn code_registry() -> Arc<FlavorRegistryFrozen> {
    let mut registry = FlavorRegistry::new();
    proxima_flavor_goal::register(&mut registry);
    proxima_code::register(&mut registry);
    Arc::new(registry.freeze())
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

#[allow(clippy::too_many_arguments)]
async fn seed_execution_request(
    pg: &PgStorage,
    owner: &Owner,
    registry: &FlavorRegistryFrozen,
    repo_id: Uuid,
    request_key: &str,
    title: &str,
    instructions: &str,
    evidence: &[MemoryId],
) -> Result<MemoryId, Box<dyn std::error::Error>> {
    let payload = ExecutionRequestV1 {
        repo_id,
        title: title.into(),
        instructions: instructions.into(),
        request_key: request_key.into(),
    };
    let mut payload_bytes = Vec::new();
    ciborium::ser::into_writer(&payload, &mut payload_bytes)?;
    let content_hash = blake3::hash(&payload_bytes);
    let observed_at = time::OffsetDateTime::now_utc();
    let draft = EventDraft {
        source_id: SourceId::new(EXECUTION_REQUEST_SOURCE_ID),
        source_batch_id: SourceBatchId::new(Uuid::now_v7()),
        owner: owner.clone(),
        schema_id: SchemaId::new(ExecutionRequestV1::SCHEMA_ID.into()),
        schema_version: SchemaVersion::new(ExecutionRequestV1::SCHEMA_VERSION),
        payload: payload_bytes,
        observed_at,
        occurred_at: observed_at,
        cited_object: CitedObjectHint {
            schema_id: SchemaId::new(EXECUTION_REQUEST_OBJECT_SCHEMA.into()),
            schema_version: SchemaVersion::new(1),
            content_hash: *content_hash.as_bytes(),
        },
        citation_mapping: CitationMappingHint {
            schema_id: SchemaId::new(EXECUTION_REQUEST_WHOLE_SCHEMA.into()),
            schema_version: SchemaVersion::new(1),
        },
    };
    let mut tx = pg.pool().begin().await?;
    let outcome = ingest_event_in_tx(&mut tx, &draft).await?;
    sqlx::query(
        "INSERT INTO proxima_code.execution_request_v1
            (memory_id, repo_id, title, instructions, request_key)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(outcome.memory_id.into_inner())
    .bind(repo_id)
    .bind(title)
    .bind(instructions)
    .bind(request_key)
    .execute(&mut *tx)
    .await?;
    let relation = registry
        .resolve_relation(CORE_DERIVED_FROM_RELATION)
        .expect("core/derived-from registered");
    for target in evidence {
        append_edge_in_tx(
            &mut tx,
            &EdgeDraft {
                edge_id: Uuid::now_v7(),
                relation,
                source_kind: EntityKind::Fact,
                source_memory_id: Some(outcome.memory_id.into_inner()),
                source_goal_id: None,
                target_kind: EntityKind::Fact,
                target_memory_id: Some(target.into_inner()),
                target_goal_id: None,
                authorship_kind: EdgeAuthorshipKind::ExternalAgent,
                authorship_owner_memory_id: None,
                owner,
            },
            None,
        )
        .await?;
    }
    tx.commit().await?;
    Ok(outcome.memory_id)
}

async fn configure_execution_worker(
    pg: &PgStorage,
    owner: &Owner,
    worker: proxima_core::PersonalityInstanceId,
) -> Result<WakeEntryDraft, Box<dyn std::error::Error>> {
    pg.register_inference_target(&RegisterInferenceTargetRequest {
        owner: owner.clone(),
        target_ref: "test/retry-worker".into(),
        config: InferenceTargetConfig::MistralChat(MistralChatConfig {
            base_url: "http://127.0.0.1:9".into(),
            model_id: "test-model".into(),
            api_key_env: "PATH".into(),
            temperature: None,
            max_completion_tokens: None,
        }),
    })
    .await?;
    pg.bind_inference_tier(&BindInferenceTierRequest {
        owner: owner.clone(),
        tier: ModelTier::Standard,
        target_ref: "test/retry-worker".into(),
    })
    .await?;
    let mut wake_entry = WakeEntryDraft::new(
        Uuid::now_v7(),
        worker,
        WakeEntryTriggerKind::OnMemory,
        ExecutionRequestV1::SCHEMA_ID,
        "execution-worker",
        WakeEntryAuthoredBy::Any,
        1000,
        ModelTier::Standard,
        None,
        Vec::new(),
        1,
    )?;
    wake_entry.execution_mode = proxima_core::WakeExecutionMode::Workspace;
    wake_entry.workspace_tool_palette = vec!["proxima-workspace/shell".into()];
    pg.set_wake_entries(&SetWakeEntriesRequest {
        owner: owner.clone(),
        personality_instance_id: worker,
        entries: vec![wake_entry.clone()],
    })
    .await?;
    Ok(wake_entry)
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
async fn shell_author_retries_execution_request_with_target_and_provenance()
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
            "retry-test",
        )
        .await?;
        let now = time::OffsetDateTime::now_utc();
        let commit = ingest_commit(
            pg.pool(),
            &owner,
            SourceBatchId::new(Uuid::now_v7()),
            &CommitV1 {
                repo_id,
                sha: "retryseed".into(),
                parents: Vec::new(),
                author_name: "Retry".into(),
                author_email: "retry@example.test".into(),
                author_time: now,
                committer_name: "Retry".into(),
                committer_email: "retry@example.test".into(),
                committer_time: now,
                message: "retry seed commit".into(),
            },
            now,
        )
        .await?;

        let registry = code_registry();
        let prior = seed_execution_request(
            &pg,
            &owner,
            &registry,
            repo_id,
            "prior-request",
            "Original execution request",
            "Make the original change.",
            &[commit.memory_id],
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
        let server = McpToolHost::from_pool(pg.pool().clone(), owner.clone(), registry.clone())
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

        let worker = engine
            .instantiate_personality(InstantiatePersonalityRequest {
                owner: owner.clone(),
                display_name: "Retry Worker".into(),
                purpose: "Execute retry requests".into(),
            })
            .await?;
        let wake_entry = configure_execution_worker(&pg, &owner, worker.instance_id).await?;
        let worker_runtime = pg
            .fetch_personality_runtime(&owner, worker.instance_id)
            .await?
            .expect("worker runtime row");

        let author = McpAuthorContext {
            model_id: "test-model".into(),
            client_name: "retry-test".into(),
            client_version: "1".into(),
            caller_self_perspective: None,
        };
        let listed = server
            .call_tool(
                "core/list_personalities",
                serde_json::json!({}),
                author.clone(),
                Some(master_auth.clone()),
            )
            .await?;
        let worker_handle = listed
            .get("personalities")
            .and_then(serde_json::Value::as_array)
            .and_then(|items| {
                items.iter().find_map(|item| {
                    (item.get("display_name")?.as_str()? == "Retry Worker")
                        .then(|| item.get("personality")?.as_str())
                        .flatten()
                })
            })
            .ok_or_else(|| format!("worker handle not listed: {listed}"))?;

        let output = server
            .call_tool(
                "proxima-code/code_retry_execution_request",
                serde_json::json!({
                    "prior_execution_request": prior.into_inner().to_string(),
                    "target_personality": worker_handle,
                    "idempotency_key": "retry-request",
                    "instructions_append": "Retry after shell-author review.",
                    "evidence": [commit.memory_id.into_inner().to_string()]
                }),
                author.clone(),
                Some(master_auth.clone()),
            )
            .await?;
        assert_eq!(
            output
                .get("idempotent_replay")
                .and_then(serde_json::Value::as_bool),
            Some(false)
        );
        assert!(
            output
                .get("authored_edge_handle")
                .and_then(serde_json::Value::as_str)
                .is_some()
        );
        assert!(
            output
                .get("target_edge_handle")
                .and_then(serde_json::Value::as_str)
                .is_some()
        );
        assert_eq!(
            output
                .get("derived_edge_handles")
                .and_then(serde_json::Value::as_array)
                .map(Vec::len),
            Some(2),
            "retry should derive from prior request plus deduped commit evidence",
        );

        let request = sqlx::query(
            "SELECT memory_id, title, instructions
             FROM proxima_code.execution_request_v1
             WHERE repo_id = $1 AND request_key = 'retry-request'",
        )
        .bind(repo_id)
        .fetch_one(pg.pool())
        .await?;
        let retry_memory_id: Uuid = request.try_get("memory_id")?;
        let retry_title: String = request.try_get("title")?;
        let retry_instructions: String = request.try_get("instructions")?;
        assert_eq!(retry_title, "Original execution request");
        assert!(retry_instructions.contains(&prior.into_inner().to_string()));
        assert!(retry_instructions.contains("retry_key: retry-request"));
        assert!(retry_instructions.contains("Retry after shell-author review."));

        let shell_author_root: Uuid = sqlx::query_scalar(
            "SELECT p.current_root_perspective_memory_id
             FROM proxima_core.master_token_personality mtp
             JOIN proxima_core.personality p
               ON p.personality_instance_id = mtp.personality_instance_id
             WHERE mtp.master_token_id = $1",
        )
        .bind(master_token)
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
        .bind(shell_author_root)
        .bind(retry_memory_id)
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(authored_edges, 1, "shell-author authors retry Fact");

        let target_edges: i64 = sqlx::query_scalar(
            "SELECT count(*)
             FROM proxima_core.edges
             WHERE relation = $1
               AND source_kind = 'Perspective'
               AND source_memory_id = $2
               AND target_kind = 'Fact'
               AND target_memory_id = $3",
        )
        .bind(proxima_code::mcp::CODE_TARGETS_EXECUTION_REQUEST_RELATION)
        .bind(
            worker_runtime
                .current_root_perspective_memory_id
                .into_inner(),
        )
        .bind(retry_memory_id)
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(target_edges, 1, "target worker root points at retry Fact");

        for target in [prior.into_inner(), commit.memory_id.into_inner()] {
            let derived_edges: i64 = sqlx::query_scalar(
                "SELECT count(*)
                 FROM proxima_core.edges
                 WHERE relation = $1
                   AND source_kind = 'Fact'
                   AND source_memory_id = $2
                   AND target_kind = 'Fact'
                   AND target_memory_id = $3",
            )
            .bind(CORE_DERIVED_FROM_RELATION)
            .bind(retry_memory_id)
            .bind(target)
            .fetch_one(pg.pool())
            .await?;
            assert_eq!(derived_edges, 1, "missing derived edge to {target}");
        }

        let eligible_wake: bool = sqlx::query_scalar(
            "SELECT EXISTS(
                 SELECT 1
                 FROM proxima_core.change_event ce
                 JOIN proxima_core.personality_wake_entries we
                   ON we.personality_instance_id = $1
                  AND we.wake_entry_id = $2
                  AND we.tombstoned_at IS NULL
                  AND we.enabled
                  AND we.execution_mode = 'workspace'
                  AND we.trigger_kind = 'on_memory'
                  AND we.trigger_id = ce.entity_schema_id
                 WHERE ce.kind = 'EntityAppend'
                   AND ce.entity_kind = 'Fact'
                   AND ce.entity_memory_id = $3
             )",
        )
        .bind(worker.instance_id.into_inner())
        .bind(wake_entry.wake_entry_id)
        .bind(retry_memory_id)
        .fetch_one(pg.pool())
        .await?;
        assert!(eligible_wake, "retry Fact must match worker wake path");

        let replay = server
            .call_tool(
                "proxima-code/code_retry_execution_request",
                serde_json::json!({
                    "prior_execution_request": prior.into_inner().to_string(),
                    "target_personality": worker_handle,
                    "idempotency_key": "retry-request"
                }),
                author,
                Some(master_auth.clone()),
            )
            .await?;
        assert_eq!(
            replay
                .get("idempotent_replay")
                .and_then(serde_json::Value::as_bool),
            Some(true)
        );
        assert!(
            replay
                .get("authored_edge_handle")
                .is_some_and(serde_json::Value::is_null)
        );
        assert!(
            replay
                .get("target_edge_handle")
                .is_some_and(serde_json::Value::is_null)
        );
        let replay_with_stale_target = server
            .call_tool(
                "proxima-code/code_retry_execution_request",
                serde_json::json!({
                    "prior_execution_request": prior.into_inner().to_string(),
                    "target_personality": Uuid::now_v7().to_string(),
                    "idempotency_key": "retry-request"
                }),
                McpAuthorContext {
                    model_id: "test-model".into(),
                    client_name: "retry-test".into(),
                    client_version: "1".into(),
                    caller_self_perspective: None,
                },
                Some(master_auth),
            )
            .await?;
        assert_eq!(
            replay_with_stale_target
                .get("idempotent_replay")
                .and_then(serde_json::Value::as_bool),
            Some(true),
            "duplicate retry key should replay before target revalidation",
        );
        let retry_rows: i64 = sqlx::query_scalar(
            "SELECT count(*)
             FROM proxima_code.execution_request_v1
             WHERE repo_id = $1 AND request_key = 'retry-request'",
        )
        .bind(repo_id)
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(
            retry_rows, 1,
            "duplicate retry key must not duplicate request"
        );

        Ok(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test(flavor = "multi_thread")]
async fn shell_author_retry_rejects_invalid_call_shapes() -> Result<(), Box<dyn std::error::Error>>
{
    let Some((db_name, pg)) = migrated_db().await else {
        return Ok(());
    };

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = test_owner();
        let repo_id = Uuid::now_v7();
        register_repo(
            pg.pool(),
            &owner,
            repo_id,
            "/tmp/retry-rejections",
            "retry-rejections",
        )
        .await?;
        let now = time::OffsetDateTime::now_utc();
        let commit = ingest_commit(
            pg.pool(),
            &owner,
            SourceBatchId::new(Uuid::now_v7()),
            &CommitV1 {
                repo_id,
                sha: "rejectseed".into(),
                parents: Vec::new(),
                author_name: "Reject".into(),
                author_email: "reject@example.test".into(),
                author_time: now,
                committer_name: "Reject".into(),
                committer_email: "reject@example.test".into(),
                committer_time: now,
                message: "reject seed commit".into(),
            },
            now,
        )
        .await?;
        let registry = code_registry();
        let prior = seed_execution_request(
            &pg,
            &owner,
            &registry,
            repo_id,
            "prior-reject",
            "Prior rejection request",
            "Prior request body.",
            &[commit.memory_id],
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
        let server = McpToolHost::from_pool(pg.pool().clone(), owner.clone(), registry.clone())
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

        let valid_worker = engine
            .instantiate_personality(InstantiatePersonalityRequest {
                owner: owner.clone(),
                display_name: "Valid Retry Worker".into(),
                purpose: "Has execution-request wake".into(),
            })
            .await?;
        configure_execution_worker(&pg, &owner, valid_worker.instance_id).await?;
        let no_wake_worker = engine
            .instantiate_personality(InstantiatePersonalityRequest {
                owner: owner.clone(),
                display_name: "No Wake Worker".into(),
                purpose: "Missing execution-request wake".into(),
            })
            .await?;
        let tombstoned_worker = engine
            .instantiate_personality(InstantiatePersonalityRequest {
                owner: owner.clone(),
                display_name: "Tombstoned Worker".into(),
                purpose: "Inactive target".into(),
            })
            .await?;
        pg.tombstone_personality(&TombstonePersonalityRequest {
            owner: owner.clone(),
            personality_instance_id: tombstoned_worker.instance_id,
        })
        .await?;

        let author = McpAuthorContext {
            model_id: "test-model".into(),
            client_name: "retry-rejections".into(),
            client_version: "1".into(),
            caller_self_perspective: None,
        };
        let base_args = |key: &str, prior_memory: MemoryId, target: Uuid| -> serde_json::Value {
            serde_json::json!({
                "prior_execution_request": prior_memory.into_inner().to_string(),
                "target_personality": target.to_string(),
                "idempotency_key": key
            })
        };

        let non_master = server
            .call_tool(
                "proxima-code/code_retry_execution_request",
                base_args(
                    "reject-non-master",
                    prior,
                    valid_worker.instance_id.into_inner(),
                ),
                author.clone(),
                None,
            )
            .await
            .expect_err("non-master retry must reject");
        assert!(non_master.to_string().contains("master-token"));

        let wrong_prior = server
            .call_tool(
                "proxima-code/code_retry_execution_request",
                base_args(
                    "reject-wrong-prior",
                    commit.memory_id,
                    valid_worker.instance_id.into_inner(),
                ),
                author.clone(),
                Some(master_auth.clone()),
            )
            .await
            .expect_err("non execution-request prior must reject");
        assert!(
            wrong_prior
                .to_string()
                .contains("must be a proxima-code/execution-request-v1 Fact")
        );

        let missing_target = server
            .call_tool(
                "proxima-code/code_retry_execution_request",
                base_args("reject-missing-target", prior, Uuid::now_v7()),
                author.clone(),
                Some(master_auth.clone()),
            )
            .await
            .expect_err("missing target must reject");
        assert!(
            missing_target
                .to_string()
                .contains("target_personality not found")
        );

        let inactive_target = server
            .call_tool(
                "proxima-code/code_retry_execution_request",
                base_args(
                    "reject-inactive-target",
                    prior,
                    tombstoned_worker.instance_id.into_inner(),
                ),
                author.clone(),
                Some(master_auth.clone()),
            )
            .await
            .expect_err("inactive target must reject");
        assert!(inactive_target.to_string().contains("not active"));

        let missing_wake = server
            .call_tool(
                "proxima-code/code_retry_execution_request",
                base_args(
                    "reject-missing-wake",
                    prior,
                    no_wake_worker.instance_id.into_inner(),
                ),
                author,
                Some(master_auth),
            )
            .await
            .expect_err("target without execution wake must reject");
        assert!(
            missing_wake
                .to_string()
                .contains("no enabled workspace wake entry")
        );

        Ok(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db_name).await;
    result
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
        let server = McpToolHost::from_pool(
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
            config: InferenceTargetConfig::MistralChat(MistralChatConfig {
                base_url: "http://127.0.0.1:9".into(),
                model_id: "test-model".into(),
                api_key_env: "PATH".into(),
                temperature: None,
                max_completion_tokens: None,
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
                        .then(|| item.get("personality")?.as_str())
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
        let invocation_status: proxima_core::WakeInvocationStatus =
            invocation.try_get("status")?;
        let failure_reason: Option<String> = invocation.try_get("failure_reason")?;
        assert_eq!(
            invocation_status,
            proxima_core::WakeInvocationStatus::Succeeded,
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
