//! Ignored live Mistral wake-dispatch coverage.
//!
//! Run explicitly:
//!
//! ```sh
//! PROXIMA_LIVE_MISTRAL=1 cargo test -p proxima-code --test mistral_wake_pg -- --ignored --test-threads=1
//! ```

#![allow(clippy::too_many_lines)]

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use proxima_code::{
    CodeChunkV1, CommitV1, ExecutionRequestV1, FileRevisionV1, FileState, WorkspaceReviewFinding,
    WorkspaceReviewV1, WorkspaceReviewVerdict, build_engine_with, ingest_code_chunk, ingest_commit,
    ingest_file_revision, register_repo,
};
use proxima_core::auth::NoAuth;
use proxima_core::llm::{EmbeddingClient, LlmError};
use proxima_core::mcp::McpAuthorContext;
use proxima_core::personality::{
    InstantiatePersonalityRequest, SetWakeEntriesRequest, WakeEntryDraft,
};
use proxima_core::storage::Storage;
use proxima_core::verbs::event_ingest::{CitationMappingHint, CitedObjectHint, EventDraft};
use proxima_core::{
    BindInferenceTierRequest, CORE_DERIVED_FROM_RELATION, CORE_WORKSPACE_RUN_OBJECT_SCHEMA,
    CORE_WORKSPACE_RUN_SOURCE_ID, CORE_WORKSPACE_RUN_WHOLE_SCHEMA,
    CoreWorkspaceDiffFile as WorkspaceDiffFile, CoreWorkspaceDiffStat as WorkspaceDiffStat,
    CoreWorkspaceRunV1, Credentials, EdgeAuthorshipKind, EntityKind, FactPayload, FlavorRegistry,
    InferenceTargetConfig, MemoryId, MistralChatConfig, ModelTier, OrgId, Owner, Principal,
    RegisterInferenceTargetRequest, SchemaId, SchemaVersion, SourceBatchId, SourceId, UserId,
    WakeEntryAuthoredBy, WakeEntryGoalScope, WakeEntryTriggerKind, WakeExecutionMode,
    WakeInvocationStatus, WakeTraceOutcomeKind,
};
use proxima_harness::HarnessLoop;
use proxima_mcp_server::McpToolHost;
use proxima_storage_pg::PgStorage;
use proxima_storage_pg::verbs::edge_append::{EdgeDraft, append_edge_in_tx};
use proxima_storage_pg::verbs::event_ingest::ingest_event_in_tx;
use serde::Deserialize;
use sqlx::{Connection, Executor, PgConnection, Row};
use tempfile::TempDir;
use uuid::Uuid;

const ADMIN_URL: &str = "postgres://proxima:proxima@localhost/postgres";
const TARGET_REF: &str = "test/mistral-medium-3.5";
const MODEL_ID: &str = "mistral-medium-3.5";
const REPO_HANDLE: &str = "live-mistral";

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
struct LiveMistralConfig {
    base_url: String,
    api_key_env: String,
    temperature: Option<f32>,
}

#[derive(Debug, Deserialize)]
struct VibeConfig {
    providers: Vec<VibeProvider>,
    models: Vec<VibeModel>,
}

#[derive(Debug, Deserialize)]
struct VibeProvider {
    name: String,
    api_base: String,
    api_key_env_var: String,
}

#[derive(Debug, Deserialize)]
struct VibeModel {
    name: String,
    provider: String,
    temperature: Option<f32>,
}

struct LiveWakeWorld {
    db_name: String,
    pg: PgStorage,
    owner: Owner,
    engine: Arc<proxima_core::Engine>,
    server: McpToolHost,
    config: LiveMistralConfig,
    tmp: TempDir,
}

struct CodeSeed {
    repo_id: Uuid,
    repo_path: PathBuf,
    head_sha: String,
    commit_memory: MemoryId,
}

impl LiveWakeWorld {
    async fn new() -> Result<Option<Self>, Box<dyn std::error::Error>> {
        let Some(config) = live_mistral_config()? else {
            return Ok(None);
        };

        let db_name = format!("proxima_live_mistral_{}", Uuid::now_v7().simple());
        create_db(&db_name).await?;
        let pg = PgStorage::connect(&db_url(&db_name)).await?;
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
            return Err(err);
        }

        let owner = test_owner();
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

        let server = McpToolHost::from_pool(pg.pool().clone(), owner.clone(), registry())
            .with_engine(engine.clone());
        let harness = HarnessLoop::new(engine.clone(), Arc::new(server.clone()));
        engine.set_target_adapter(Arc::new(harness)).await;

