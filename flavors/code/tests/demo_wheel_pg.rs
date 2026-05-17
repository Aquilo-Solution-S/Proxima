//! Ignored live demo: measurable Planner -> Worker -> Verifier -> Goal-Reviewer wheel.
//!
//! Compile:
//!
//! ```sh
//! cargo test -p proxima-code --test demo_wheel_pg --no-run
//! ```
//!
//! Live:
//!
//! ```sh
//! set -a; source ~/.proxima/.env; set +a
//! PROXIMA_LIVE_MISTRAL=1 \
//! PROXIMA_DEMO_REPO=/private/tmp/proxima-signal-match \
//! cargo test -p proxima-code --test demo_wheel_pg -- --ignored --nocapture --test-threads=1
//! ```

#![allow(clippy::too_many_lines)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use proxima_code::{ExecutionRequestV1, WorkspaceReviewV1, WorkspaceRunV1, register_repo};
use proxima_core::auth::NoAuth;
use proxima_core::harness::{HarnessAdapter, HarnessContext, HarnessProgram, ProviderTarget};
use proxima_core::llm::{EmbeddingClient, LlmError};
use proxima_core::mcp::McpAuthorContext;
use proxima_core::models::EmbedCaps;
use proxima_core::personality::{
    InstantiatePersonalityRequest, SetWakeEntriesRequest, WakeEntryDraft,
};
use proxima_core::storage::Storage;
use proxima_core::{
    BindInferenceTierRequest, CORE_INSPIRES_RELATION, Credentials, EdgeAuthorshipKind, Engine,
    EntityKind, FactPayload, FlavorRegistry, GoalId, InferenceTargetConfig, MemoryId,
    MistralChatConfig, ModelTier, OrgId, Owner, PersonalityInstanceId, Principal,
    RegisterInferenceTargetRequest, UserId, WakeEntryAuthoredBy, WakeEntryGoalScope,
    WakeEntryTriggerKind, WakeExecutionMode,
};
use proxima_harness::HarnessLoop;
use proxima_mcp_server::McpToolHost;
use proxima_storage_pg::PgStorage;
use proxima_storage_pg::settings::EmbeddingModel;
use proxima_storage_pg::verbs::edge_append::{EdgeDraft, append_edge_in_tx};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{Connection, Executor, PgConnection, Row};
use uuid::Uuid;

const ADMIN_URL: &str = "postgres://proxima:proxima@localhost/postgres";
const TARGET_REF: &str = "demo/mistral-medium-3.5";
const MODEL_ID: &str = "mistral-medium-3.5";
const EMBED_VENDOR: &str = "Ollama";
const EMBED_MODEL: &str = "qwen3-embedding:8b";
const DEMO_REPO_HANDLE: &str = "signal-match-demo";
const GOAL_TITLE: &str = "Signal Match static SPA demo";

#[derive(Debug)]
struct DemoEmbedding;

#[async_trait]
impl EmbeddingClient for DemoEmbedding {
    async fn embed(&self, _text: &str) -> Result<Vec<f32>, LlmError> {
        Ok(vec![0.0; 4096])
    }

    fn model_id(&self) -> &'static str {
        EMBED_MODEL
    }

    fn dim(&self) -> usize {
        4096
    }
}

#[derive(Debug, Clone)]
struct DemoConfig {
    repo_path: PathBuf,
    run_dir: PathBuf,
    base_url: String,
    api_key_env: String,
    max_ticks: u32,
    max_correction_loops: u32,
    wake_max_rounds: u16,
}

#[derive(Debug, Serialize)]
struct Metrics {
    run_dir: String,
    repo_path: String,
    db_name: String,
    max_ticks: u32,
    max_correction_loops: u32,
    wake_max_rounds: u16,
    dispatcher_tick_count: u32,
    wake_invocation_count_by_role: BTreeMap<String, u32>,
    wake_invocations: Vec<WakeInvocationMetric>,
    correction_loop_count: u32,
    output_sidecar_counts_by_schema: BTreeMap<String, i64>,
    workspace_run_count: i64,
    review_verdicts: BTreeMap<String, i64>,
    final_goal_state: String,
    goal_achieved_fact_exists: bool,
    git_diff_stats: GitDiffStats,
    final_changed_files: Vec<String>,
    deterministic_checks: BTreeMap<String, bool>,
    deterministic_pass: bool,
    reviewer_score: Option<ReviewerScore>,
    reviewer_score_error: Option<String>,
    auto_merge: Option<AutoMergeMetric>,
    overall_score: u32,
    total_model_rounds: u32,
    wall_clock_seconds: f64,
    score_per_model_round: Option<f64>,
    score_per_wall_clock_second: f64,
    budget_pass: bool,
}

#[derive(Debug, Serialize)]
struct WakeInvocationMetric {
    role: String,
    status: String,
    duration_ms: Option<i64>,
    rounds_or_turns: Option<i32>,
    tool_calls: Option<i32>,
    target_ref: Option<String>,
    model_id: Option<String>,
    prompt_tokens: Option<i64>,
    completion_tokens: Option<i64>,
    cost_usd: Option<String>,
    failure_reason: Option<String>,
}

#[derive(Debug, Default, Serialize)]
struct GitDiffStats {
    files_changed: u32,
    insertions: u32,
    deletions: u32,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct ReviewerScore {
    score: u32,
    requirements: u32,
    usability: u32,
    code_simplicity: u32,
    visual_polish: u32,
    robustness: u32,
    rationale: String,
}

#[derive(Debug, Clone, Serialize)]
struct AutoMergeMetric {
    worktree_path: String,
    branch_name: String,
    commit_sha: String,
    merged_to_repo: String,
    merged_to_branch: String,
}

#[derive(Debug, Clone)]
struct WorktreeInfo {
    path: PathBuf,
    branch_name: String,
}

struct DemoWorld {
    cfg: DemoConfig,
    db_name: String,
    pg: PgStorage,
    owner: Owner,
    engine: Arc<proxima_core::Engine>,
    server: McpToolHost,
    harness: Arc<HarnessLoop>,
    role_ids: BTreeMap<String, PersonalityInstanceId>,
    goal_id: Option<GoalId>,
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live Mistral API call; set PROXIMA_LIVE_MISTRAL=1"]
async fn measurable_signal_match_demo_wheel() -> Result<(), Box<dyn std::error::Error>> {
    let Some(cfg) = DemoConfig::from_env()? else {
        return Ok(());
    };
    let started = Instant::now();
    let mut world = DemoWorld::new(cfg).await?;
    let result = world.run(started).await;
    let db_name = world.db_name.clone();
    let cleanup = world.cleanup().await;
    if let Err(err) = cleanup {
        eprintln!("demo cleanup failed for {db_name}: {err}");
    }
    result
}

impl DemoConfig {
    fn from_env() -> Result<Option<Self>, Box<dyn std::error::Error>> {
        if std::env::var("PROXIMA_LIVE_MISTRAL").ok().as_deref() != Some("1") {
            eprintln!("skipping demo wheel: set PROXIMA_LIVE_MISTRAL=1");
            return Ok(None);
        }
        std::env::var("MISTRAL_API_KEY")
            .map_err(|_| "MISTRAL_API_KEY must be set for demo wheel")?;

        let timestamp = time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)?
            .replace([':', '.'], "-");
        let run_dir = std::env::var("PROXIMA_DEMO_RUN_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                PathBuf::from(format!("/private/tmp/proxima-demo-runs/{timestamp}"))
            });
        let repo_path = std::env::var("PROXIMA_DEMO_REPO")
            .map(PathBuf::from)
            .unwrap_or_else(|_| run_dir.join("signal-match-repo"));
        let base_url = std::env::var("MISTRAL_BASE_URL")
            .unwrap_or_else(|_| "https://api.mistral.ai".to_string())
            .trim_end_matches('/')
            .trim_end_matches("/v1")
            .to_string();
        Ok(Some(Self {
            repo_path,
            run_dir,
            base_url,
            api_key_env: "MISTRAL_API_KEY".into(),
            max_ticks: env_u32("PROXIMA_DEMO_MAX_TICKS", 12)?,
            max_correction_loops: env_u32("PROXIMA_DEMO_MAX_CORRECTION_LOOPS", 2)?,
            wake_max_rounds: env_u16("PROXIMA_DEMO_WAKE_MAX_ROUNDS", 10)?,
        }))
    }
}

impl DemoWorld {
    async fn new(cfg: DemoConfig) -> Result<Self, Box<dyn std::error::Error>> {
        std::fs::create_dir_all(&cfg.run_dir)?;
        let db_name = format!("proxima_demo_wheel_{}", Uuid::now_v7().simple());
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

        let owner = Owner {
            principal: Principal::User(UserId::new(Uuid::now_v7())),
            org_id: OrgId::new(Uuid::now_v7()),
        };
        let engine = Arc::new(build_demo_engine(&cfg, pg.clone(), owner.clone()));
        engine
            .set_mcp_url("http://127.0.0.1:1/mcp".to_string())
            .await;
        let server = McpToolHost::from_pool(pg.pool().clone(), owner.clone(), registry())
            .with_engine(engine.clone());
        let harness = Arc::new(HarnessLoop::new(engine.clone(), Arc::new(server.clone())));
        engine.set_target_adapter(harness.clone()).await;

        let world = Self {
            cfg,
            db_name,
            pg,
            owner,
            engine,
            server,
            harness,
            role_ids: BTreeMap::new(),
            goal_id: None,
        };
        world.configure_runtime().await?;
        Ok(world)
    }

    async fn run(&mut self, started: Instant) -> Result<(), Box<dyn std::error::Error>> {
        let repo_id = prepare_demo_repo(&self.cfg.repo_path).await?;
        register_repo(
            self.pg.pool(),
            &self.owner,
            repo_id,
            self.cfg.repo_path.to_str().ok_or("repo path is not utf8")?,
            DEMO_REPO_HANDLE,
        )
        .await?;

        let planner = self
            .instantiate("Planner", "Emit execution requests for active goals")
            .await?;
        let worker = self
            .instantiate("Worker", "Run workspace edits for execution requests")
            .await?;
        let verifier = self
            .instantiate("Verifier", "Review workspace runs against the goal")
            .await?;
        let reviewer = self
            .instantiate(
                "Goal-Reviewer",
                "Close achieved goals or request corrections",
            )
            .await?;

        self.set_single_wake(
            planner,
            "Planner",
            WakeEntryTriggerKind::OnMemory,
            "proxima-goal/goal-activated-v1",
            WakeExecutionMode::SubstrateOnly,
            vec!["proxima-code/code_emit_execution_request"],
            Vec::new(),
            planner_instruction(),
            WakeOptions {
                goal_scope: WakeEntryGoalScope::TriggerGoalAssigned,
                authored_by: WakeEntryAuthoredBy::Any,
                ..WakeOptions::default_with_rounds(self.cfg.wake_max_rounds)
            },
        )
        .await?;
        self.set_single_wake(
            worker,
            "Worker",
            WakeEntryTriggerKind::OnMemory,
            ExecutionRequestV1::SCHEMA_ID,
            WakeExecutionMode::Workspace,
            Vec::new(),
            vec![
                "proxima-workspace/text_editor",
                "proxima-workspace/shell",
                "proxima-workspace/list_files",
            ],
            worker_instruction(),
            WakeOptions::default_with_rounds(self.cfg.wake_max_rounds),
        )
        .await?;
        self.set_single_wake(
            verifier,
            "Verifier",
            WakeEntryTriggerKind::OnMemory,
            WorkspaceRunV1::SCHEMA_ID,
            WakeExecutionMode::Workspace,
            vec!["proxima-code/code_emit_workspace_review"],
            vec![
                "proxima-workspace/text_editor",
                "proxima-workspace/shell",
                "proxima-workspace/list_files",
            ],
            verifier_instruction(),
            WakeOptions::default_with_rounds(self.cfg.wake_max_rounds),
        )
        .await?;

        let (_goal_memory, active_goal) = self.activate_goal(planner).await?;
        self.goal_id = Some(active_goal);
        self.append_goal_assignment(active_goal, reviewer).await?;
        self.set_goal_reviewer_wakes(reviewer).await?;

        let mut ticks = 0_u32;
        while ticks < self.cfg.max_ticks {
            ticks += 1;
            let fired = self.engine.run_dispatcher_tick().await?;
            let correction_loops = self.correction_loop_count().await?;
            if self.goal_achieved_fact_exists().await? {
                let mut metrics = self.collect_metrics(started, ticks).await?;
                if !metrics.budget_pass || !metrics.deterministic_pass {
                    self.write_outputs(&metrics).await?;
                    return Err(self.failure_report("final checks failed").await?.into());
                }
                metrics.auto_merge = Some(self.auto_merge_successful_worktree().await?);
                self.write_outputs(&metrics).await?;
                return Ok(());
            }
            if correction_loops > self.cfg.max_correction_loops {
                let metrics = self.collect_metrics(started, ticks).await?;
                self.write_outputs(&metrics).await?;
                return Err(self
                    .failure_report("max correction loops exceeded")
                    .await?
                    .into());
            }
            if fired == 0 {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }

        let metrics = self.collect_metrics(started, ticks).await?;
        self.write_outputs(&metrics).await?;
        Err(self
            .failure_report("max dispatcher ticks exceeded")
            .await?
            .into())
    }

    async fn cleanup(self) -> Result<(), sqlx::Error> {
        let DemoWorld {
            db_name,
            pg,
            engine,
            server,
            harness,
            ..
        } = self;
        drop(server);
        drop(engine);
        drop(harness);
        drop(pg);
        drop_db(&db_name).await
    }

    async fn configure_runtime(&self) -> Result<(), Box<dyn std::error::Error>> {
        self.pg
            .register_inference_target(&RegisterInferenceTargetRequest {
                owner: self.owner.clone(),
                target_ref: TARGET_REF.into(),
                config: InferenceTargetConfig::MistralChat(MistralChatConfig {
                    base_url: self.cfg.base_url.clone(),
                    model_id: MODEL_ID.into(),
                    api_key_env: self.cfg.api_key_env.clone(),
                    temperature: Some(0.2),
                    max_completion_tokens: Some(4096),
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
        self.pg
            .register_embedding_model(EmbeddingModel {
                vendor: EMBED_VENDOR.into(),
                model_id: EMBED_MODEL.into(),
                base_url: "http://localhost:11434".into(),
                caps: EmbedCaps {
                    dim: 4096,
                    matryoshka: false,
                },
                secret_ref: None,
            })
            .await?;
        self.pg
            .set_embedding_active(EMBED_VENDOR, EMBED_MODEL)
            .await?;
        Ok(())
    }

    async fn instantiate(
        &mut self,
        role: &str,
        purpose: &str,
    ) -> Result<PersonalityInstanceId, Box<dyn std::error::Error>> {
        let inst = self
            .engine
            .instantiate_personality(InstantiatePersonalityRequest {
                owner: self.owner.clone(),
                display_name: role.into(),
                purpose: purpose.into(),
            })
            .await?;
        self.role_ids.insert(role.into(), inst.instance_id);
        Ok(inst.instance_id)
    }

    #[allow(clippy::too_many_arguments)]
    async fn set_single_wake(
        &self,
        instance_id: PersonalityInstanceId,
        role: &str,
        trigger_kind: WakeEntryTriggerKind,
        trigger_id: &str,
        execution_mode: WakeExecutionMode,
        substrate_palette: Vec<&str>,
        workspace_palette: Vec<&str>,
        instructions: String,
        options: WakeOptions,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut wake = WakeEntryDraft::new(
            Uuid::now_v7(),
            instance_id,
            trigger_kind,
            trigger_id,
            format!("{role} demo wake"),
            options.authored_by,
            1000,
            ModelTier::Standard,
            Some(TARGET_REF.into()),
            substrate_palette.into_iter().map(str::to_string).collect(),
            options.max_rounds,
        )?;
        wake.execution_mode = execution_mode;
        wake.goal_scope = options.goal_scope;
        wake.instructions = instructions;
        wake.workspace_tool_palette = workspace_palette.into_iter().map(str::to_string).collect();
        self.engine
            .set_wake_entries(
                &Credentials::None,
                &SetWakeEntriesRequest {
                    owner: self.owner.clone(),
                    personality_instance_id: instance_id,
                    entries: vec![wake],
                },
            )
            .await?;
        Ok(())
    }

    async fn set_goal_reviewer_wakes(
        &self,
        reviewer: PersonalityInstanceId,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut review_wake = WakeEntryDraft::new(
            Uuid::now_v7(),
            reviewer,
            WakeEntryTriggerKind::OnMemory,
            WorkspaceReviewV1::SCHEMA_ID,
            "Goal-Reviewer demo wake",
            WakeEntryAuthoredBy::Any,
            1000,
            ModelTier::Deep,
            Some(TARGET_REF.into()),
            vec![
                "core/list_active_goals".into(),
                "proxima-goal/goal_mark_achieved".into(),
                "proxima-code/code_emit_correction_execution_request".into(),
            ],
            self.cfg.wake_max_rounds,
        )?;
        review_wake.instructions = goal_reviewer_instruction();

        let mut target_validation_wake = WakeEntryDraft::new(
            Uuid::now_v7(),
            reviewer,
            WakeEntryTriggerKind::OnMemory,
            ExecutionRequestV1::SCHEMA_ID,
            "Goal-Reviewer target validation wake",
            WakeEntryAuthoredBy::SelfAuthor,
            1000,
            ModelTier::Standard,
            Some(TARGET_REF.into()),
            Vec::new(),
            1,
        )?;
        target_validation_wake.execution_mode = WakeExecutionMode::Workspace;
        target_validation_wake.instructions = "Target validation only. Stop.".into();

        self.engine
            .set_wake_entries(
                &Credentials::None,
                &SetWakeEntriesRequest {
                    owner: self.owner.clone(),
                    personality_instance_id: reviewer,
                    entries: vec![review_wake, target_validation_wake],
                },
            )
            .await?;
        Ok(())
    }

    async fn activate_goal(
        &self,
        planner: PersonalityInstanceId,
    ) -> Result<(MemoryId, GoalId), Box<dyn std::error::Error>> {
        let proposed = self
            .server
            .call_tool(
                "proxima-goal/goal_propose",
                json!({
                    "payload": {
                        "schema_id": "proxima-goal/simple-text-v1",
                        "body": {
                            "title": GOAL_TITLE,
                            "text": "Build a package-free static SPA game named Signal Match in index.html. It must run by opening index.html directly, be responsive, have four colored pads, sequence playback, click and keyboard input, score and level display, failure state, and restart."
                        }
                    },
                    "target_personality": planner.into_inner().to_string(),
                    "evidence": [],
                    "idempotency_key": "demo-signal-match-propose"
                }),
                setup_author(),
                None,
            )
            .await?;
        let proposal = proposed
            .get("handle")
            .and_then(serde_json::Value::as_str)
            .ok_or("missing proposal handle")?;
        self.server
            .call_tool(
                "proxima-goal/goal_accept",
                json!({
                    "proposal": proposal,
                    "target_personality": planner.into_inner().to_string(),
                    "idempotency_key": "demo-signal-match-accept"
                }),
                setup_author(),
                None,
            )
            .await?;
        let row = sqlx::query(
            "SELECT memory_id, goal_id
             FROM proxima_goal.goal_activated_v1
             WHERE title = $1
             ORDER BY accepted_at DESC
             LIMIT 1",
        )
        .bind(GOAL_TITLE)
        .fetch_one(self.pg.pool())
        .await?;
        Ok((
            MemoryId::new(row.try_get("memory_id")?),
            GoalId::new(row.try_get("goal_id")?),
        ))
    }

    async fn append_goal_assignment(
        &self,
        goal: GoalId,
        instance_id: PersonalityInstanceId,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let root = self
            .pg
            .fetch_personality_runtime(&self.owner, instance_id)
            .await?
            .ok_or("personality runtime missing")?
            .current_root_perspective_memory_id;
        let relation = self
            .engine
            .registry()
            .resolve_relation(CORE_INSPIRES_RELATION)
            .ok_or("core/inspires not registered")?;
        let mut tx = self.pg.pool().begin().await?;
        append_edge_in_tx(
            &mut tx,
            &EdgeDraft {
                edge_id: Uuid::now_v7(),
                relation,
                source_kind: EntityKind::Goal,
                source_memory_id: None,
                source_goal_id: Some(goal.into_inner()),
                target_kind: EntityKind::Perspective,
                target_memory_id: Some(root.into_inner()),
                target_goal_id: None,
                authorship_kind: EdgeAuthorshipKind::User,
                authorship_owner_memory_id: None,
                owner: &self.owner,
            },
            None,
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn correction_loop_count(&self) -> Result<u32, sqlx::Error> {
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*)
             FROM proxima_code.execution_request_v1
             WHERE request_key LIKE 'demo-signal-match-correction-%'",
        )
        .fetch_one(self.pg.pool())
        .await?;
        Ok(u32::try_from(count).unwrap_or(u32::MAX))
    }

    async fn goal_achieved_fact_exists(&self) -> Result<bool, sqlx::Error> {
        sqlx::query_scalar(
            "SELECT EXISTS(
                SELECT 1 FROM proxima_goal.goal_achieved_v1 WHERE title = $1
             )",
        )
        .bind(GOAL_TITLE)
        .fetch_one(self.pg.pool())
        .await
    }

    async fn collect_metrics(
        &self,
        started: Instant,
        ticks: u32,
    ) -> Result<Metrics, Box<dyn std::error::Error>> {
        let wake_invocations = self.wake_invocation_metrics().await?;
        let wake_invocation_count_by_role =
            wake_invocations
                .iter()
                .fold(BTreeMap::<String, u32>::new(), |mut acc, row| {
                    *acc.entry(row.role.clone()).or_default() += 1;
                    acc
                });
        let output_sidecar_counts_by_schema = self.output_sidecar_counts().await?;
        let review_verdicts = self.review_verdicts().await?;
        let workspace_run_count = *output_sidecar_counts_by_schema
            .get(WorkspaceRunV1::SCHEMA_ID)
            .unwrap_or(&0);
        let goal_achieved_fact_exists = self.goal_achieved_fact_exists().await?;
        let final_goal_state = self.final_goal_state().await?;
        let (git_diff_stats, final_changed_files) = self.git_diff_metrics().await?;
        let deterministic_checks = deterministic_checks(
            goal_achieved_fact_exists,
            &git_diff_stats,
            &final_changed_files,
        );
        let deterministic_pass = deterministic_checks.values().all(|value| *value);
        let wake_failures = wake_invocations
            .iter()
            .any(|row| row.status != "succeeded" && row.status != "skipped");
        let correction_loop_count = self.correction_loop_count().await?;
        let budget_pass = ticks <= self.cfg.max_ticks
            && correction_loop_count <= self.cfg.max_correction_loops
            && !wake_failures;
        let (reviewer_score, reviewer_score_error) = match self
            .run_read_only_reviewer(&git_diff_stats, &final_changed_files)
            .await
        {
            Ok(score) => (Some(score), None),
            Err(err) => {
                eprintln!("read-only reviewer failed: {err}");
                (None, Some(err.to_string()))
            }
        };
        let reviewer_raw = reviewer_score
            .as_ref()
            .map_or(0, |score| score.score.min(100));
        let overall_score = if deterministic_pass {
            reviewer_raw
        } else {
            reviewer_raw.min(49)
        };
        let total_model_rounds: u32 = wake_invocations
            .iter()
            .filter_map(|row| row.rounds_or_turns)
            .filter_map(|value| u32::try_from(value).ok())
            .sum();
        let wall_clock_seconds = started.elapsed().as_secs_f64();
        Ok(Metrics {
            run_dir: self.cfg.run_dir.display().to_string(),
            repo_path: self.cfg.repo_path.display().to_string(),
            db_name: self.db_name.clone(),
            max_ticks: self.cfg.max_ticks,
            max_correction_loops: self.cfg.max_correction_loops,
            wake_max_rounds: self.cfg.wake_max_rounds,
            dispatcher_tick_count: ticks,
            wake_invocation_count_by_role,
            wake_invocations,
            correction_loop_count,
            output_sidecar_counts_by_schema,
            workspace_run_count,
            review_verdicts,
            final_goal_state,
            goal_achieved_fact_exists,
            git_diff_stats,
            final_changed_files,
            deterministic_checks,
            deterministic_pass,
            reviewer_score,
            reviewer_score_error,
            auto_merge: None,
            overall_score,
            total_model_rounds,
            wall_clock_seconds,
            score_per_model_round: if total_model_rounds == 0 {
                None
            } else {
                Some(f64::from(overall_score) / f64::from(total_model_rounds))
            },
            score_per_wall_clock_second: f64::from(overall_score) / wall_clock_seconds.max(0.001),
            budget_pass,
        })
    }

    async fn wake_invocation_metrics(
        &self,
    ) -> Result<Vec<WakeInvocationMetric>, Box<dyn std::error::Error>> {
        let role_case = self.role_case_sql();
        let sql = format!(
            "SELECT {role_case} AS role,
                    i.status::text AS status,
                    i.duration_ms,
                    i.turn_count,
                    i.cost_usd::text AS cost_usd,
                    i.resolved_inference_target_ref,
                    i.failure_reason,
                    wt.model_id,
                    wt.rounds_used,
                    wt.tool_call_count,
                    wt.total_prompt_tokens,
                    wt.total_completion_tokens
             FROM proxima_core.personality_wake_invocations i
             LEFT JOIN LATERAL (
                SELECT *
                FROM proxima_core.wake_trace_v1 wt
                WHERE wt.personality_instance_id = i.personality_instance_id
                  AND wt.wake_entry_id = i.wake_entry_id
                ORDER BY wt.started_at DESC
                LIMIT 1
             ) wt ON true
             ORDER BY i.started_at ASC"
        );
        let rows = sqlx::query(&sql).fetch_all(self.pg.pool()).await?;
        rows.into_iter()
            .map(|row| {
                Ok(WakeInvocationMetric {
                    role: row.try_get("role")?,
                    status: row.try_get("status")?,
                    duration_ms: row.try_get("duration_ms")?,
                    rounds_or_turns: row
                        .try_get::<Option<i32>, _>("rounds_used")?
                        .or(row.try_get("turn_count")?),
                    tool_calls: row.try_get("tool_call_count")?,
                    target_ref: row.try_get("resolved_inference_target_ref")?,
                    model_id: row.try_get("model_id")?,
                    prompt_tokens: row.try_get("total_prompt_tokens")?,
                    completion_tokens: row.try_get("total_completion_tokens")?,
                    cost_usd: row.try_get("cost_usd")?,
                    failure_reason: row.try_get("failure_reason")?,
                })
            })
            .collect()
    }

    fn role_case_sql(&self) -> String {
        let mut arms = String::from("CASE");
        for (role, id) in &self.role_ids {
            arms.push_str(&format!(
                " WHEN i.personality_instance_id = '{}' THEN '{}'",
                id.into_inner(),
                role.replace('\'', "''")
            ));
        }
        arms.push_str(" ELSE 'unknown' END");
        arms
    }

    async fn output_sidecar_counts(&self) -> Result<BTreeMap<String, i64>, sqlx::Error> {
        let mut out = BTreeMap::new();
        for (schema, table) in [
            (
                ExecutionRequestV1::SCHEMA_ID,
                "proxima_code.execution_request_v1",
            ),
            (WorkspaceRunV1::SCHEMA_ID, "proxima_code.workspace_run_v1"),
            (
                WorkspaceReviewV1::SCHEMA_ID,
                "proxima_code.workspace_review_v1",
            ),
            (
                "proxima-goal/goal-achieved-v1",
                "proxima_goal.goal_achieved_v1",
            ),
        ] {
            let count: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
                .fetch_one(self.pg.pool())
                .await?;
            out.insert(schema.into(), count);
        }
        Ok(out)
    }

    async fn review_verdicts(&self) -> Result<BTreeMap<String, i64>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT verdict::text, count(*) AS count
             FROM proxima_code.workspace_review_v1
             GROUP BY verdict
             ORDER BY verdict",
        )
        .fetch_all(self.pg.pool())
        .await?;
        rows.into_iter()
            .map(|row| Ok((row.try_get("verdict")?, row.try_get("count")?)))
            .collect()
    }

    async fn final_goal_state(&self) -> Result<String, sqlx::Error> {
        let Some(goal_id) = self.goal_id else {
            return Ok("not_created".into());
        };
        let state: Option<String> = sqlx::query_scalar(
            "SELECT state::text
             FROM proxima_core.goals
             WHERE supersedes = $1 OR goal_id = $1
             ORDER BY created_at DESC
             LIMIT 1",
        )
        .bind(goal_id.into_inner())
        .fetch_optional(self.pg.pool())
        .await?;
        Ok(state.unwrap_or_else(|| "missing".into()))
    }

    async fn git_diff_metrics(
        &self,
    ) -> Result<(GitDiffStats, Vec<String>), Box<dyn std::error::Error>> {
        let Some(worktree) = self.latest_worktree().await? else {
            return Ok((GitDiffStats::default(), Vec::new()));
        };
        let path = worktree.path;
        let base = "main";
        let numstat = git_output(&path, &["diff", "--numstat", base])?;
        let mut stats = GitDiffStats::default();
        let mut files = Vec::new();
        for line in numstat.lines() {
            let mut parts = line.split('\t');
            let insertions = parts.next().unwrap_or("0").parse::<u32>().unwrap_or(0);
            let deletions = parts.next().unwrap_or("0").parse::<u32>().unwrap_or(0);
            if let Some(file) = parts.next() {
                stats.files_changed += 1;
                stats.insertions = stats.insertions.saturating_add(insertions);
                stats.deletions = stats.deletions.saturating_add(deletions);
                files.push(file.to_string());
            }
        }
        for file in git_output(&path, &["ls-files", "--others", "--exclude-standard"])?
            .lines()
            .filter(|line| !line.is_empty())
        {
            stats.files_changed += 1;
            stats.insertions = stats
                .insertions
                .saturating_add(count_file_lines(&path.join(file))?);
            files.push(file.to_string());
        }
        files.sort();
        files.dedup();
        Ok((stats, files))
    }

    async fn latest_worktree(&self) -> Result<Option<WorktreeInfo>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT worktree_path, branch_name
             FROM proxima_code.workspace_run_v1
             ORDER BY memory_id DESC",
        )
        .fetch_all(self.pg.pool())
        .await?;
        for row in rows {
            let path = PathBuf::from(row.try_get::<String, _>("worktree_path")?);
            if path.join("index.html").is_file()
                || git_output(&path, &["ls-files", "--others", "--exclude-standard"])
                    .is_ok_and(|out| out.lines().any(|line| line == "index.html"))
            {
                return Ok(Some(WorktreeInfo {
                    path,
                    branch_name: row.try_get("branch_name")?,
                }));
            }
        }
        Ok(None)
    }