        let world = Self {
            db_name,
            pg,
            owner,
            engine,
            server,
            config,
            tmp: tempfile::tempdir()?,
        };
        world.register_live_target().await?;
        Ok(Some(world))
    }

    async fn register_live_target(&self) -> Result<(), Box<dyn std::error::Error>> {
        self.pg
            .register_inference_target(&RegisterInferenceTargetRequest {
                owner: self.owner.clone(),
                target_ref: TARGET_REF.into(),
                config: InferenceTargetConfig::MistralChat(MistralChatConfig {
                    base_url: self.config.base_url.clone(),
                    model_id: MODEL_ID.into(),
                    api_key_env: self.config.api_key_env.clone(),
                    temperature: self.config.temperature,
                    max_completion_tokens: Some(2048),
                    reasoning_effort: Some("high".into()),

                    context_window_tokens: None,
                }),
            })
            .await?;
        for tier in [ModelTier::Fast, ModelTier::Standard, ModelTier::Deep] {
            self.pg
                .bind_inference_tier(&BindInferenceTierRequest {
                    owner: self.owner.clone(),
                    tier,
                    target_ref: TARGET_REF.into(),
                })
                .await?;
        }
        Ok(())
    }

    async fn cleanup(self) {
        drop(self.pg);
        let _ = drop_db(&self.db_name).await;
    }

    async fn seed_repo_and_code(&self) -> Result<CodeSeed, Box<dyn std::error::Error>> {
        let repo_path = self.tmp.path().join(format!("repo-{}", Uuid::now_v7()));
        std::fs::create_dir(&repo_path)?;
        git(&repo_path, &["init", "-b", "main"])?;
        std::fs::write(
            repo_path.join("README.md"),
            "live mistral wake coverage\nimportant function marker\n",
        )?;
        git(&repo_path, &["add", "README.md"])?;
        git(
            &repo_path,
            &[
                "-c",
                "user.name=Proxima Test",
                "-c",
                "user.email=proxima@example.test",
                "commit",
                "-m",
                "feat: live mistral wake coverage",
            ],
        )?;
        let head_sha = git(&repo_path, &["rev-parse", "HEAD"])?;
        let repo_id = Uuid::now_v7();
        register_repo(
            self.pg.pool(),
            &self.owner,
            repo_id,
            repo_path.to_str().expect("utf8 repo"),
            REPO_HANDLE,
        )
        .await?;

        let now = time::OffsetDateTime::now_utc();
        let batch = SourceBatchId::new(Uuid::now_v7());
        let content_hash = *blake3::hash(b"live mistral wake coverage").as_bytes();
        let commit = CommitV1 {
            repo_id,
            sha: head_sha.clone(),
            parents: Vec::new(),
            author_name: "Proxima Test".into(),
            author_email: "proxima@example.test".into(),
            author_time: now,
            committer_name: "Proxima Test".into(),
            committer_email: "proxima@example.test".into(),
            committer_time: now,
            message: "feat: live mistral wake coverage".into(),
        };
        let commit_outcome =
            ingest_commit(self.pg.pool(), &self.owner, batch, &commit, now).await?;
        let file = FileRevisionV1 {
            repo_id,
            file_path: "README.md".into(),
            language: Some("markdown".into()),
            content_sha256: content_hash,
            size_bytes: 48,
            indexed_commit_sha: head_sha.clone(),
            state: FileState::Present,
        };
        ingest_file_revision(self.pg.pool(), &self.owner, batch, &file, now).await?;
        let chunk = CodeChunkV1 {
            repo_id,
            file_path: "README.md".into(),
            chunk_index: 0,
            text: "live mistral wake coverage\nimportant function marker\n".into(),
            language: Some("markdown".into()),
            chunk_type: "document".into(),
            byte_range_start: 0,
            byte_range_end: 48,
            line_range_start: 1,
            line_range_end: 2,
            state: FileState::Present,
        };
        ingest_code_chunk(
            self.pg.pool(),
            &self.owner,
            batch,
            &chunk,
            content_hash,
            now,
        )
        .await?;

        Ok(CodeSeed {
            repo_id,
            repo_path,
            head_sha,
            commit_memory: commit_outcome.memory_id,
        })
    }

    async fn add_live_personality_wake(
        &self,
        display_name: &str,
        trigger_kind: WakeEntryTriggerKind,
        trigger_id: &str,
        palette: Vec<&str>,
        instructions: String,
        options: WakeOptions,
    ) -> Result<(proxima_core::PersonalityInstanceId, Uuid), Box<dyn std::error::Error>> {
        let inst = self
            .engine
            .instantiate_personality(InstantiatePersonalityRequest {
                owner: self.owner.clone(),
                display_name: display_name.into(),
                purpose: "Live Mistral wake integration coverage".into(),
            })
            .await?;
        let wake_entry_id = Uuid::now_v7();
        let mut wake = WakeEntryDraft::new(
            wake_entry_id,
            inst.instance_id,
            trigger_kind,
            trigger_id,
            format!("{display_name} wake"),
            options.authored_by,
            options.probability_promille,
            options.model_tier,
            options.inference_target_ref,
            palette.into_iter().map(str::to_string).collect(),
            options.max_rounds,
        )?;
        wake.enabled = options.enabled;
        wake.execution_mode = options.execution_mode;
        wake.goal_scope = options.goal_scope;
        wake.instructions = instructions;
        wake.workspace_tool_palette = options
            .workspace_tool_palette
            .into_iter()
            .map(str::to_string)
            .collect();
        self.engine
            .set_wake_entries(
                &Credentials::None,
                &SetWakeEntriesRequest {
                    owner: self.owner.clone(),
                    personality_instance_id: inst.instance_id,
                    entries: vec![wake],
                },
            )
            .await?;
        Ok((inst.instance_id, wake_entry_id))
    }

    async fn set_wakes(
        &self,
        instance_id: proxima_core::PersonalityInstanceId,
        entries: Vec<WakeEntryDraft>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.engine
            .set_wake_entries(
                &Credentials::None,
                &SetWakeEntriesRequest {
                    owner: self.owner.clone(),
                    personality_instance_id: instance_id,
                    entries,
                },
            )
            .await?;
        Ok(())
    }

    async fn activate_goal_for(
        &self,
        instance_id: proxima_core::PersonalityInstanceId,
        title: &str,
    ) -> Result<MemoryId, Box<dyn std::error::Error>> {
        let proposed = self
            .server
            .call_tool(
                "proxima-goal/goal_propose",
                serde_json::json!({
                    "payload": {
                        "schema_id": "proxima-goal/simple-text-v1",
                        "body": {
                            "title": title,
                            "text": "Live Mistral wake coverage goal."
                        }
                    },
                    "target_personality": instance_id.into_inner().to_string(),
                    "evidence": [],
                    "idempotency_key": format!("live-propose-{title}")
                }),
                setup_author(),
                None,
            )
            .await?;
        let proposal = proposed
            .get("handle")
            .and_then(serde_json::Value::as_str)
            .expect("proposal handle");
        self.server
            .call_tool(
                "proxima-goal/goal_accept",
                serde_json::json!({
                    "proposal": proposal,
                    "target_personality": instance_id.into_inner().to_string(),
                    "idempotency_key": format!("live-accept-{title}")
                }),
                setup_author(),
                None,
            )
            .await?;
        let memory_id: Uuid = sqlx::query_scalar(
            "SELECT memory_id
             FROM proxima_goal.goal_activated_v1
             WHERE title = $1
             ORDER BY accepted_at DESC
             LIMIT 1",
        )
        .bind(title)
        .fetch_one(self.pg.pool())
        .await?;
        Ok(MemoryId::new(memory_id))
    }

    async fn append_edge_event(
        &self,
        source: MemoryId,
        target: MemoryId,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let relation = self
            .engine
            .registry()
            .resolve_relation(CORE_DERIVED_FROM_RELATION)
            .expect("core/derived-from registered");
        let mut tx = self.pg.pool().begin().await?;
        append_edge_in_tx(
            &mut tx,
            &EdgeDraft {
                edge_id: Uuid::now_v7(),
                relation,
                source_kind: EntityKind::Perspective,
                source_memory_id: Some(source.into_inner()),
                source_goal_id: None,
                target_kind: EntityKind::Fact,
                target_memory_id: Some(target.into_inner()),
                target_goal_id: None,
                authorship_kind: EdgeAuthorshipKind::ExternalAgent,
                authorship_owner_memory_id: None,
                owner: &self.owner,
            },
            None,
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn root_memory(
        &self,
        instance_id: proxima_core::PersonalityInstanceId,
    ) -> Result<MemoryId, Box<dyn std::error::Error>> {
        let runtime = self
            .pg
            .fetch_personality_runtime(&self.owner, instance_id)
            .await?
            .expect("personality runtime");
        Ok(runtime.current_root_perspective_memory_id)
    }

    async fn assert_invocation_succeeded(
        &self,
        instance_id: proxima_core::PersonalityInstanceId,
        wake_entry_id: Uuid,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let row = sqlx::query(
            "SELECT status, failure_reason
             FROM proxima_core.personality_wake_invocations
             WHERE personality_instance_id = $1
               AND wake_entry_id = $2
             ORDER BY started_at DESC
             LIMIT 1",
        )
        .bind(instance_id.into_inner())
        .bind(wake_entry_id)
        .fetch_optional(self.pg.pool())
        .await?;
        let Some(row) = row else {
            panic!(
                "missing wake invocation\n{}",
                self.diagnostics(instance_id, wake_entry_id).await?
            );
        };
        let status: WakeInvocationStatus = row.try_get("status")?;
        if status != WakeInvocationStatus::Succeeded {
            let failure: Option<String> = row.try_get("failure_reason")?;
            panic!(
                "wake invocation failed: status={status:?} failure={failure:?}\n{}",
                self.diagnostics(instance_id, wake_entry_id).await?
            );
        }
        Ok(())
    }

    async fn diagnostics(
        &self,
        instance_id: proxima_core::PersonalityInstanceId,
        wake_entry_id: Uuid,
    ) -> Result<String, Box<dyn std::error::Error>> {
        let invocations = sqlx::query(
            "SELECT status, failure_reason, turn_count, stdout_tail, stderr_tail
             FROM proxima_core.personality_wake_invocations
             WHERE personality_instance_id = $1
               AND wake_entry_id = $2
             ORDER BY started_at DESC
             LIMIT 5",
        )
        .bind(instance_id.into_inner())
        .bind(wake_entry_id)
        .fetch_all(self.pg.pool())
        .await?;
        let mut out = String::from("invocations:\n");
        for row in invocations {
            let status: WakeInvocationStatus = row.try_get("status")?;
            let failure: Option<String> = row.try_get("failure_reason")?;
            let turns: Option<i32> = row.try_get("turn_count")?;
            let stdout: Option<String> = row.try_get("stdout_tail")?;
            let stderr: Option<String> = row.try_get("stderr_tail")?;
            out.push_str(&format!(
                "- status={status:?} turns={turns:?} failure={failure:?} stdout={stdout:?} stderr={stderr:?}\n"
            ));
        }

        let traces = sqlx::query(
            "SELECT wt.invocation_id, wt.model_id, wt.outcome_kind, wt.failure_reason,
                    wt.rounds_used, wt.tool_call_count, cj.body
             FROM proxima_core.wake_trace_v1 wt
             JOIN proxima_core.memories m ON m.memory_id = wt.memory_id
             JOIN proxima_core.citation_mappings cm ON cm.memory_id = m.memory_id
             JOIN proxima_core.cited_wake_trace_jsonl_v1 cj
               ON cj.cited_object_id = cm.cited_object_id
             WHERE wt.personality_instance_id = $1
               AND wt.wake_entry_id = $2
             ORDER BY wt.started_at DESC
             LIMIT 2",
        )
        .bind(instance_id.into_inner())
        .bind(wake_entry_id)
        .fetch_all(self.pg.pool())
        .await?;
        out.push_str("wake traces:\n");
        for row in traces {
            let invocation_id: Uuid = row.try_get("invocation_id")?;
            let model_id: String = row.try_get("model_id")?;
            let outcome: WakeTraceOutcomeKind = row.try_get("outcome_kind")?;
            let failure: Option<String> = row.try_get("failure_reason")?;
            let rounds: i32 = row.try_get("rounds_used")?;
            let tools: i32 = row.try_get("tool_call_count")?;
            let body: Vec<u8> = row.try_get("body")?;
            let first_lines = String::from_utf8_lossy(&body)
                .lines()
                .take(12)
                .collect::<Vec<_>>()
                .join("\n");
            out.push_str(&format!(
                "- invocation={invocation_id} model={model_id} outcome={outcome:?} rounds={rounds} tools={tools} failure={failure:?}\n{first_lines}\n"
            ));
        }
        Ok(out)
    }

    async fn trace_model_ids(
        &self,
        instances: &[proxima_core::PersonalityInstanceId],
    ) -> Result<Vec<String>, Box<dyn std::error::Error>> {
        let rows = sqlx::query(
            "SELECT model_id
             FROM proxima_core.wake_trace_v1
             WHERE personality_instance_id = ANY($1)
             ORDER BY started_at ASC",
        )
        .bind(
            instances
                .iter()
                .map(|id| id.into_inner())
                .collect::<Vec<_>>(),
        )
        .fetch_all(self.pg.pool())
        .await?;
        rows.into_iter()
            .map(|row| row.try_get("model_id").map_err(Into::into))
            .collect()
    }

    async fn assert_jsonl_successful_tools(
        &self,
        instance_id: proxima_core::PersonalityInstanceId,
        wake_entry_id: Uuid,
        expected: &[&str],
    ) -> Result<(), Box<dyn std::error::Error>> {
        let body: Vec<u8> = sqlx::query_scalar(
            "SELECT cj.body
             FROM proxima_core.wake_trace_v1 wt
             JOIN proxima_core.memories m ON m.memory_id = wt.memory_id
             JOIN proxima_core.citation_mappings cm ON cm.memory_id = m.memory_id
             JOIN proxima_core.cited_wake_trace_jsonl_v1 cj
               ON cj.cited_object_id = cm.cited_object_id
             WHERE wt.personality_instance_id = $1
               AND wt.wake_entry_id = $2
             ORDER BY wt.started_at DESC
             LIMIT 1",
        )
        .bind(instance_id.into_inner())
        .bind(wake_entry_id)
        .fetch_one(self.pg.pool())
        .await?;
        let mut call_tool: HashMap<String, String> = HashMap::new();
        let mut ok_tools = HashSet::new();
        for line in String::from_utf8_lossy(&body).lines() {
            let value: serde_json::Value = serde_json::from_str(line)?;
            match value.get("record").and_then(serde_json::Value::as_str) {
                Some("tool_call") => {
                    if let (Some(call_id), Some(tool_name)) = (
                        value.get("call_id").and_then(serde_json::Value::as_str),
                        value.get("tool_name").and_then(serde_json::Value::as_str),
                    ) {
                        call_tool.insert(call_id.to_string(), tool_name.to_string());
                    }
                }
                Some("tool_result") => {
                    let is_ok = value
                        .get("status")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|status| status == "ok");
                    if is_ok
                        && let Some(call_id) =
                            value.get("call_id").and_then(serde_json::Value::as_str)
                        && let Some(tool) = call_tool.get(call_id)
                    {
                        ok_tools.insert(tool.clone());
                    }
                }
                _ => {}
            }
        }
        for tool in expected {
            assert!(
                ok_tools.contains(*tool),
                "missing successful tool_result for {tool}; got {ok_tools:?}\n{}",
                self.diagnostics(instance_id, wake_entry_id).await?
            );
        }
        Ok(())
    }
}