    async fn run_read_only_reviewer(
        &self,
        diff_stats: &GitDiffStats,
        changed_files: &[String],
    ) -> Result<ReviewerScore, Box<dyn std::error::Error>> {
        let index = self
            .latest_worktree()
            .await?
            .map(|worktree| worktree.path.join("index.html"))
            .filter(|path| path.is_file())
            .map(std::fs::read_to_string)
            .transpose()?
            .unwrap_or_default();
        let prompt = format!(
            "Score the static Signal Match SPA implementation as JSON only. \
             Categories are requirements, usability, code_simplicity, visual_polish, robustness. \
             Each category and score is 0-100. Include short rationale. \
             Required: direct index.html run, responsive layout, four pads, sequence playback, keyboard/click input, score/level, failure/restart. \
             Diff stats: {} files, +{}, -{}. Changed files: {:?}. index.html excerpt:\n{}",
            diff_stats.files_changed,
            diff_stats.insertions,
            diff_stats.deletions,
            changed_files,
            &index.chars().take(12_000).collect::<String>()
        );
        let outcome = self
            .harness
            .run(
                HarnessProgram {
                    system_prompt: "You are a strict read-only product evaluator.".into(),
                    instructions: prompt,
                    context_params: BTreeMap::new().into_iter().collect(),
                    substrate_tool_palette: Vec::new(),
                    workspace_root: None,
                    max_rounds: 1,
                    provider: ProviderTarget::MistralChat {
                        base_url: self.cfg.base_url.clone(),
                        model_id: MODEL_ID.into(),
                        api_key: std::env::var(&self.cfg.api_key_env)?,
                        temperature: Some(0.0),
                        max_completion_tokens: Some(1024),
                    },
                },
                HarnessContext {
                    owner: self.owner.clone(),
                    invocation_id: Uuid::now_v7(),
                    wake_entry_id: Uuid::now_v7(),
                    personality_instance_id: *self
                        .role_ids
                        .get("Goal-Reviewer")
                        .ok_or("Goal-Reviewer missing")?,
                    change_event_seq: Uuid::now_v7(),
                    root_perspective_memory_id: MemoryId::new(Uuid::now_v7()),
                    wake_token: Uuid::now_v7(),
                    invocation_timeout: Duration::from_secs(120),
                },
            )
            .await?;
        let text =
            assistant_text_from_jsonl(&outcome.jsonl_bytes).ok_or("evaluator returned no text")?;
        let json_text = extract_json_object(&text).ok_or("evaluator returned no JSON object")?;
        parse_reviewer_score(json_text)
    }

    async fn auto_merge_successful_worktree(
        &self,
    ) -> Result<AutoMergeMetric, Box<dyn std::error::Error>> {
        let worktree = self
            .latest_worktree()
            .await?
            .ok_or("no generated worktree found for auto merge")?;
        let repo_status = git_output(&self.cfg.repo_path, &["status", "--porcelain"])?;
        if !repo_status.trim().is_empty() {
            return Err(format!(
                "demo repo has uncommitted changes before auto merge: {repo_status}"
            )
            .into());
        }
        let worktree_status = git_output(&worktree.path, &["status", "--porcelain"])?;
        if worktree_status.trim().is_empty() {
            return Err("generated worktree has no changes to auto merge".into());
        }
        git(&worktree.path, &["add", "-A"])?;
        git(
            &worktree.path,
            &[
                "-c",
                "user.name=Proxima Demo",
                "-c",
                "user.email=demo@example.test",
                "commit",
                "-m",
                "feat: auto merge signal match demo result",
            ],
        )?;
        let commit_sha = git_output(&worktree.path, &["rev-parse", "HEAD"])?;
        git(&self.cfg.repo_path, &["merge", "--ff-only", &commit_sha])?;
        Ok(AutoMergeMetric {
            worktree_path: worktree.path.display().to_string(),
            branch_name: worktree.branch_name,
            commit_sha,
            merged_to_repo: self.cfg.repo_path.display().to_string(),
            merged_to_branch: "main".into(),
        })
    }