#[derive(Clone)]
struct WakeOptions {
    enabled: bool,
    execution_mode: WakeExecutionMode,
    authored_by: WakeEntryAuthoredBy,
    probability_promille: u16,
    goal_scope: WakeEntryGoalScope,
    model_tier: ModelTier,
    inference_target_ref: Option<String>,
    workspace_tool_palette: Vec<&'static str>,
    max_rounds: u16,
}

impl Default for WakeOptions {
    fn default() -> Self {
        Self {
            enabled: true,
            execution_mode: WakeExecutionMode::SubstrateOnly,
            authored_by: WakeEntryAuthoredBy::Other,
            probability_promille: 1000,
            goal_scope: WakeEntryGoalScope::None,
            model_tier: ModelTier::Standard,
            inference_target_ref: None,
            workspace_tool_palette: Vec::new(),
            max_rounds: 4,
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live Mistral API call; set PROXIMA_LIVE_MISTRAL=1"]
async fn live_mistral_fast_standard_deep_resolve_same_model()
-> Result<(), Box<dyn std::error::Error>> {
    let Some(world) = LiveWakeWorld::new().await? else {
        return Ok(());
    };
    let result = async {
        let mut instances = Vec::new();
        for (tier, label) in [
            (ModelTier::Fast, "fast"),
            (ModelTier::Standard, "standard"),
            (ModelTier::Deep, "deep"),
        ] {
            let (instance, _wake) = world
                .add_live_personality_wake(
                    &format!("tier {label}"),
                    WakeEntryTriggerKind::OnMemory,
                    CommitV1::SCHEMA_ID,
                    vec!["core/emit_perspective"],
                    emit_perspective_instruction(&format!("tier {label} perspective")),
                    WakeOptions {
                        model_tier: tier,
                        max_rounds: 3,
                        ..WakeOptions::default()
                    },
                )
                .await?;
            instances.push(instance);
        }
        world.seed_repo_and_code().await?;
        assert_eq!(world.engine.run_dispatcher_tick().await?, 3);
        let model_ids = world.trace_model_ids(&instances).await?;
        assert_eq!(model_ids, vec![MODEL_ID, MODEL_ID, MODEL_ID]);
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    world.cleanup().await;
    result
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live Mistral API call; set PROXIMA_LIVE_MISTRAL=1"]
async fn live_mistral_wake_config_inputs_and_guards() -> Result<(), Box<dyn std::error::Error>> {
    let Some(world) = LiveWakeWorld::new().await? else {
        return Ok(());
    };
    let result = async {
        let (target_instance, target_wake) = world
            .add_live_personality_wake(
                "explicit target",
                WakeEntryTriggerKind::OnMemory,
                CommitV1::SCHEMA_ID,
                vec!["core/emit_perspective"],
                emit_perspective_instruction("explicit target path"),
                WakeOptions {
                    inference_target_ref: Some(TARGET_REF.into()),
                    max_rounds: 3,
                    ..WakeOptions::default()
                },
            )
            .await?;
        world
            .add_live_personality_wake(
                "disabled",
                WakeEntryTriggerKind::OnMemory,
                CommitV1::SCHEMA_ID,
                vec!["core/emit_perspective"],
                emit_perspective_instruction("disabled"),
                WakeOptions {
                    enabled: false,
                    ..WakeOptions::default()
                },
            )
            .await?;
        world
            .add_live_personality_wake(
                "probability zero",
                WakeEntryTriggerKind::OnMemory,
                CommitV1::SCHEMA_ID,
                vec!["core/emit_perspective"],
                emit_perspective_instruction("probability zero"),
                WakeOptions {
                    probability_promille: 0,
                    ..WakeOptions::default()
                },
            )
            .await?;
        world
            .add_live_personality_wake(
                "self author external no match",
                WakeEntryTriggerKind::OnMemory,
                CommitV1::SCHEMA_ID,
                vec!["core/emit_perspective"],
                emit_perspective_instruction("self author"),
                WakeOptions {
                    authored_by: WakeEntryAuthoredBy::SelfAuthor,
                    ..WakeOptions::default()
                },
            )
            .await?;
        let (edge_instance, edge_wake) = world
            .add_live_personality_wake(
                "edge trigger",
                WakeEntryTriggerKind::OnEdge,
                CORE_DERIVED_FROM_RELATION,
                vec!["core/emit_perspective"],
                emit_perspective_instruction("edge trigger"),
                WakeOptions::default(),
            )
            .await?;
        let seed = world.seed_repo_and_code().await?;
        let edge_root = world.root_memory(edge_instance).await?;
        world
            .append_edge_event(edge_root, seed.commit_memory)
            .await?;
        assert_eq!(world.engine.run_dispatcher_tick().await?, 1);
        world
            .assert_invocation_succeeded(target_instance, target_wake)
            .await?;
        let edge_invocations: i64 = sqlx::query_scalar(
            "SELECT count(*)
             FROM proxima_core.personality_wake_invocations
             WHERE personality_instance_id = $1
               AND wake_entry_id = $2",
        )
        .bind(edge_instance.into_inner())
        .bind(edge_wake)
        .fetch_one(world.pg.pool())
        .await?;
        assert_eq!(
            edge_invocations, 0,
            "OnEdge validates but current dispatcher cannot assemble an edge-triggered memory"
        );

        let (goal_instance, goal_wake) = world
            .add_live_personality_wake(
                "assigned goal scoped",
                WakeEntryTriggerKind::OnMemory,
                "proxima-goal/goal-activated-v1",
                vec!["core/emit_perspective"],
                emit_perspective_instruction("assigned goal scoped"),
                WakeOptions {
                    goal_scope: WakeEntryGoalScope::TriggerGoalAssigned,
                    authored_by: WakeEntryAuthoredBy::Any,
                    ..WakeOptions::default()
                },
            )
            .await?;
        world
            .activate_goal_for(goal_instance, "live-mistral-assigned-goal")
            .await?;
        assert_eq!(world.engine.run_dispatcher_tick().await?, 1);
        world
            .assert_invocation_succeeded(goal_instance, goal_wake)
            .await?;

        let (workspace_instance, workspace_wake) = world
            .add_live_personality_wake(
                "workspace mode",
                WakeEntryTriggerKind::OnMemory,
                ExecutionRequestV1::SCHEMA_ID,
                Vec::new(),
                "Do not call tools. Reply with exactly: done".into(),
                WakeOptions {
                    execution_mode: WakeExecutionMode::Workspace,
                    workspace_tool_palette: vec!["proxima-workspace/list_files"],
                    max_rounds: 1,
                    ..WakeOptions::default()
                },
            )
            .await?;
        seed_execution_request(&world, seed.repo_id, "workspace-mode-request").await?;
        assert_eq!(world.engine.run_dispatcher_tick().await?, 1);
        world
            .assert_invocation_succeeded(workspace_instance, workspace_wake)
            .await?;
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    world.cleanup().await;
    result
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live Mistral API call; set PROXIMA_LIVE_MISTRAL=1"]
async fn live_mistral_core_emit_outputs() -> Result<(), Box<dyn std::error::Error>> {
    let Some(world) = LiveWakeWorld::new().await? else {
        return Ok(());
    };
    let result = async {
        let seed = world.seed_repo_and_code().await?;
        let (summary_instance, summary_wake) = world
            .add_live_personality_wake(
                "summary output",
                WakeEntryTriggerKind::OnMemory,
                CommitV1::SCHEMA_ID,
                vec!["core/emit_abstraction"],
                emit_abstraction_instruction(seed.repo_id, &seed.head_sha),
                WakeOptions {
                    max_rounds: 3,
                    ..WakeOptions::default()
                },
            )
            .await?;
        let (perspective_instance, perspective_wake) = world
            .add_live_personality_wake(
                "perspective output",
                WakeEntryTriggerKind::OnMemory,
                CommitV1::SCHEMA_ID,
                vec!["core/emit_perspective"],
                emit_perspective_instruction("development perspective output"),
                WakeOptions {
                    max_rounds: 3,
                    ..WakeOptions::default()
                },
            )
            .await?;
        seed_extra_commit(&world, seed.repo_id, "feat: trigger core outputs").await?;
        assert!(world.engine.run_dispatcher_tick().await? >= 2);
        world
            .assert_invocation_succeeded(summary_instance, summary_wake)
            .await?;
        world
            .assert_invocation_succeeded(perspective_instance, perspective_wake)
            .await?;
        let summaries: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM proxima_code.commit_summary_v1 WHERE repo_id = $1",
        )
        .bind(seed.repo_id)
        .fetch_one(world.pg.pool())
        .await?;
        assert!(summaries >= 1);
        let perspectives: i64 = sqlx::query_scalar(
            "SELECT count(*)
             FROM proxima_code.development_perspective_v1
             WHERE summary = $1",
        )
        .bind("development perspective output")
        .fetch_one(world.pg.pool())
        .await?;
        assert!(perspectives >= 1);
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    world.cleanup().await;
    result
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live Mistral API call; set PROXIMA_LIVE_MISTRAL=1"]
async fn live_mistral_goal_lifecycle_outputs() -> Result<(), Box<dyn std::error::Error>> {
    let Some(world) = LiveWakeWorld::new().await? else {
        return Ok(());
    };
    let result = async {
        let (instance, wake) = world
            .add_live_personality_wake(
                "goal lifecycle",
                WakeEntryTriggerKind::OnMemory,
                CommitV1::SCHEMA_ID,
                vec![
                    "proxima-goal/goal_propose",
                    "proxima-goal/goal_accept",
                    "proxima-goal/goal_mark_achieved",
                ],
                goal_lifecycle_instruction(),
                WakeOptions {
                    authored_by: WakeEntryAuthoredBy::Any,
                    max_rounds: 8,
                    ..WakeOptions::default()
                },
            )
            .await?;
        world.seed_repo_and_code().await?;
        assert_eq!(world.engine.run_dispatcher_tick().await?, 1);
        world.assert_invocation_succeeded(instance, wake).await?;
        let proposed: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM proxima_goal.goal_proposed_v1 WHERE title = $1",
        )
        .bind("live mistral lifecycle")
        .fetch_one(world.pg.pool())
        .await?;
        let activated: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM proxima_goal.goal_activated_v1 WHERE title = $1",
        )
        .bind("live mistral lifecycle")
        .fetch_one(world.pg.pool())
        .await?;
        let achieved: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM proxima_goal.goal_achieved_v1 WHERE title = $1",
        )
        .bind("live mistral lifecycle")
        .fetch_one(world.pg.pool())
        .await?;
        assert_eq!((proposed, activated, achieved), (1, 1, 1));
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    world.cleanup().await;
    result
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live Mistral API call; set PROXIMA_LIVE_MISTRAL=1"]
async fn live_mistral_code_action_outputs() -> Result<(), Box<dyn std::error::Error>> {
    let Some(world) = LiveWakeWorld::new().await? else {
        return Ok(());
    };
    let result = async {
        world.seed_repo_and_code().await?;
        let (planner, planner_wake) = world
            .add_live_personality_wake(
                "execution planner",
                WakeEntryTriggerKind::OnMemory,
                "proxima-goal/goal-activated-v1",
                vec!["proxima-code/code_emit_execution_request"],
                execution_request_instruction(),
                WakeOptions {
                    goal_scope: WakeEntryGoalScope::TriggerGoalAssigned,
                    authored_by: WakeEntryAuthoredBy::Any,
                    max_rounds: 4,
                    ..WakeOptions::default()
                },
            )
            .await?;
        world
            .activate_goal_for(planner, "live-mistral-execution-request")
            .await?;
        assert_eq!(world.engine.run_dispatcher_tick().await?, 1);
        world
            .assert_invocation_succeeded(planner, planner_wake)
            .await?;
        let execution_requests: i64 = sqlx::query_scalar(
            "SELECT count(*)
             FROM proxima_code.execution_request_v1
             WHERE request_key = 'live-mistral-execution-request'",
        )
        .fetch_one(world.pg.pool())
        .await?;
        assert_eq!(execution_requests, 1);
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    world.cleanup().await;
    result?;

    let Some(world) = LiveWakeWorld::new().await? else {
        return Ok(());
    };
    let result = async {
        let seed = world.seed_repo_and_code().await?;
        let (reviewer, reviewer_wake) = world
            .add_live_personality_wake(
                "workspace reviewer",
                WakeEntryTriggerKind::OnMemory,
                CoreWorkspaceRunV1::SCHEMA_ID,
                vec!["proxima-code/code_emit_workspace_review"],
                workspace_review_instruction(),
                WakeOptions {
                    max_rounds: 4,
                    ..WakeOptions::default()
                },
            )
            .await?;
        let (_run_memory, _request_memory) =
            seed_workspace_run(&world, seed.repo_id, &seed.repo_path, &seed.head_sha).await?;
        assert_eq!(world.engine.run_dispatcher_tick().await?, 1);
        world
            .assert_invocation_succeeded(reviewer, reviewer_wake)
            .await?;
        let reviews: i64 = sqlx::query_scalar(
            "SELECT count(*)
             FROM proxima_code.workspace_review_v1
             WHERE summary = 'live mistral workspace review'",
        )
        .fetch_one(world.pg.pool())
        .await?;
        assert_eq!(reviews, 1);
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    world.cleanup().await;
    result?;

    let Some(world) = LiveWakeWorld::new().await? else {
        return Ok(());
    };
    let result = async {
        let seed = world.seed_repo_and_code().await?;
        let correction_inst = world
            .engine
            .instantiate_personality(InstantiatePersonalityRequest {
                owner: world.owner.clone(),
                display_name: "correction planner".into(),
                purpose: "Live correction request coverage".into(),
            })
            .await?;
        let correction_wake_id = Uuid::now_v7();
        let correction_wake = wake_entry(
            correction_wake_id,
            correction_inst.instance_id,
            WakeEntryTriggerKind::OnMemory,
            WorkspaceReviewV1::SCHEMA_ID,
            vec!["proxima-code/code_emit_correction_execution_request"],
            correction_request_instruction(),
            WakeOptions {
                max_rounds: 4,
                ..WakeOptions::default()
            },
        )?;
        let target_wake = wake_entry(
            Uuid::now_v7(),
            correction_inst.instance_id,
            WakeEntryTriggerKind::OnMemory,
            ExecutionRequestV1::SCHEMA_ID,
            Vec::new(),
            String::new(),
            WakeOptions {
                execution_mode: WakeExecutionMode::Workspace,
                authored_by: WakeEntryAuthoredBy::SelfAuthor,
                ..WakeOptions::default()
            },
        )?;
        world
            .set_wakes(
                correction_inst.instance_id,
                vec![correction_wake, target_wake],
            )
            .await?;
        let (run_memory, request_memory) =
            seed_workspace_run(&world, seed.repo_id, &seed.repo_path, &seed.head_sha).await?;
        seed_workspace_review(
            &world,
            run_memory,
            request_memory,
            WorkspaceReviewVerdict::Rejected,
        )
        .await?;
        assert_eq!(world.engine.run_dispatcher_tick().await?, 1);
        world
            .assert_invocation_succeeded(correction_inst.instance_id, correction_wake_id)
            .await?;
        let corrections: i64 = sqlx::query_scalar(
            "SELECT count(*)
             FROM proxima_code.execution_request_v1
             WHERE request_key = 'live-mistral-correction-request'",
        )
        .fetch_one(world.pg.pool())
        .await?;
        assert_eq!(corrections, 1);
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    world.cleanup().await;
    result
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live Mistral API call; set PROXIMA_LIVE_MISTRAL=1"]
async fn live_mistral_read_tool_dispatch_smoke() -> Result<(), Box<dyn std::error::Error>> {
    let Some(world) = LiveWakeWorld::new().await? else {
        return Ok(());
    };
    let result = async {
        let (reader, wake) = world
            .add_live_personality_wake(
                "read tools",
                WakeEntryTriggerKind::OnMemory,
                CommitV1::SCHEMA_ID,
                vec![
                    "core/fetch_memory",
                    "core/list_active_goals",
                    "core/search_memories",
                    "proxima-code/code_search_chunks",
                    "proxima-code/code_open_file_revision",
                    "proxima-code/code_search_commits",
                ],
                read_tools_instruction(),
                WakeOptions {
                    authored_by: WakeEntryAuthoredBy::Any,
                    max_rounds: 10,
                    ..WakeOptions::default()
                },
            )
            .await?;
        world
            .activate_goal_for(reader, "live-mistral-read-tools")
            .await?;
        world.seed_repo_and_code().await?;
        assert_eq!(world.engine.run_dispatcher_tick().await?, 1);
        world.assert_invocation_succeeded(reader, wake).await?;
        world
            .assert_jsonl_successful_tools(
                reader,
                wake,
                &[
                    "core/fetch_memory",
                    "core/list_active_goals",
                    "core/search_memories",
                    "proxima-code/code_search_chunks",
                    "proxima-code/code_open_file_revision",
                    "proxima-code/code_search_commits",
                ],
            )
            .await?;
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    world.cleanup().await;
    result
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live Mistral API call; set PROXIMA_LIVE_MISTRAL=1"]
async fn live_mistral_merge_tool_is_master_token_guarded() -> Result<(), Box<dyn std::error::Error>>
{
    let Some(world) = LiveWakeWorld::new().await? else {
        return Ok(());
    };
    let result = async {
        let seed = world.seed_repo_and_code().await?;
        let (instance, wake) = world
            .add_live_personality_wake(
                "merge guard",
                WakeEntryTriggerKind::OnMemory,
                CoreWorkspaceRunV1::SCHEMA_ID,
                vec!["proxima-code/code_merge_workspace_run"],
                "Call proxima_code_code_merge_workspace_run with {\"workspace_run_memory\":\"F1\"}. Then stop.".into(),
                WakeOptions {
                    max_rounds: 3,
                    ..WakeOptions::default()
                },
            )
            .await?;
        seed_workspace_run_event(&world, seed.repo_id, &seed.repo_path, &seed.head_sha).await?;
        assert_eq!(world.engine.run_dispatcher_tick().await?, 1);
        world.assert_invocation_succeeded(instance, wake).await?;
        let decisions: i64 = sqlx::query_scalar(
             "SELECT count(*)
             FROM proxima_code.workspace_decision_v1
             WHERE decision = 'merged'",
        )
        .fetch_one(world.pg.pool())
        .await?;
        assert_eq!(
            decisions, 0,
            "model wake tokens must not bypass the merge tool master-token gate"
        );
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    world.cleanup().await;
    result
}

fn live_mistral_config() -> Result<Option<LiveMistralConfig>, Box<dyn std::error::Error>> {
    if std::env::var("PROXIMA_LIVE_MISTRAL").ok().as_deref() != Some("1") {
        eprintln!("skipping live Mistral test: set PROXIMA_LIVE_MISTRAL=1");
        return Ok(None);
    }
    let home = std::env::var("HOME")?;
    let path = Path::new(&home).join(".vibe/config.toml");
    let raw = std::fs::read_to_string(&path)?;
    let parsed: VibeConfig = toml::from_str(&raw)?;
    let model = parsed
        .models
        .iter()
        .find(|model| model.name == MODEL_ID && model.provider == "mistral")
        .ok_or("~/.vibe/config.toml has no mistral-medium-3.5 model for provider mistral")?;
    let provider = parsed
        .providers
        .iter()
        .find(|provider| provider.name == "mistral")
        .ok_or("~/.vibe/config.toml has no provider named mistral")?;
    if provider.api_key_env_var.trim().is_empty() {
        return Err("Vibe mistral provider has empty api_key_env_var".into());
    }
    std::env::var(&provider.api_key_env_var).map_err(|_| {
        format!(
            "required live Mistral env var from ~/.vibe/config.toml is unset: {}",
            provider.api_key_env_var
        )
    })?;
    Ok(Some(LiveMistralConfig {
        base_url: normalize_mistral_base_url(&provider.api_base),
        api_key_env: provider.api_key_env_var.clone(),
        temperature: model.temperature,
    }))
}

fn normalize_mistral_base_url(api_base: &str) -> String {
    api_base
        .trim_end_matches('/')
        .trim_end_matches("/v1")
        .to_string()
}

fn registry() -> Arc<proxima_core::FlavorRegistryFrozen> {
    let mut registry = FlavorRegistry::new();
    proxima_flavor_goal::register(&mut registry);
    proxima_code::register(&mut registry);
    Arc::new(registry.freeze())
}

fn test_owner() -> Owner {
    Owner {
        principal: Principal::User(UserId::new(Uuid::now_v7())),
        org_id: OrgId::new(Uuid::now_v7()),
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

fn setup_author() -> McpAuthorContext {
    McpAuthorContext {
        model_id: "live-mistral-test-setup".into(),
        client_name: "mistral_wake_pg".into(),
        client_version: "1".into(),
        caller_self_perspective: None,
    }
}

fn wake_entry(
    wake_entry_id: Uuid,
    instance_id: proxima_core::PersonalityInstanceId,
    trigger_kind: WakeEntryTriggerKind,
    trigger_id: &str,
    palette: Vec<&str>,
    instructions: String,
    options: WakeOptions,
) -> Result<WakeEntryDraft, proxima_core::ProtocolError> {
    let mut wake = WakeEntryDraft::new(
        wake_entry_id,
        instance_id,
        trigger_kind,
        trigger_id,
        trigger_id,
        options.authored_by,
        options.probability_promille,
        options.model_tier,
        options.inference_target_ref,
        palette.into_iter().map(str::to_string).collect(),
        options.max_rounds,
    )?;
    wake.enabled = options.enabled;
    wake.execution_mode = options.execution_mode;
    wake.goal_scope = options.goal_scope;
    wake.instructions = instructions;
    wake.workspace_tool_palette = options
        .workspace_tool_palette
        .into_iter()
        .map(str::to_string)
        .collect();
    Ok(wake)
}

fn emit_perspective_instruction(summary: &str) -> String {
    format!(
        "Call the available development perspective emit tool from Wake Contract exactly once with these top-level JSON fields: {}. Do not pass schema_id, schema_version, or payload. Then stop.",
        serde_json::json!({
            "repo_id": null,
            "summary": summary,
            "pattern": "live_mistral_test",
            "risk": "low",
            "recommended_posture": "continue",
            "confidence": 0.9,
            "text": summary
        })
    )
}

fn emit_abstraction_instruction(repo_id: Uuid, sha: &str) -> String {
    format!(
        "Call the available commit summary emit tool from Wake Contract exactly once with these top-level JSON fields: {}. Do not pass schema_id, schema_version, or payload. Then stop.",
        serde_json::json!({
            "repo_id": repo_id,
            "commit_sha": sha,
            "summary": "live mistral commit summary",
            "key_files": ["README.md"],
            "change_kind": "test",
            "text": "live mistral commit summary"
        })
    )
}

fn goal_lifecycle_instruction() -> String {
    "Use the available functions in this exact order. First call proxima_goal_goal_propose with {\"payload\":{\"schema_id\":\"proxima-goal/simple-text-v1\",\"body\":{\"title\":\"live mistral lifecycle\",\"text\":\"Prove live Mistral goal lifecycle tools.\"}},\"evidence\":[\"F1\"],\"idempotency_key\":\"live-mistral-lifecycle-propose\"}. Then call proxima_goal_goal_accept with the returned proposal handle and idempotency_key live-mistral-lifecycle-accept. Then call proxima_goal_goal_mark_achieved with the active goal handle returned by accept, evidence [\"F1\"], and idempotency_key live-mistral-lifecycle-achieved. Then stop.".into()
}

fn execution_request_instruction() -> String {
    format!(
        "Call the available function proxima_code_code_emit_execution_request exactly once with this JSON: {}. Then stop.",
        serde_json::json!({
            "repo_handle": REPO_HANDLE,
            "title": "Live Mistral execution request",
            "instructions": "Make the smallest safe change.",
            "idempotency_key": "live-mistral-execution-request",
            "goal_activated_memory": "F1",
            "evidence": []
        })
    )
}

fn workspace_review_instruction() -> String {
    "Call the available function proxima_code_code_emit_workspace_review exactly once with {\"workspace_run_memory\":\"F1\",\"verdict\":\"approved\",\"summary\":\"live mistral workspace review\",\"findings\":[],\"verification_summary\":\"reviewed by live mistral\",\"idempotency_key\":\"live-mistral-workspace-review\"}. Then stop.".into()
}

fn correction_request_instruction() -> String {
    "Call the available function proxima_code_code_emit_correction_execution_request exactly once with {\"workspace_review_memory\":\"F1\",\"target_personality\":\"I1\",\"idempotency_key\":\"live-mistral-correction-request\"}. Then stop.".into()
}

fn read_tools_instruction() -> String {
    format!(
        "Call these available functions exactly once each, then stop: \
         core_fetch_memory with {{\"memory\":\"F1\"}}; \
         core_list_active_goals with {{}}; \
         core_search_memories with {{\"query\":\"live mistral\",\"mode\":\"lexical\",\"limit\":3}}; \
         proxima_code_code_search_chunks with {{\"query\":\"important function marker\",\"limit\":3,\"repo_handle\":\"{REPO_HANDLE}\",\"include_calls\":false}}; \
         proxima_code_code_open_file_revision with {{\"repo_handle\":\"{REPO_HANDLE}\",\"file_path\":\"README.md\",\"include_text\":true,\"line_start\":1,\"line_limit\":5}}; \
         proxima_code_code_search_commits with {{\"query\":\"live mistral wake coverage\",\"limit\":3,\"repo_handle\":\"{REPO_HANDLE}\"}}."
    )
}

async fn seed_extra_commit(
    world: &LiveWakeWorld,
    repo_id: Uuid,
    message: &str,
) -> Result<MemoryId, Box<dyn std::error::Error>> {
    let now = time::OffsetDateTime::now_utc();
    let payload = CommitV1 {
        repo_id,
        sha: Uuid::now_v7().simple().to_string(),
        parents: Vec::new(),
        author_name: "Proxima Test".into(),
        author_email: "proxima@example.test".into(),
        author_time: now,
        committer_name: "Proxima Test".into(),
        committer_email: "proxima@example.test".into(),
        committer_time: now,
        message: message.into(),
    };
    let outcome = ingest_commit(
        world.pg.pool(),
        &world.owner,
        SourceBatchId::new(Uuid::now_v7()),
        &payload,
        now,
    )
    .await?;
    Ok(outcome.memory_id)
}

async fn seed_execution_request(
    world: &LiveWakeWorld,
    repo_id: Uuid,
    key: &str,
) -> Result<MemoryId, Box<dyn std::error::Error>> {
    let payload = ExecutionRequestV1 {
        repo_id,
        title: "Workspace mode trigger".into(),
        instructions: "No-op live Mistral workspace mode trigger.".into(),
        request_key: key.into(),
    };
    let sidecar_payload = payload.clone();
    ingest_fact_with_sidecar(
        world,
        ExecutionRequestV1::SCHEMA_ID,
        ExecutionRequestV1::SCHEMA_VERSION,
        &payload,
        "proxima-code/execution-request",
        proxima_code::EXECUTION_REQUEST_OBJECT_SCHEMA,
        proxima_code::EXECUTION_REQUEST_WHOLE_SCHEMA,
        |memory_id, tx| {
            Box::pin(async move {
                sqlx::query(
                    "INSERT INTO proxima_code.execution_request_v1
                        (memory_id, repo_id, title, instructions, request_key)
                     VALUES ($1,$2,$3,$4,$5)",
                )
                .bind(memory_id.into_inner())
                .bind(sidecar_payload.repo_id)
                .bind(&sidecar_payload.title)
                .bind(&sidecar_payload.instructions)
                .bind(&sidecar_payload.request_key)
                .execute(&mut **tx)
                .await?;
                Ok(())
            })
        },
    )
    .await
}

async fn seed_workspace_run(
    world: &LiveWakeWorld,
    repo_id: Uuid,
    repo_path: &Path,
    head_sha: &str,
) -> Result<(MemoryId, MemoryId), Box<dyn std::error::Error>> {
    let request = seed_execution_request(
        world,
        repo_id,
        &format!("workspace-run-request-{}", Uuid::now_v7()),
    )
    .await?;
    let run = seed_workspace_run_event(world, repo_id, repo_path, head_sha).await?;
    append_fact_derived_edge(world, run, request).await?;
    Ok((run, request))
}

async fn seed_workspace_run_event(
    world: &LiveWakeWorld,
    _repo_id: Uuid,
    repo_path: &Path,
    head_sha: &str,
) -> Result<MemoryId, Box<dyn std::error::Error>> {
    let payload = CoreWorkspaceRunV1 {
        wake_invocation_id: Uuid::now_v7(),
        wake_entry_id: Uuid::now_v7(),
        personality_instance_id: Uuid::now_v7(),
        binding_kind: "code_git_worktree".into(),
        finalize: "commit_all_candidate".into(),
        repo_path: repo_path.to_string_lossy().to_string(),
        base_ref: "main".into(),
        worktree_path: repo_path.to_string_lossy().to_string(),
        branch_name: "main".into(),
        parent_sha: head_sha.into(),
        head_sha: head_sha.into(),
        committed: true,
        diff_stat_json: WorkspaceDiffStat {
            files_changed: 1,
            insertions: 1,
            deletions: 0,
            files: vec![WorkspaceDiffFile {
                path: "README.md".into(),
                insertions: 1,
                deletions: 0,
            }],
        },
        exit_code: Some(0),
        stdout_tail: Some("ok".into()),
        stderr_tail: None,
        duration_ms: Some(1),
        sandbox_image: None,
        sandbox_container: None,
        wake_branch: None,
        transcript_blob_hash: None,
        network_log_blob_hash: None,
    };
    let sidecar_payload = payload.clone();
    ingest_fact_with_sidecar(
        world,
        CoreWorkspaceRunV1::SCHEMA_ID,
        CoreWorkspaceRunV1::SCHEMA_VERSION,
        &payload,
        CORE_WORKSPACE_RUN_SOURCE_ID,
        CORE_WORKSPACE_RUN_OBJECT_SCHEMA,
        CORE_WORKSPACE_RUN_WHOLE_SCHEMA,
        |memory_id, tx| {
            Box::pin(async move {
                sqlx::query(
                    "INSERT INTO proxima_core.workspace_run_v1
                        (memory_id, wake_invocation_id, wake_entry_id, personality_instance_id,
                         binding_kind, finalize, repo_path, base_ref, worktree_path,
                         branch_name, parent_sha, head_sha, committed, diff_stat_json, exit_code,
                         stdout_tail, stderr_tail, duration_ms)
                     VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18)",
                )
                .bind(memory_id.into_inner())
                .bind(sidecar_payload.wake_invocation_id)
                .bind(sidecar_payload.wake_entry_id)
                .bind(sidecar_payload.personality_instance_id)
                .bind(&sidecar_payload.binding_kind)
                .bind(&sidecar_payload.finalize)
                .bind(&sidecar_payload.repo_path)
                .bind(&sidecar_payload.base_ref)
                .bind(&sidecar_payload.worktree_path)
                .bind(&sidecar_payload.branch_name)
                .bind(&sidecar_payload.parent_sha)
                .bind(&sidecar_payload.head_sha)
                .bind(sidecar_payload.committed)
                .bind(serde_json::to_value(&sidecar_payload.diff_stat_json)?)
                .bind(sidecar_payload.exit_code)
                .bind(sidecar_payload.stdout_tail.as_deref())
                .bind(sidecar_payload.stderr_tail.as_deref())
                .bind(
                    sidecar_payload
                        .duration_ms
                        .and_then(|value| i64::try_from(value).ok()),
                )
                .execute(&mut **tx)
                .await?;
                Ok(())
            })
        },
    )
    .await
}

async fn seed_workspace_review(
    world: &LiveWakeWorld,
    run: MemoryId,
    request: MemoryId,
    verdict: WorkspaceReviewVerdict,
) -> Result<MemoryId, Box<dyn std::error::Error>> {
    let payload = WorkspaceReviewV1 {
        workspace_run_memory_id: run.into_inner(),
        execution_request_memory_id: request.into_inner(),
        verdict,
        round_index: 0,
        summary: "seed rejected workspace review".into(),
        findings: vec![WorkspaceReviewFinding {
            severity: "medium".into(),
            file_path: Some("README.md".into()),
            line: Some(1),
            message: "needs correction".into(),
        }],
        correction_instructions: Some("Correct the README change.".into()),
        verification_summary: None,
        reviewed_at: time::OffsetDateTime::now_utc(),
    };
    let sidecar_payload = payload.clone();
    ingest_fact_with_sidecar(
        world,
        WorkspaceReviewV1::SCHEMA_ID,
        WorkspaceReviewV1::SCHEMA_VERSION,
        &payload,
        proxima_code::mcp::WORKSPACE_REVIEW_SOURCE_ID,
        proxima_code::mcp::WORKSPACE_REVIEW_OBJECT_SCHEMA,
        proxima_code::mcp::WORKSPACE_REVIEW_WHOLE_SCHEMA,
        |memory_id, tx| {
            Box::pin(async move {
                sqlx::query(
                    "INSERT INTO proxima_code.workspace_review_v1
                        (memory_id, workspace_run_memory_id, execution_request_memory_id,
                         verdict, round_index, summary, findings_json,
                         correction_instructions, verification_summary, reviewed_at)
                     VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
                )
                .bind(memory_id.into_inner())
                .bind(sidecar_payload.workspace_run_memory_id)
                .bind(sidecar_payload.execution_request_memory_id)
                .bind(sidecar_payload.verdict)
                .bind(i32::try_from(sidecar_payload.round_index).unwrap_or(i32::MAX))
                .bind(&sidecar_payload.summary)
                .bind(serde_json::to_value(&sidecar_payload.findings)?)
                .bind(sidecar_payload.correction_instructions.as_deref())
                .bind(sidecar_payload.verification_summary.as_deref())
                .bind(sidecar_payload.reviewed_at)
                .execute(&mut **tx)
                .await?;
                Ok(())
            })
        },
    )
    .await
}

async fn ingest_fact_with_sidecar<T, F>(
    world: &LiveWakeWorld,
    schema_id: &str,
    schema_version: u32,
    payload: &T,
    source_id: &str,
    object_schema: &str,
    mapping_schema: &str,
    insert_sidecar: F,
) -> Result<MemoryId, Box<dyn std::error::Error>>
where
    T: serde::Serialize,
    F: for<'a> FnOnce(
        MemoryId,
        &'a mut sqlx::Transaction<'_, sqlx::Postgres>,
    )
        -> futures::future::BoxFuture<'a, Result<(), Box<dyn std::error::Error>>>,
{
    let mut payload_bytes = Vec::new();
    ciborium::ser::into_writer(payload, &mut payload_bytes)?;
    let content_hash = *blake3::hash(&payload_bytes).as_bytes();
    let now = time::OffsetDateTime::now_utc();
    let draft = EventDraft {
        source_id: SourceId::new(source_id),
        source_batch_id: SourceBatchId::new(Uuid::now_v7()),
        owner: world.owner.clone(),
        schema_id: SchemaId::new(schema_id.into()),
        schema_version: SchemaVersion::new(schema_version),
        payload: payload_bytes,
        observed_at: now,
        occurred_at: now,
        cited_object: CitedObjectHint {
            schema_id: SchemaId::new(object_schema.into()),
            schema_version: SchemaVersion::new(1),
            content_hash,
        },
        citation_mapping: CitationMappingHint {
            schema_id: SchemaId::new(mapping_schema.into()),
            schema_version: SchemaVersion::new(1),
        },
    };
    let mut tx = world.pg.pool().begin().await?;
    let outcome = ingest_event_in_tx(&mut tx, &draft).await?;
    insert_sidecar(outcome.memory_id, &mut tx).await?;
    tx.commit().await?;
    Ok(outcome.memory_id)
}

async fn append_fact_derived_edge(
    world: &LiveWakeWorld,
    source: MemoryId,
    target: MemoryId,
) -> Result<(), Box<dyn std::error::Error>> {
    let relation = world
        .engine
        .registry()
        .resolve_relation(CORE_DERIVED_FROM_RELATION)
        .expect("core/derived-from registered");
    let mut tx = world.pg.pool().begin().await?;
    append_edge_in_tx(
        &mut tx,
        &EdgeDraft {
            edge_id: Uuid::now_v7(),
            relation,
            source_kind: EntityKind::Fact,
            source_memory_id: Some(source.into_inner()),
            source_goal_id: None,
            target_kind: EntityKind::Fact,
            target_memory_id: Some(target.into_inner()),
            target_goal_id: None,
            authorship_kind: EdgeAuthorshipKind::EventSource,
            authorship_owner_memory_id: None,
            owner: &world.owner,
        },
        None,
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

fn git(cwd: &Path, args: &[&str]) -> Result<String, Box<dyn std::error::Error>> {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}