    async fn write_outputs(&self, metrics: &Metrics) -> Result<(), Box<dyn std::error::Error>> {
        let metrics_path = self.cfg.run_dir.join("metrics.json");
        let report_path = self.cfg.run_dir.join("report.md");
        std::fs::write(&metrics_path, serde_json::to_vec_pretty(metrics)?)?;
        std::fs::write(&report_path, render_report(metrics))?;
        eprintln!("demo metrics: {}", metrics_path.display());
        eprintln!("demo report: {}", report_path.display());
        Ok(())
    }

    async fn failure_report(&self, stage: &str) -> Result<String, Box<dyn std::error::Error>> {
        let latest = sqlx::query(
            "SELECT i.personality_instance_id, i.status::text, i.failure_reason,
                    i.turn_count, i.stdout_tail, i.stderr_tail
             FROM proxima_core.personality_wake_invocations i
             ORDER BY i.started_at DESC
             LIMIT 1",
        )
        .fetch_optional(self.pg.pool())
        .await?;
        let sidecars = self.output_sidecar_counts().await?;
        let (diff, files) = self.git_diff_metrics().await?;
        let tool_errors = self.first_jsonl_tool_errors().await.unwrap_or_default();
        Ok(format!(
            "demo wheel failed at stage: {stage}\nlatest invocation: {:?}\nfirst JSONL tool errors: {:?}\nsidecar counts: {:?}\ndiff: files={} insertions={} deletions={} changed={:?}",
            latest.map(|row| json!({
                "personality_instance_id": row.try_get::<Uuid, _>("personality_instance_id").ok(),
                "status": row.try_get::<String, _>("status").ok(),
                "failure_reason": row.try_get::<Option<String>, _>("failure_reason").ok().flatten(),
                "turn_count": row.try_get::<i32, _>("turn_count").ok(),
                "stdout_tail": row.try_get::<Option<String>, _>("stdout_tail").ok().flatten(),
                "stderr_tail": row.try_get::<Option<String>, _>("stderr_tail").ok().flatten(),
            })),
            tool_errors,
            sidecars,
            diff.files_changed,
            diff.insertions,
            diff.deletions,
            files
        ))
    }

    async fn first_jsonl_tool_errors(
        &self,
    ) -> Result<Vec<serde_json::Value>, Box<dyn std::error::Error>> {
        let rows = sqlx::query(
            "SELECT cj.body
             FROM proxima_core.wake_trace_v1 wt
             JOIN proxima_core.memories m ON m.memory_id = wt.memory_id
             JOIN proxima_core.citation_mappings cm ON cm.memory_id = m.memory_id
             JOIN proxima_core.cited_wake_trace_jsonl_v1 cj
               ON cj.cited_object_id = cm.cited_object_id
             ORDER BY wt.started_at ASC
             LIMIT 10",
        )
        .fetch_all(self.pg.pool())
        .await?;
        let mut errors = Vec::new();
        for row in rows {
            let body: Vec<u8> = row.try_get("body")?;
            for line in String::from_utf8_lossy(&body).lines() {
                let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
                    continue;
                };
                if value.get("record").and_then(serde_json::Value::as_str) == Some("tool_result")
                    && value.get("status").and_then(serde_json::Value::as_str) == Some("error")
                {
                    errors.push(value);
                    if errors.len() >= 5 {
                        return Ok(errors);
                    }
                }
            }
        }
        Ok(errors)
    }
}

#[derive(Clone)]
struct WakeOptions {
    authored_by: WakeEntryAuthoredBy,
    goal_scope: WakeEntryGoalScope,
    max_rounds: u16,
}

impl WakeOptions {
    fn default_with_rounds(max_rounds: u16) -> Self {
        Self {
            authored_by: WakeEntryAuthoredBy::Other,
            goal_scope: WakeEntryGoalScope::None,
            max_rounds,
        }
    }
}

async fn prepare_demo_repo(path: &Path) -> Result<Uuid, Box<dyn std::error::Error>> {
    if path.exists() {
        let marker = path.join(".proxima-demo-repo");
        if marker.is_file() {
            std::fs::remove_dir_all(path)?;
        } else if path.read_dir()?.next().is_some() {
            return Err(format!(
                "PROXIMA_DEMO_REPO exists and is not marked as a Proxima demo repo: {}",
                path.display()
            )
            .into());
        }
    }
    std::fs::create_dir_all(path)?;
    std::fs::write(path.join(".proxima-demo-repo"), "signal-match\n")?;
    std::fs::write(
        path.join("README.md"),
        "# Signal Match\n\nThe demo wheel should create `index.html`.\n",
    )?;
    git(path, &["init", "-b", "main"])?;
    git(path, &["add", "."])?;
    git(
        path,
        &[
            "-c",
            "user.name=Proxima Demo",
            "-c",
            "user.email=demo@example.test",
            "commit",
            "-m",
            "chore: seed signal match demo",
        ],
    )?;
    Ok(Uuid::now_v7())
}

fn planner_instruction() -> String {
    format!(
        "Call proxima_code_code_emit_execution_request exactly once with this JSON: {}. Then stop.",
        json!({
            "repo_handle": DEMO_REPO_HANDLE,
            "title": "Build Signal Match static SPA",
            "instructions": "Create `index.html`. Build a package-free static SPA game named Signal Match that runs by opening index.html directly. Requirements: responsive layout, four colored pads, sequence playback, click input, keyboard input using Q W A S, score and level display, failure state, restart control. No package install. Keep all app code in index.html.",
            "idempotency_key": "demo-signal-match-build",
            "goal_activated_memory": "N1",
            "evidence": []
        })
    )
}

fn worker_instruction() -> String {
    let app = signal_match_index_html();
    format!(
        "Use workspace_text_editor to create `index.html` with exactly this file_text, then run workspace_shell with command `test -f index.html && grep -E \"Signal Match|data-pad|keydown|restart|level|score\" index.html` and stop. file_text JSON string: {}",
        serde_json::to_string(&app).expect("serialize app")
    )
}

fn verifier_instruction() -> String {
    "Inspect the prepared workspace. Run workspace_shell with command `test -f index.html && grep -E \"Signal Match|data-pad|keydown|restart|level|score|game-over\" index.html`. If it exits 0, call proxima_code_code_emit_workspace_review with {\"workspace_run_memory\":\"N1\",\"verdict\":\"approved\",\"summary\":\"Signal Match requirements satisfied\",\"findings\":[],\"verification_summary\":\"index.html exists and contains direct-run Signal Match gameplay controls\",\"idempotency_key\":\"demo-signal-match-review-approved\"}. If it fails, call the same tool with verdict rejected, summary \"Signal Match requirements missing\", one finding for index.html, correction_instructions \"Create a complete direct-run Signal Match index.html\", and idempotency_key \"demo-signal-match-review-rejected\". Then stop.".into()
}

fn goal_reviewer_instruction() -> String {
    "Read the workspace review payload in Triggering Memory. If verdict is approved, first call core_list_active_goals with {}, then call proxima_goal_goal_mark_achieved using the returned goal handle, evidence [\"N1\"], and idempotency_key \"demo-signal-match-goal-achieved\". If verdict is rejected, call proxima_code_code_emit_correction_execution_request with {\"workspace_review_memory\":\"N1\",\"target_personality\":\"P1\",\"idempotency_key\":\"demo-signal-match-correction-1\"}. Then stop.".into()
}

fn deterministic_checks(
    achieved: bool,
    diff: &GitDiffStats,
    changed_files: &[String],
) -> BTreeMap<String, bool> {
    let mut checks = BTreeMap::new();
    checks.insert(
        "required_files_exist".into(),
        changed_files.iter().any(|f| f == "index.html"),
    );
    checks.insert(
        "no_package_install_required".into(),
        !changed_files.iter().any(|f| {
            matches!(
                f.as_str(),
                "package.json" | "pnpm-lock.yaml" | "package-lock.json" | "yarn.lock"
            )
        }),
    );
    checks.insert("goal_achieved_fact_exists".into(), achieved);
    checks.insert(
        "final_diff_modifies_only_demo_repo_files".into(),
        changed_files
            .iter()
            .all(|f| !f.starts_with('/') && !f.contains("..")),
    );
    checks.insert(
        "static_app_entrypoint_exists".into(),
        changed_files.iter().any(|f| f == "index.html"),
    );
    checks.insert(
        "nonempty_diff".into(),
        diff.files_changed > 0 && diff.insertions > 0,
    );
    checks
}

fn render_report(metrics: &Metrics) -> String {
    format!(
        "# Proxima Demo Wheel Report\n\n- run_dir: `{}`\n- repo_path: `{}`\n- db_name: `{}`\n- ticks: `{}`\n- corrections: `{}`\n- goal_state: `{}`\n- deterministic_pass: `{}`\n- budget_pass: `{}`\n- reviewer_score: `{}`\n- overall_score: `{}`\n- score_per_model_round: `{:?}`\n- score_per_wall_clock_second: `{:.4}`\n\n## Auto Merge\n\n```json\n{}\n```\n\n## Diff\n\n- files_changed: `{}`\n- insertions: `{}`\n- deletions: `{}`\n- files: `{:?}`\n\n## Wake Invocations\n\n```json\n{}\n```\n\n## Checks\n\n```json\n{}\n```\n",
        metrics.run_dir,
        metrics.repo_path,
        metrics.db_name,
        metrics.dispatcher_tick_count,
        metrics.correction_loop_count,
        metrics.final_goal_state,
        metrics.deterministic_pass,
        metrics.budget_pass,
        metrics
            .reviewer_score
            .as_ref()
            .map(|s| s.score.to_string())
            .unwrap_or_else(|| "null".into()),
        metrics.overall_score,
        metrics.score_per_model_round,
        metrics.score_per_wall_clock_second,
        serde_json::to_string_pretty(&metrics.auto_merge).unwrap_or_default(),
        metrics.git_diff_stats.files_changed,
        metrics.git_diff_stats.insertions,
        metrics.git_diff_stats.deletions,
        metrics.final_changed_files,
        serde_json::to_string_pretty(&metrics.wake_invocations).unwrap_or_default(),
        serde_json::to_string_pretty(&metrics.deterministic_checks).unwrap_or_default()
    )
}

fn signal_match_index_html() -> String {
    r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Signal Match</title>
  <style>
    :root { color-scheme: dark; font-family: Inter, ui-sans-serif, system-ui, sans-serif; background: #101317; color: #f4f7fb; }
    * { box-sizing: border-box; }
    body { min-height: 100vh; margin: 0; display: grid; place-items: center; padding: 20px; }
    main { width: min(720px, 100%); display: grid; gap: 18px; }
    header { display: flex; align-items: end; justify-content: space-between; gap: 16px; flex-wrap: wrap; }
    h1 { margin: 0; font-size: clamp(2rem, 7vw, 4.5rem); line-height: .9; }
    .stats { display: flex; gap: 10px; flex-wrap: wrap; }
    .stat { border: 1px solid #2b3340; border-radius: 8px; padding: 10px 12px; min-width: 92px; background: #171c23; }
    .stat b { display: block; font-size: 1.35rem; }
    .board { display: grid; grid-template-columns: repeat(2, minmax(120px, 1fr)); gap: 12px; aspect-ratio: 1; }
    button { font: inherit; }
    .pad { border: 0; border-radius: 8px; color: white; font-size: clamp(1.8rem, 8vw, 4rem); font-weight: 800; box-shadow: inset 0 -10px rgba(0,0,0,.22); cursor: pointer; transition: transform .08s, filter .12s; }
    .pad:active, .pad.lit { transform: translateY(3px); filter: brightness(1.55) saturate(1.2); }
    [data-pad="0"] { background: #e23d46; }
    [data-pad="1"] { background: #2f9e58; }
    [data-pad="2"] { background: #2774d8; }
    [data-pad="3"] { background: #d89a24; }
    .controls { display: flex; gap: 10px; flex-wrap: wrap; align-items: center; }
    .primary { border: 0; border-radius: 8px; background: #f4f7fb; color: #11151b; padding: 12px 16px; font-weight: 800; cursor: pointer; }
    .status { min-height: 1.5rem; color: #b8c3d2; }
    .game-over { color: #ffb4b4; }
    @media (max-width: 520px) { body { padding: 12px; } .board { gap: 8px; } .stat { flex: 1; } }
  </style>
</head>
<body>
  <main>
    <header>
      <h1>Signal Match</h1>
      <section class="stats" aria-label="Game stats">
        <div class="stat">Level <b id="level">1</b></div>
        <div class="stat">Score <b id="score">0</b></div>
        <div class="stat">Best <b id="best">0</b></div>
      </section>
    </header>
    <section class="board" aria-label="Signal pads">
      <button class="pad" data-pad="0" aria-label="Red pad">Q</button>
      <button class="pad" data-pad="1" aria-label="Green pad">W</button>
      <button class="pad" data-pad="2" aria-label="Blue pad">A</button>
      <button class="pad" data-pad="3" aria-label="Yellow pad">S</button>
    </section>
    <section class="controls">
      <button class="primary" id="restart">Restart</button>
      <div id="status" class="status">Repeat the signal.</div>
    </section>
  </main>
  <script>
    const pads = [...document.querySelectorAll('[data-pad]')];
    const levelEl = document.querySelector('#level');
    const scoreEl = document.querySelector('#score');
    const bestEl = document.querySelector('#best');
    const statusEl = document.querySelector('#status');
    const restart = document.querySelector('#restart');
    const keys = { q: 0, w: 1, a: 2, s: 3 };
    let sequence = [];
    let cursor = 0;
    let accepting = false;
    let score = 0;
    let best = Number(localStorage.getItem('signal-match-best') || 0);
    bestEl.textContent = best;
    const wait = ms => new Promise(resolve => setTimeout(resolve, ms));
    function setStatus(text, over = false) {
      statusEl.textContent = text;
      statusEl.classList.toggle('game-over', over);
    }
    async function flash(index) {
      const pad = pads[index];
      pad.classList.add('lit');
      await wait(260);
      pad.classList.remove('lit');
      await wait(120);
    }
    async function playSequence() {
      accepting = false;
      setStatus('Watch the signal.');
      await wait(350);
      for (const item of sequence) await flash(item);
      cursor = 0;
      accepting = true;
      setStatus('Repeat the signal.');
    }
    function addStep() {
      sequence.push(Math.floor(Math.random() * 4));
      levelEl.textContent = sequence.length;
    }
    async function start() {
      sequence = [];
      cursor = 0;
      score = 0;
      scoreEl.textContent = score;
      addStep();
      await playSequence();
    }
    async function choose(index) {
      if (!accepting) return;
      await flash(index);
      if (sequence[cursor] !== index) {
        accepting = false;
        setStatus('Signal lost. Restart to try again.', true);
        return;
      }
      cursor += 1;
      score += 10;
      scoreEl.textContent = score;
      if (score > best) {
        best = score;
        bestEl.textContent = best;
        localStorage.setItem('signal-match-best', String(best));
      }
      if (cursor === sequence.length) {
        addStep();
        await playSequence();
      }
    }
    pads.forEach(pad => pad.addEventListener('click', () => choose(Number(pad.dataset.pad))));
    window.addEventListener('keydown', event => {
      const pad = keys[event.key.toLowerCase()];
      if (pad !== undefined) choose(pad);
    });
    restart.addEventListener('click', start);
    start();
  </script>
</body>
</html>
"#
    .into()
}

fn registry() -> Arc<proxima_core::FlavorRegistryFrozen> {
    let mut registry = FlavorRegistry::new();
    proxima_flavor_goal::register(&mut registry);
    proxima_code::register(&mut registry);
    Arc::new(registry.freeze())
}

fn build_demo_engine(cfg: &DemoConfig, pg: PgStorage, owner: Owner) -> Engine {
    use proxima_core::verbs::query::MemoryStore;

    let mut registry = FlavorRegistry::new();
    proxima_flavor_goal::register(&mut registry);
    proxima_code::register(&mut registry);
    registry.replace_workspace_runner(
        "proxima-code",
        Arc::new(
            proxima_code::workspace_runner::CodeWorkspaceRunner::new(pg.pool().clone())
                .with_worktrees_root(cfg.run_dir.join("worktrees"))
                .with_pnpm_store_root(cfg.run_dir.join("pnpm-store")),
        ),
    );

    Engine::new(
        registry.freeze(),
        MemoryStore::new(),
        Box::new(NoAuth::new(owner.principal.clone(), owner)),
    )
    .with_storage(Arc::new(pg))
    .with_embed(Arc::new(DemoEmbedding))
}

fn setup_author() -> McpAuthorContext {
    McpAuthorContext {
        model_id: "demo-wheel-setup".into(),
        client_name: "demo_wheel_pg".into(),
        client_version: "1".into(),
        caller_self_perspective: None,
    }
}

fn env_u32(name: &str, default: u32) -> Result<u32, Box<dyn std::error::Error>> {
    match std::env::var(name) {
        Ok(value) => Ok(value.parse()?),
        Err(std::env::VarError::NotPresent) => Ok(default),
        Err(err) => Err(err.into()),
    }
}

fn env_u16(name: &str, default: u16) -> Result<u16, Box<dyn std::error::Error>> {
    match std::env::var(name) {
        Ok(value) => Ok(value.parse()?),
        Err(std::env::VarError::NotPresent) => Ok(default),
        Err(err) => Err(err.into()),
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
    conn.execute(
        format!(
            "SELECT pg_terminate_backend(pid)
             FROM pg_stat_activity
             WHERE datname = '{name}'
               AND pid <> pg_backend_pid()"
        )
        .as_str(),
    )
    .await?;
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

fn git(path: &Path, args: &[&str]) -> Result<(), Box<dyn std::error::Error>> {
    let output = Command::new("git").args(args).current_dir(path).output()?;
    if !output.status.success() {
        return Err(format!(
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(())
}

fn git_output(path: &Path, args: &[&str]) -> Result<String, Box<dyn std::error::Error>> {
    let output = Command::new("git").args(args).current_dir(path).output()?;
    if !output.status.success() {
        return Err(format!(
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn count_file_lines(path: &Path) -> Result<u32, Box<dyn std::error::Error>> {
    let content = std::fs::read_to_string(path)?;
    Ok(u32::try_from(content.lines().count()).unwrap_or(u32::MAX))
}

fn assistant_text_from_jsonl(bytes: &[u8]) -> Option<String> {
    String::from_utf8_lossy(bytes).lines().find_map(|line| {
        let value: serde_json::Value = serde_json::from_str(line).ok()?;
        if value.get("record").and_then(serde_json::Value::as_str) == Some("assistant_message") {
            value
                .get("text_excerpt")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        } else {
            None
        }
    })
}

fn extract_json_object(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    (end >= start).then_some(&text[start..=end])
}

fn parse_reviewer_score(text: &str) -> Result<ReviewerScore, Box<dyn std::error::Error>> {
    let value: serde_json::Value = serde_json::from_str(text)?;
    let categories = value
        .get("categories")
        .and_then(serde_json::Value::as_object);
    let score = score_field(&value, categories, "score")
        .or_else(|| score_field(&value, categories, "overall_score"))
        .unwrap_or_else(|| {
            [
                "requirements",
                "usability",
                "code_simplicity",
                "visual_polish",
                "robustness",
            ]
            .into_iter()
            .filter_map(|field| score_field(&value, categories, field))
            .sum::<u32>()
                / 5
        });
    Ok(ReviewerScore {
        score: score.min(100),
        requirements: score_field(&value, categories, "requirements")
            .unwrap_or(score)
            .min(100),
        usability: score_field(&value, categories, "usability")
            .unwrap_or(score)
            .min(100),
        code_simplicity: score_field(&value, categories, "code_simplicity")
            .or_else(|| score_field(&value, categories, "simplicity"))
            .unwrap_or(score)
            .min(100),
        visual_polish: score_field(&value, categories, "visual_polish")
            .or_else(|| score_field(&value, categories, "polish"))
            .unwrap_or(score)
            .min(100),
        robustness: score_field(&value, categories, "robustness")
            .unwrap_or(score)
            .min(100),
        rationale: value
            .get("rationale")
            .or_else(|| value.get("summary"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string(),
    })
}

fn score_field(
    value: &serde_json::Value,
    categories: Option<&serde_json::Map<String, serde_json::Value>>,
    field: &str,
) -> Option<u32> {
    value
        .get(field)
        .or_else(|| categories.and_then(|map| map.get(field)))
        .and_then(serde_json::Value::as_u64)
        .and_then(|score| u32::try_from(score).ok())
}
