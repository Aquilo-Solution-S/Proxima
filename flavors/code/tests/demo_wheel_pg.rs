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
    BindInferenceTierRequest, BudgetDecisionV1, BudgetExhaustionPolicy, BudgetReviewRequestedV1,
    CORE_INSPIRES_RELATION, Credentials, EdgeAuthorshipKind, Engine, EntityKind, FactPayload,
    FlavorRegistry, GoalId, InferenceTargetConfig, MemoryId, MistralChatConfig, ModelTier, OrgId,
    Owner, PersonalityInstanceId, Principal, RegisterInferenceTargetRequest, UserId,
    WakeEntryAuthoredBy, WakeEntryGoalScope, WakeEntryTriggerKind, WakeExecutionMode,
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
const SIGNAL_MATCH_REPO_HANDLE: &str = "signal-match-demo";
const SIGNAL_MATCH_GOAL_TITLE: &str = "Signal Match static SPA demo";
const TODO_CLI_REPO_HANDLE: &str = "todo-audit-demo";
const TODO_CLI_GOAL_TITLE: &str = "Todo Audit CLI demo";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DemoChallenge {
    SignalMatch,
    TodoCli,
}

impl DemoChallenge {
    fn from_env() -> Result<Self, Box<dyn std::error::Error>> {
        match std::env::var("PROXIMA_DEMO_CHALLENGE")
            .unwrap_or_else(|_| "signal_match".into())
            .as_str()
        {
            "signal_match" => Ok(Self::SignalMatch),
            "todo_cli" => Ok(Self::TodoCli),
            value => Err(format!(
                "unsupported PROXIMA_DEMO_CHALLENGE {value:?}; expected signal_match or todo_cli"
            )
            .into()),
        }
    }

    fn repo_handle(self) -> &'static str {
        match self {
            Self::SignalMatch => SIGNAL_MATCH_REPO_HANDLE,
            Self::TodoCli => TODO_CLI_REPO_HANDLE,
        }
    }

    fn default_repo_name(self) -> &'static str {
        match self {
            Self::SignalMatch => "signal-match-repo",
            Self::TodoCli => "todo-audit-repo",
        }
    }

    fn marker(self) -> &'static str {
        match self {
            Self::SignalMatch => "signal-match\n",
            Self::TodoCli => "todo-audit-cli\n",
        }
    }

    fn goal_title(self) -> &'static str {
        match self {
            Self::SignalMatch => SIGNAL_MATCH_GOAL_TITLE,
            Self::TodoCli => TODO_CLI_GOAL_TITLE,
        }
    }

    fn goal_text(self) -> &'static str {
        match self {
            Self::SignalMatch => {
                "Build a package-free static SPA game named Signal Match in index.html. It must run by opening index.html directly, be responsive, have four colored pads, sequence playback, click and keyboard input, score and level display, failure state, and restart."
            }
            Self::TodoCli => {
                "Build a package-free Node.js CLI named Todo Audit. It must parse Markdown task lists, extract completion state, owners, tags, priorities, due dates, and output deterministic JSON summaries with tests and sample fixtures."
            }
        }
    }

    fn worktree_has_primary_output(self, path: &Path) -> bool {
        self.required_files()
            .iter()
            .any(|file| path.join(file).is_file())
            || git_output(path, &["ls-files", "--others", "--exclude-standard"]).is_ok_and(|out| {
                out.lines()
                    .any(|line| self.required_files().iter().any(|file| line == *file))
            })
    }

    fn required_files(self) -> &'static [&'static str] {
        match self {
            Self::SignalMatch => &["index.html"],
            Self::TodoCli => &["todo_audit.mjs", "test_todo_audit.mjs"],
        }
    }

    fn reviewer_prompt(
        self,
        worktree: Option<WorktreeInfo>,
        diff_stats: &GitDiffStats,
        changed_files: &[String],
    ) -> Result<String, Box<dyn std::error::Error>> {
        let excerpt = match (self, worktree) {
            (Self::SignalMatch, Some(worktree)) => read_excerpt(&worktree.path.join("index.html"))?,
            (Self::TodoCli, Some(worktree)) => {
                let mut text = String::new();
                for file in ["todo_audit.mjs", "test_todo_audit.mjs", "examples/tasks.md"] {
                    text.push_str(&format!("\n--- {file} ---\n"));
                    text.push_str(&read_excerpt(&worktree.path.join(file))?);
                }
                text
            }
            (_, None) => String::new(),
        };
        let required = match self {
            Self::SignalMatch => {
                "direct index.html run, responsive layout, four pads, sequence playback, keyboard/click input, score/level, failure/restart"
            }
            Self::TodoCli => {
                "Node CLI with no package install, Markdown task parser, owners, tags, priority and due-date extraction, deterministic JSON output, sample fixture, and runnable tests"
            }
        };
        Ok(format!(
            "Score the {} implementation as JSON only. \
             Categories are requirements, usability, code_simplicity, visual_polish, robustness. \
             Each category and score is 0-100. Include short rationale. \
             Required: {required}. \
             Diff stats: {} files, +{}, -{}. Changed files: {:?}. Excerpt:\n{}",
            self.goal_title(),
            diff_stats.files_changed,
            diff_stats.insertions,
            diff_stats.deletions,
            changed_files,
            excerpt.chars().take(14_000).collect::<String>()
        ))
    }
}

fn read_excerpt(path: &Path) -> Result<String, Box<dyn std::error::Error>> {
    if path.is_file() {
        Ok(std::fs::read_to_string(path)?)
    } else {
        Ok(String::new())
    }
}

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
    challenge: DemoChallenge,
    repo_path: PathBuf,
    run_dir: PathBuf,
    base_url: String,
    api_key_env: String,
    max_ticks: u32,
    max_correction_loops: u32,
    role_max_rounds: RoleMaxRounds,
}

#[derive(Debug, Clone, Copy, Serialize)]
struct RoleMaxRounds {
    planner: u16,
    worker: u16,
    verifier: u16,
    goal_reviewer: u16,
    budgeter: u16,
}

#[derive(Debug, Serialize)]
struct Metrics {
    run_dir: String,
    repo_path: String,
    db_name: String,
    max_ticks: u32,
    max_correction_loops: u32,
    role_max_rounds: RoleMaxRounds,
    dispatcher_tick_count: u32,
    wake_invocation_count_by_role: BTreeMap<String, u32>,
    wake_invocations: Vec<WakeInvocationMetric>,
    terminal_guard_hits: BTreeMap<String, u32>,
    correction_loop_count: u32,
    output_sidecar_counts_by_schema: BTreeMap<String, i64>,
    workspace_run_count: i64,
    request_flow_counts: Vec<RequestFlowCount>,
    review_verdicts: BTreeMap<String, i64>,
    final_goal_state: String,
    goal_achieved_fact_exists: bool,
    goal_graph: GoalGraphMetrics,
    git_diff_stats: GitDiffStats,
    final_changed_files: Vec<String>,
    deterministic_checks: BTreeMap<String, bool>,
    deterministic_pass: bool,
    functional_pass: bool,
    flow_graph_json: String,
    flow_graph_mermaid: String,
    flow_graph_summary: FlowGraphSummary,
    reviewer_score: Option<ReviewerScore>,
    reviewer_score_error: Option<String>,
    auto_merge: Option<AutoMergeMetric>,
    overall_score: u32,
    total_model_rounds: u32,
    wall_clock_seconds: f64,
    score_per_model_round: Option<f64>,
    score_per_wall_clock_second: f64,
    budget_pass: bool,
    overall_pass: bool,
}

#[derive(Debug, Serialize)]
struct RequestFlowCount {
    request_memory_id: String,
    title: String,
    workspace_run_count: i64,
    workspace_review_count: i64,
    terminal_review_count: i64,
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

fn terminal_guard_hits(invocations: &[WakeInvocationMetric]) -> BTreeMap<String, u32> {
    let mut hits = BTreeMap::new();
    for invocation in invocations {
        let Some(reason) = invocation.failure_reason.as_deref() else {
            continue;
        };
        let key = if reason.contains("terminal workspace review") {
            "terminal_review"
        } else if reason.contains("already has a workspace run")
            || reason.contains("derived workspace run")
        {
            "duplicate_workspace_run"
        } else {
            continue;
        };
        *hits.entry(key.to_string()).or_default() += 1;
    }
    hits
}

#[derive(Debug, Default, Serialize)]
struct FlowGraphSummary {
    node_count: usize,
    edge_count: usize,
    personality_count: usize,
    goal_count: usize,
    execution_request_count: usize,
    workspace_run_count: usize,
    workspace_review_count: usize,
    verification_evidence_count: usize,
    budget_review_count: usize,
    budget_decision_count: usize,
    wake_invocation_count: usize,
    unresolved_endpoint_count: usize,
}

#[derive(Debug, Default, Clone, Serialize)]
struct GoalGraphMetrics {
    child_goal_count: i64,
    achieved_child_goal_count: i64,
    child_execution_request_count: i64,
    child_workspace_run_count: i64,
    child_workspace_review_count: i64,
    verification_evidence_count: i64,
}

impl GoalGraphMetrics {
    fn complete(&self, parent_achieved: bool) -> bool {
        parent_achieved
            && self.child_goal_count >= 2
            && self.achieved_child_goal_count == self.child_goal_count
            && self.child_execution_request_count >= 2
            && self.child_workspace_run_count >= 2
            && self.child_workspace_review_count >= 2
            && self.verification_evidence_count >= 1
    }
}

#[derive(Debug, Serialize)]
struct FlowGraph {
    nodes: Vec<FlowNode>,
    edges: Vec<FlowEdge>,
    events: Vec<FlowEvent>,
    summary: FlowGraphSummary,
}

#[derive(Debug, Serialize)]
struct FlowNode {
    id: String,
    kind: String,
    label: String,
    role: Option<String>,
    schema_id: Option<String>,
    state: Option<String>,
    status: Option<String>,
}

#[derive(Debug, Serialize)]
struct FlowEdge {
    id: String,
    source: String,
    target: String,
    relation: String,
    persisted_edge_id: Option<String>,
}

#[derive(Debug, Serialize)]
struct FlowEvent {
    seq: String,
    kind: String,
    entity_kind: Option<String>,
    entity_id: Option<String>,
    schema_id: Option<String>,
    edge_relation: Option<String>,
    personality_instance_id: Option<String>,
    wake_chain_depth: i16,
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
async fn measurable_complex_demo_wheel() -> Result<(), Box<dyn std::error::Error>> {
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
        let challenge = DemoChallenge::from_env()?;

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
            .unwrap_or_else(|_| run_dir.join(challenge.default_repo_name()));
        let base_url = std::env::var("MISTRAL_BASE_URL")
            .unwrap_or_else(|_| "https://api.mistral.ai".to_string())
            .trim_end_matches('/')
            .trim_end_matches("/v1")
            .to_string();
        Ok(Some(Self {
            challenge,
            repo_path,
            run_dir,
            base_url,
            api_key_env: "MISTRAL_API_KEY".into(),
            max_ticks: env_u32("PROXIMA_DEMO_MAX_TICKS", 12)?,
            max_correction_loops: env_u32("PROXIMA_DEMO_MAX_CORRECTION_LOOPS", 2)?,
            role_max_rounds: RoleMaxRounds::from_env()?,
        }))
    }
}

impl RoleMaxRounds {
    fn from_env() -> Result<Self, Box<dyn std::error::Error>> {
        let fallback = env_optional_u16("PROXIMA_DEMO_WAKE_MAX_ROUNDS")?;
        Ok(Self {
            planner: env_u16_with_fallback("PROXIMA_DEMO_PLANNER_MAX_ROUNDS", 8, fallback)?,
            worker: env_u16_with_fallback("PROXIMA_DEMO_WORKER_MAX_ROUNDS", 14, fallback)?,
            verifier: env_u16_with_fallback("PROXIMA_DEMO_VERIFIER_MAX_ROUNDS", 5, fallback)?,
            goal_reviewer: env_u16_with_fallback(
                "PROXIMA_DEMO_GOAL_REVIEWER_MAX_ROUNDS",
                5,
                fallback,
            )?,
            budgeter: env_u16_with_fallback("PROXIMA_DEMO_BUDGETER_MAX_ROUNDS", 3, fallback)?,
        })
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
        let repo_id = prepare_demo_repo(&self.cfg.repo_path, self.cfg.challenge).await?;
        register_repo(
            self.pg.pool(),
            &self.owner,
            repo_id,
            self.cfg.repo_path.to_str().ok_or("repo path is not utf8")?,
            self.cfg.challenge.repo_handle(),
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
        let budgeter = self
            .instantiate(
                "Budgeter",
                "Decide whether max-round wake truncations deserve more budget",
            )
            .await?;
        self.set_budgeter_wake(budgeter).await?;
        let budget_policy = demo_budget_policy(budgeter);

        self.set_single_wake(
            planner,
            "Planner",
            WakeEntryTriggerKind::OnMemory,
            "proxima-goal/goal-activated-v1",
            WakeExecutionMode::SubstrateOnly,
            vec![
                "proxima-goal/goal_decompose",
                "proxima-code/code_emit_execution_request",
            ],
            Vec::new(),
            planner_instruction(planner, self.cfg.challenge),
            WakeOptions {
                goal_scope: WakeEntryGoalScope::TriggerGoalAssigned,
                authored_by: WakeEntryAuthoredBy::Any,
                budget_policy: Some(budget_policy.clone()),
                ..WakeOptions::default_with_rounds(self.cfg.role_max_rounds.planner)
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
            worker_instruction(self.cfg.challenge),
            WakeOptions {
                budget_policy: Some(budget_policy.clone()),
                ..WakeOptions::default_with_rounds(self.cfg.role_max_rounds.worker)
            },
        )
        .await?;
        self.set_single_wake(
            verifier,
            "Verifier",
            WakeEntryTriggerKind::OnMemory,
            WorkspaceRunV1::SCHEMA_ID,
            WakeExecutionMode::Workspace,
            vec![
                "proxima-code/code_emit_verification_evidence",
                "proxima-code/code_emit_workspace_review",
            ],
            vec![
                "proxima-workspace/text_editor",
                "proxima-workspace/shell",
                "proxima-workspace/list_files",
            ],
            verifier_instruction(self.cfg.challenge),
            WakeOptions {
                budget_policy: Some(budget_policy.clone()),
                ..WakeOptions::default_with_rounds(self.cfg.role_max_rounds.verifier)
            },
        )
        .await?;

        let (_goal_memory, active_goal) = self.activate_goal(planner).await?;
        self.goal_id = Some(active_goal);
        self.append_goal_assignment(active_goal, reviewer).await?;
        self.set_goal_reviewer_wakes(reviewer, budget_policy)
            .await?;

        let mut ticks = 0_u32;
        while ticks < self.cfg.max_ticks {
            ticks += 1;
            let fired = self.engine.run_dispatcher_tick().await?;
            let correction_loops = self.correction_loop_count().await?;
            if self.demo_goal_graph_complete().await? {
                let mut metrics = self.collect_metrics(started, ticks).await?;
                if !metrics.overall_pass {
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
        wake.budget_policy = options.budget_policy;
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

    async fn set_budgeter_wake(
        &self,
        budgeter: PersonalityInstanceId,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut wake = WakeEntryDraft::new(
            Uuid::now_v7(),
            budgeter,
            WakeEntryTriggerKind::OnMemory,
            BudgetReviewRequestedV1::SCHEMA_ID,
            "Budgeter demo wake",
            WakeEntryAuthoredBy::Any,
            1000,
            ModelTier::Standard,
            Some(TARGET_REF.into()),
            vec!["core/emit_budget_decision".into()],
            self.cfg.role_max_rounds.budgeter,
        )?;
        wake.instructions = budgeter_instruction();
        self.engine
            .set_wake_entries(
                &Credentials::None,
                &SetWakeEntriesRequest {
                    owner: self.owner.clone(),
                    personality_instance_id: budgeter,
                    entries: vec![wake],
                },
            )
            .await?;
        Ok(())
    }

    async fn set_goal_reviewer_wakes(
        &self,
        reviewer: PersonalityInstanceId,
        budget_policy: BudgetExhaustionPolicy,
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
                "proxima-code/code_goal_completion_status".into(),
                "proxima-goal/goal_mark_achieved".into(),
                "proxima-code/code_emit_correction_execution_request".into(),
            ],
            self.cfg.role_max_rounds.goal_reviewer,
        )?;
        review_wake.instructions = goal_reviewer_instruction();
        review_wake.budget_policy = Some(budget_policy);

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
                            "title": self.cfg.challenge.goal_title(),
                            "text": self.cfg.challenge.goal_text()
                        }
                    },
                    "target_personality": planner.into_inner().to_string(),
                    "evidence": [],
                    "idempotency_key": format!("demo-{}-propose", self.cfg.challenge.repo_handle())
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
                    "idempotency_key": format!("demo-{}-accept", self.cfg.challenge.repo_handle())
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
        .bind(self.cfg.challenge.goal_title())
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
        .bind(self.cfg.challenge.goal_title())
        .fetch_one(self.pg.pool())
        .await
    }

    async fn demo_goal_graph_complete(&self) -> Result<bool, sqlx::Error> {
        let parent_achieved = self.goal_achieved_fact_exists().await?;
        let graph = self.goal_graph_metrics().await?;
        Ok(graph.complete(parent_achieved))
    }

    async fn goal_graph_metrics(&self) -> Result<GoalGraphMetrics, sqlx::Error> {
        let Some(parent_goal) = self.goal_id else {
            return Ok(GoalGraphMetrics::default());
        };
        let row = sqlx::query(
            "WITH RECURSIVE child_roots AS (
                 SELECT gp.goal_id AS root_goal_id
                   FROM proxima_core.goal_parents gp
                  WHERE gp.parent_goal_id = $1
             ),
             child_lineage(root_goal_id, goal_id, depth, path) AS (
                 SELECT root_goal_id, root_goal_id, 0, ARRAY[root_goal_id]
                   FROM child_roots
                 UNION ALL
                 SELECT l.root_goal_id, child.goal_id, l.depth + 1, l.path || child.goal_id
                   FROM child_lineage l
                   JOIN proxima_core.goals child
                     ON child.supersedes = l.goal_id
                  WHERE NOT child.goal_id = ANY(l.path)
             ),
             child_heads AS (
                 SELECT DISTINCT ON (l.root_goal_id)
                        l.root_goal_id,
                        g.goal_id AS head_goal_id,
                        g.state
                   FROM child_lineage l
                   JOIN proxima_core.goals g ON g.goal_id = l.goal_id
                  ORDER BY l.root_goal_id, l.depth DESC, g.created_at DESC
             ),
             child_activations AS (
                 SELECT ga.memory_id, ga.goal_id
                   FROM proxima_goal.goal_activated_v1 ga
                   JOIN child_roots cr ON cr.root_goal_id = ga.goal_id
             ),
             child_requests AS (
                 SELECT DISTINCT er.memory_id
                   FROM proxima_code.execution_request_v1 er
                   JOIN proxima_core.edges e
                     ON e.source_kind = 'Fact'
                    AND e.source_memory_id = er.memory_id
                    AND e.target_kind = 'Fact'
                    AND e.target_memory_id IN (SELECT memory_id FROM child_activations)
                    AND e.relation = 'core/derived-from'
             ),
             child_runs AS (
                 SELECT DISTINCT wr.memory_id
                   FROM proxima_code.workspace_run_v1 wr
                   JOIN proxima_core.edges e
                     ON e.source_kind = 'Fact'
                    AND e.source_memory_id = wr.memory_id
                    AND e.target_kind = 'Fact'
                    AND e.target_memory_id IN (SELECT memory_id FROM child_requests)
                    AND e.relation = 'core/derived-from'
             )
             SELECT
                (SELECT count(*) FROM child_roots) AS child_goal_count,
                (SELECT count(*) FROM child_heads WHERE state = 'Achieved') AS achieved_child_goal_count,
                (SELECT count(*) FROM child_requests) AS child_execution_request_count,
                (SELECT count(*) FROM child_runs) AS child_workspace_run_count,
                (SELECT count(*) FROM proxima_code.workspace_review_v1
                  WHERE execution_request_memory_id IN (SELECT memory_id FROM child_requests)) AS child_workspace_review_count,
                (SELECT count(*) FROM proxima_code.verification_evidence_v1
                  WHERE execution_request_memory_id IN (SELECT memory_id FROM child_requests)) AS verification_evidence_count",
        )
        .bind(parent_goal.into_inner())
        .fetch_one(self.pg.pool())
        .await?;
        Ok(GoalGraphMetrics {
            child_goal_count: row.try_get("child_goal_count")?,
            achieved_child_goal_count: row.try_get("achieved_child_goal_count")?,
            child_execution_request_count: row.try_get("child_execution_request_count")?,
            child_workspace_run_count: row.try_get("child_workspace_run_count")?,
            child_workspace_review_count: row.try_get("child_workspace_review_count")?,
            verification_evidence_count: row.try_get("verification_evidence_count")?,
        })
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
        let request_flow_counts = self.request_flow_counts().await?;
        let workspace_run_count = *output_sidecar_counts_by_schema
            .get(WorkspaceRunV1::SCHEMA_ID)
            .unwrap_or(&0);
        let goal_achieved_fact_exists = self.goal_achieved_fact_exists().await?;
        let goal_graph = self.goal_graph_metrics().await?;
        let final_goal_state = self.final_goal_state().await?;
        let (git_diff_stats, final_changed_files) = self.git_diff_metrics().await?;
        let deterministic_checks = deterministic_checks(
            self.cfg.challenge,
            goal_achieved_fact_exists,
            &goal_graph,
            &git_diff_stats,
            &final_changed_files,
        );
        let deterministic_pass = deterministic_checks.values().all(|value| *value);
        let budget_decision_count = *output_sidecar_counts_by_schema
            .get(BudgetDecisionV1::SCHEMA_ID)
            .unwrap_or(&0);
        let wake_failures = wake_invocations.iter().any(|row| {
            if row.status == "succeeded" || row.status == "skipped" {
                return false;
            }
            row.status != "truncated"
                || row.failure_reason.as_deref() != Some("max_rounds_reached")
                || budget_decision_count == 0
        });
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
        let functional_pass = deterministic_pass && reviewer_raw >= 70;
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
            role_max_rounds: self.cfg.role_max_rounds,
            dispatcher_tick_count: ticks,
            wake_invocation_count_by_role,
            terminal_guard_hits: terminal_guard_hits(&wake_invocations),
            wake_invocations,
            correction_loop_count,
            output_sidecar_counts_by_schema,
            workspace_run_count,
            request_flow_counts,
            review_verdicts,
            final_goal_state,
            goal_achieved_fact_exists,
            goal_graph,
            git_diff_stats,
            final_changed_files,
            deterministic_checks,
            deterministic_pass,
            functional_pass,
            flow_graph_json: self
                .cfg
                .run_dir
                .join("flow_graph.json")
                .display()
                .to_string(),
            flow_graph_mermaid: self
                .cfg
                .run_dir
                .join("flow_graph.mmd")
                .display()
                .to_string(),
            flow_graph_summary: self.collect_flow_graph().await?.summary,
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
            overall_pass: functional_pass && budget_pass,
        })
    }

    async fn request_flow_counts(&self) -> Result<Vec<RequestFlowCount>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT er.memory_id,
                    er.title,
                    count(DISTINCT wr.memory_id) AS workspace_run_count,
                    count(DISTINCT rv.memory_id) AS workspace_review_count,
                    count(DISTINCT rv.memory_id)
                      FILTER (WHERE rv.verdict IN ('approved', 'needs_user')) AS terminal_review_count
             FROM proxima_code.execution_request_v1 er
             LEFT JOIN proxima_core.edges e
               ON e.source_kind = 'Fact'
              AND e.target_kind = 'Fact'
              AND e.target_memory_id = er.memory_id
              AND e.relation = 'core/derived-from'
             LEFT JOIN proxima_code.workspace_run_v1 wr
               ON wr.memory_id = e.source_memory_id
             LEFT JOIN proxima_code.workspace_review_v1 rv
               ON rv.execution_request_memory_id = er.memory_id
             GROUP BY er.memory_id, er.title
             ORDER BY er.title, er.memory_id",
        )
        .fetch_all(self.pg.pool())
        .await?;
        rows.into_iter()
            .map(|row| {
                Ok(RequestFlowCount {
                    request_memory_id: row.try_get::<Uuid, _>("memory_id")?.to_string(),
                    title: row.try_get("title")?,
                    workspace_run_count: row.try_get("workspace_run_count")?,
                    workspace_review_count: row.try_get("workspace_review_count")?,
                    terminal_review_count: row.try_get("terminal_review_count")?,
                })
            })
            .collect()
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
            (
                BudgetReviewRequestedV1::SCHEMA_ID,
                "proxima_core.budget_review_requested_v1",
            ),
            (
                BudgetDecisionV1::SCHEMA_ID,
                "proxima_core.budget_decision_v1",
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
            if self.cfg.challenge.worktree_has_primary_output(&path) {
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
        let prompt = self.cfg.challenge.reviewer_prompt(
            self.latest_worktree().await?,
            diff_stats,
            changed_files,
        )?;
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
        let graph_path = self.cfg.run_dir.join("flow_graph.json");
        let mermaid_path = self.cfg.run_dir.join("flow_graph.mmd");
        let flow_graph = self.collect_flow_graph().await?;
        std::fs::write(&metrics_path, serde_json::to_vec_pretty(metrics)?)?;
        std::fs::write(&graph_path, serde_json::to_vec_pretty(&flow_graph)?)?;
        std::fs::write(&mermaid_path, render_flow_mermaid(&flow_graph))?;
        std::fs::write(&report_path, render_report(metrics, &flow_graph))?;
        eprintln!("demo metrics: {}", metrics_path.display());
        eprintln!("demo report: {}", report_path.display());
        eprintln!("demo flow graph: {}", graph_path.display());
        eprintln!("demo flow mermaid: {}", mermaid_path.display());
        Ok(())
    }

    async fn collect_flow_graph(&self) -> Result<FlowGraph, Box<dyn std::error::Error>> {
        let mut nodes = BTreeMap::<String, FlowNode>::new();
        let mut edges = Vec::<FlowEdge>::new();

        for (role, id) in &self.role_ids {
            nodes.insert(
                entity_node_id("personality", id.into_inner()),
                FlowNode {
                    id: entity_node_id("personality", id.into_inner()),
                    kind: "personality".into(),
                    label: role.clone(),
                    role: Some(role.clone()),
                    schema_id: None,
                    state: None,
                    status: Some("active".into()),
                },
            );
        }

        for row in sqlx::query(
            "SELECT goal_id, title, state::text AS state
             FROM proxima_core.goals
             ORDER BY created_at ASC",
        )
        .fetch_all(self.pg.pool())
        .await?
        {
            let goal_id: Uuid = row.try_get("goal_id")?;
            nodes.insert(
                entity_node_id("goal", goal_id),
                FlowNode {
                    id: entity_node_id("goal", goal_id),
                    kind: "goal".into(),
                    label: row.try_get("title")?,
                    role: None,
                    schema_id: Some("proxima-goal".into()),
                    state: Some(row.try_get("state")?),
                    status: None,
                },
            );
        }

        for row in sqlx::query(
            "SELECT m.memory_id, m.schema_id,
                    COALESCE(er.title, wr.branch_name, rv.summary, ga.title, gp.title, gh.title,
                             'Budget review: ' || br.original_invocation_id::text,
                             'Budget decision: ' || bd.decision::text,
                             m.schema_id) AS label
             FROM proxima_core.memories m
             LEFT JOIN proxima_code.execution_request_v1 er USING (memory_id)
             LEFT JOIN proxima_code.workspace_run_v1 wr USING (memory_id)
             LEFT JOIN proxima_code.workspace_review_v1 rv USING (memory_id)
             LEFT JOIN proxima_code.verification_evidence_v1 ve USING (memory_id)
             LEFT JOIN proxima_goal.goal_activated_v1 ga USING (memory_id)
             LEFT JOIN proxima_goal.goal_proposed_v1 gp USING (memory_id)
             LEFT JOIN proxima_goal.goal_achieved_v1 gh USING (memory_id)
             LEFT JOIN proxima_core.budget_review_requested_v1 br USING (memory_id)
             LEFT JOIN proxima_core.budget_decision_v1 bd USING (memory_id)
             ORDER BY m.created_at ASC",
        )
        .fetch_all(self.pg.pool())
        .await?
        {
            let memory_id: Uuid = row.try_get("memory_id")?;
            let schema_id: String = row.try_get("schema_id")?;
            nodes.insert(
                entity_node_id("memory", memory_id),
                FlowNode {
                    id: entity_node_id("memory", memory_id),
                    kind: "memory".into(),
                    label: row.try_get("label")?,
                    role: None,
                    schema_id: Some(schema_id),
                    state: None,
                    status: None,
                },
            );
        }

        for row in sqlx::query(
            "SELECT goal_id, parent_goal_id
             FROM proxima_core.goal_parents
             ORDER BY parent_goal_id, goal_id",
        )
        .fetch_all(self.pg.pool())
        .await?
        {
            let child: Uuid = row.try_get("goal_id")?;
            let parent: Uuid = row.try_get("parent_goal_id")?;
            edges.push(FlowEdge {
                id: format!("goal-parent:{parent}:{child}"),
                source: entity_node_id("goal", parent),
                target: entity_node_id("goal", child),
                relation: "goal_parent".into(),
                persisted_edge_id: None,
            });
        }

        for row in sqlx::query(
            "SELECT edge_id, relation,
                    source_memory_id, source_goal_id,
                    target_memory_id, target_goal_id
             FROM proxima_core.edges
             ORDER BY created_at ASC",
        )
        .fetch_all(self.pg.pool())
        .await?
        {
            let edge_id: Uuid = row.try_get("edge_id")?;
            let source = flow_endpoint(
                row.try_get::<Option<Uuid>, _>("source_memory_id")?,
                row.try_get::<Option<Uuid>, _>("source_goal_id")?,
            );
            let target = flow_endpoint(
                row.try_get::<Option<Uuid>, _>("target_memory_id")?,
                row.try_get::<Option<Uuid>, _>("target_goal_id")?,
            );
            edges.push(FlowEdge {
                id: format!("edge:{edge_id}"),
                source,
                target,
                relation: row.try_get("relation")?,
                persisted_edge_id: Some(edge_id.to_string()),
            });
        }

        for row in sqlx::query(
            "SELECT i.personality_instance_id, i.wake_entry_id, i.change_event_seq, i.status::text AS status
             FROM proxima_core.personality_wake_invocations i
             ORDER BY i.started_at ASC",
        )
        .fetch_all(self.pg.pool())
        .await?
        {
            let personality_id: Uuid = row.try_get("personality_instance_id")?;
            let change_event_seq: Uuid = row.try_get("change_event_seq")?;
            let wake_entry_id: Uuid = row.try_get("wake_entry_id")?;
            let wake_node_id = format!("wake:{personality_id}:{wake_entry_id}:{change_event_seq}");
            nodes.insert(
                wake_node_id.clone(),
                FlowNode {
                    id: wake_node_id.clone(),
                    kind: "wake_invocation".into(),
                    label: role_for_personality(&self.role_ids, personality_id)
                        .unwrap_or_else(|| "wake".into()),
                    role: role_for_personality(&self.role_ids, personality_id),
                    schema_id: None,
                    state: None,
                    status: Some(row.try_get("status")?),
                },
            );
            edges.push(FlowEdge {
                id: format!("wake-trigger:{personality_id}:{change_event_seq}"),
                source: format!("event:{change_event_seq}"),
                target: wake_node_id.clone(),
                relation: "wake_triggered".into(),
                persisted_edge_id: None,
            });
            edges.push(FlowEdge {
                id: format!("wake-role:{personality_id}:{change_event_seq}"),
                source: entity_node_id("personality", personality_id),
                target: wake_node_id,
                relation: "wake_executed_by".into(),
                persisted_edge_id: None,
            });
        }

        let mut events = Vec::new();
        for row in sqlx::query(
            "SELECT seq, kind::text AS kind, entity_kind::text AS entity_kind,
                    entity_memory_id, entity_goal_id, entity_schema_id, edge_relation,
                    entity_personality_instance_id, wake_chain_depth
             FROM proxima_core.change_event
             ORDER BY seq ASC",
        )
        .fetch_all(self.pg.pool())
        .await?
        {
            let seq: Uuid = row.try_get("seq")?;
            let entity_memory_id: Option<Uuid> = row.try_get("entity_memory_id")?;
            let entity_goal_id: Option<Uuid> = row.try_get("entity_goal_id")?;
            let event_node_id = format!("event:{seq}");
            nodes.insert(
                event_node_id.clone(),
                FlowNode {
                    id: event_node_id,
                    kind: "change_event".into(),
                    label: row.try_get("kind")?,
                    role: None,
                    schema_id: row.try_get("entity_schema_id")?,
                    state: None,
                    status: None,
                },
            );
            if let Some(entity_id) = entity_memory_id {
                edges.push(FlowEdge {
                    id: format!("event-entity:{seq}:{entity_id}"),
                    source: format!("event:{seq}"),
                    target: entity_node_id("memory", entity_id),
                    relation: "event_appended".into(),
                    persisted_edge_id: None,
                });
            }
            if let Some(entity_id) = entity_goal_id {
                edges.push(FlowEdge {
                    id: format!("event-goal:{seq}:{entity_id}"),
                    source: format!("event:{seq}"),
                    target: entity_node_id("goal", entity_id),
                    relation: "event_appended".into(),
                    persisted_edge_id: None,
                });
            }
            events.push(FlowEvent {
                seq: seq.to_string(),
                kind: row.try_get("kind")?,
                entity_kind: row.try_get("entity_kind")?,
                entity_id: entity_memory_id.or(entity_goal_id).map(|id| id.to_string()),
                schema_id: row.try_get("entity_schema_id")?,
                edge_relation: row.try_get("edge_relation")?,
                personality_instance_id: row
                    .try_get::<Option<Uuid>, _>("entity_personality_instance_id")?
                    .map(|id| id.to_string()),
                wake_chain_depth: row.try_get("wake_chain_depth")?,
            });
        }

        let unresolved_endpoint_count = edges
            .iter()
            .filter(|edge| !nodes.contains_key(&edge.source) || !nodes.contains_key(&edge.target))
            .count();
        let nodes = nodes.into_values().collect::<Vec<_>>();
        let summary = FlowGraphSummary {
            node_count: nodes.len(),
            edge_count: edges.len(),
            personality_count: nodes.iter().filter(|n| n.kind == "personality").count(),
            goal_count: nodes.iter().filter(|n| n.kind == "goal").count(),
            execution_request_count: nodes
                .iter()
                .filter(|n| {
                    n.kind == "memory"
                        && n.schema_id.as_deref() == Some(ExecutionRequestV1::SCHEMA_ID)
                })
                .count(),
            workspace_run_count: nodes
                .iter()
                .filter(|n| {
                    n.kind == "memory" && n.schema_id.as_deref() == Some(WorkspaceRunV1::SCHEMA_ID)
                })
                .count(),
            workspace_review_count: nodes
                .iter()
                .filter(|n| {
                    n.kind == "memory"
                        && n.schema_id.as_deref() == Some(WorkspaceReviewV1::SCHEMA_ID)
                })
                .count(),
            verification_evidence_count: nodes
                .iter()
                .filter(|n| {
                    n.kind == "memory"
                        && n.schema_id.as_deref() == Some("proxima-code/verification-evidence-v1")
                })
                .count(),
            budget_review_count: nodes
                .iter()
                .filter(|n| {
                    n.kind == "memory"
                        && n.schema_id.as_deref() == Some(BudgetReviewRequestedV1::SCHEMA_ID)
                })
                .count(),
            budget_decision_count: nodes
                .iter()
                .filter(|n| {
                    n.kind == "memory"
                        && n.schema_id.as_deref() == Some(BudgetDecisionV1::SCHEMA_ID)
                })
                .count(),
            wake_invocation_count: nodes.iter().filter(|n| n.kind == "wake_invocation").count(),
            unresolved_endpoint_count,
        };
        Ok(FlowGraph {
            nodes,
            edges,
            events,
            summary,
        })
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
    budget_policy: Option<BudgetExhaustionPolicy>,
}

impl WakeOptions {
    fn default_with_rounds(max_rounds: u16) -> Self {
        Self {
            authored_by: WakeEntryAuthoredBy::Other,
            goal_scope: WakeEntryGoalScope::None,
            max_rounds,
            budget_policy: None,
        }
    }
}

fn demo_budget_policy(budgeter: PersonalityInstanceId) -> BudgetExhaustionPolicy {
    BudgetExhaustionPolicy {
        budgeter_personality_instance_id: budgeter.into_inner(),
        budget_extension_rounds: 4,
        budget_hard_cap_rounds: 8,
        budget_progress_contract: "Decide from the budget review Fact and wake lineage whether the truncated wake made concrete progress toward the active demo Goal. Loops with repeated tool errors should stop. Truncations after useful work or after the larger goal has enough downstream evidence may be accepted as terminal for v1; automatic continuation is not enabled in this demo yet.".into(),
    }
}

async fn prepare_demo_repo(
    path: &Path,
    challenge: DemoChallenge,
) -> Result<Uuid, Box<dyn std::error::Error>> {
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
    std::fs::write(path.join(".proxima-demo-repo"), challenge.marker())?;
    match challenge {
        DemoChallenge::SignalMatch => {
            std::fs::write(
                path.join("README.md"),
                "# Signal Match\n\nThe demo wheel should create `index.html`.\n",
            )?;
        }
        DemoChallenge::TodoCli => {
            std::fs::create_dir_all(path.join("examples"))?;
            std::fs::write(
                path.join("README.md"),
                "# Todo Audit\n\nBuild `todo_audit.mjs` and `test_todo_audit.mjs`. Use only Node built-ins.\n",
            )?;
            std::fs::write(
                path.join("examples/tasks.md"),
                "- [ ] Ship parser @ana #cli !high due:2026-05-17\n- [x] Draft README @bo #docs !low due:2026-05-10\n- [ ] Add JSON output @ana #cli #report !medium due:2026-05-20\n- [ ] Triage backlog @cy #ops !high\n",
            )?;
        }
    }
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
            match challenge {
                DemoChallenge::SignalMatch => "chore: seed signal match demo",
                DemoChallenge::TodoCli => "chore: seed todo audit demo",
            },
        ],
    )?;
    Ok(Uuid::now_v7())
}

fn planner_instruction(planner: PersonalityInstanceId, challenge: DemoChallenge) -> String {
    match challenge {
        DemoChallenge::SignalMatch => signal_match_planner_instruction(planner),
        DemoChallenge::TodoCli => todo_cli_planner_instruction(planner),
    }
}

fn worker_instruction(challenge: DemoChallenge) -> String {
    match challenge {
        DemoChallenge::SignalMatch => {
            let app = signal_match_index_html();
            format!(
                "Use workspace_text_editor to create `index.html` with exactly this file_text, then run workspace_shell with command `test -f index.html && grep -E \"Signal Match|data-pad|keydown|restart|level|score\" index.html` and stop. file_text JSON string: {}",
                serde_json::to_string(&app).expect("serialize app")
            )
        }
        DemoChallenge::TodoCli => {
            "Implement the requested Todo Audit CLI using only Node.js built-ins. Create or update `todo_audit.mjs`, `test_todo_audit.mjs`, and `examples/tasks.md` as needed. The CLI must support `node todo_audit.mjs <markdown-file> --today 2026-05-18 --json`, parse Markdown task-list items, extract done/open state, @owner, #tags, !priority, due:YYYY-MM-DD, compute totals, open/done/overdue counts, byOwner, byTag, highPriorityOpen, and nextDue sorted by due date. Write meaningful tests in `test_todo_audit.mjs` using node:assert/child_process only, run `node test_todo_audit.mjs`, then stop.".into()
        }
    }
}

fn verifier_instruction(challenge: DemoChallenge) -> String {
    match challenge {
        DemoChallenge::SignalMatch => signal_match_verifier_instruction(),
        DemoChallenge::TodoCli => todo_cli_verifier_instruction(),
    }
}

fn signal_match_planner_instruction(planner: PersonalityInstanceId) -> String {
    format!(
        "You are the Planner for the triggering active Goal in N1. Decide whether the Goal is small enough for one execution request or should first be decomposed. This demo Goal is intentionally larger than one directly verifiable unit; prefer decomposing it into independently verifiable child Goals before emitting execution requests. Do not create child Goals unless you decide decomposition is warranted. If N1 is the top-level Signal Match static SPA demo Goal, call proxima_goal_goal_decompose with parent_goal \"N1\", activate_children true, target_personality \"{}\", idempotency_key \"demo-signal-match-decompose\", and these suggested children: {}. Then stop. If N1 is already one of those child Goals, call proxima_code_code_emit_execution_request for that child with repo_handle \"{}\", goal_activated_memory \"N1\", evidence [], a child-specific title/instructions/idempotency_key, and these required acceptance_criteria: {}. Use idempotency_key \"demo-signal-match-shell\" for the shell/pads child and \"demo-signal-match-gameplay\" for the gameplay/restart child. Then stop.",
        planner.into_inner(),
        json!([
            {
                "payload": {
                    "schema_id": "proxima-goal/simple-text-v1",
                    "body": {
                        "title": "Signal Match static shell and responsive pads",
                        "text": "Create index.html with a package-free responsive Signal Match shell, title, four colored pads, and direct browser entrypoint."
                    }
                },
                "evidence": []
            },
            {
                "payload": {
                    "schema_id": "proxima-goal/simple-text-v1",
                    "body": {
                        "title": "Signal Match gameplay controls and restart loop",
                        "text": "Create index.html gameplay behavior for sequence playback, click input, Q W A S keyboard input, score and level display, failure state, and restart control."
                    }
                },
                "evidence": []
            }
        ]),
        SIGNAL_MATCH_REPO_HANDLE,
        json!([
            {
                "key": "static_entrypoint",
                "description": "index.html exists and runs without package installation",
                "required": true,
                "verifier_kind": "file_exists",
                "verifier_spec_json": { "path": "index.html" }
            },
            {
                "key": "gameplay_controls",
                "description": "Signal Match includes pads, keyboard input, score, level, failure state, and restart",
                "required": true,
                "verifier_kind": "command",
                "verifier_spec_json": {
                    "command": ["grep", "-E", "Signal Match|data-pad|keydown|restart|level|score|game-over", "index.html"]
                }
            }
        ])
    )
}

fn todo_cli_planner_instruction(planner: PersonalityInstanceId) -> String {
    format!(
        "You are the Planner for the triggering active Goal in N1. Decide whether the Goal is small enough for one execution request or should first be decomposed. This Todo Audit CLI Goal requires parser logic, CLI output, fixtures, and tests; prefer decomposing it into independently verifiable child Goals before emitting execution requests. Do not create child Goals unless you decide decomposition is warranted. If N1 is the top-level Todo Audit CLI demo Goal, call proxima_goal_goal_decompose with parent_goal \"N1\", activate_children true, target_personality \"{}\", idempotency_key \"demo-todo-audit-decompose\", and these suggested children: {}. Then stop. If N1 is already one of those child Goals, call proxima_code_code_emit_execution_request for that child with repo_handle \"{}\", goal_activated_memory \"N1\", evidence [], a child-specific title/instructions/idempotency_key, and these required acceptance_criteria: {}. Each child request must still produce a complete runnable CLI and test suite because workspace runs are evaluated independently. Then stop.",
        planner.into_inner(),
        json!([
            {
                "payload": {
                    "schema_id": "proxima-goal/simple-text-v1",
                    "body": {
                        "title": "Todo Audit parser and data model",
                        "text": "Implement Markdown task-list parsing for done/open state, @owner, #tags, !priority, due date tokens, and stable task records."
                    }
                },
                "evidence": []
            },
            {
                "payload": {
                    "schema_id": "proxima-goal/simple-text-v1",
                    "body": {
                        "title": "Todo Audit JSON summary CLI",
                        "text": "Implement a package-free Node CLI that reads a Markdown file and prints deterministic JSON summary counts, byOwner, byTag, highPriorityOpen, and nextDue."
                    }
                },
                "evidence": []
            },
            {
                "payload": {
                    "schema_id": "proxima-goal/simple-text-v1",
                    "body": {
                        "title": "Todo Audit fixtures and tests",
                        "text": "Add sample Markdown tasks and Node built-in tests that verify parser and CLI JSON behavior."
                    }
                },
                "evidence": []
            }
        ]),
        TODO_CLI_REPO_HANDLE,
        json!([
            {
                "key": "cli_entrypoint",
                "description": "todo_audit.mjs exists and can be executed with Node without package installation",
                "required": true,
                "verifier_kind": "file_exists",
                "verifier_spec_json": { "path": "todo_audit.mjs" }
            },
            {
                "key": "parser_tests",
                "description": "Node built-in test script passes",
                "required": true,
                "verifier_kind": "command",
                "verifier_spec_json": { "command": ["node", "test_todo_audit.mjs"] }
            },
            {
                "key": "json_summary",
                "description": "CLI emits deterministic JSON summary for examples/tasks.md",
                "required": true,
                "verifier_kind": "command",
                "verifier_spec_json": { "command": ["sh", "-c", "node todo_audit.mjs examples/tasks.md --today 2026-05-18 --json | grep -E '\"total\"|\"open\"|\"byOwner\"|\"nextDue\"'"] }
            }
        ])
    )
}

fn signal_match_verifier_instruction() -> String {
    "Inspect the prepared workspace. Run workspace_shell with command `test -f index.html && grep -E \"Signal Match|data-pad|keydown|restart|level|score|game-over\" index.html`. If it exits 0, first call proxima_code_code_emit_verification_evidence twice: {\"workspace_run_memory\":\"N1\",\"criterion_key\":\"static_entrypoint\",\"status\":\"passed\",\"summary\":\"index.html exists\",\"artifact_refs_json\":{\"path\":\"index.html\"},\"idempotency_key\":\"demo-signal-match-evidence-static\"} and {\"workspace_run_memory\":\"N1\",\"criterion_key\":\"gameplay_controls\",\"status\":\"passed\",\"summary\":\"index.html contains Signal Match controls and states\",\"artifact_refs_json\":{\"path\":\"index.html\"},\"idempotency_key\":\"demo-signal-match-evidence-gameplay\"}. Then call proxima_code_code_emit_workspace_review with {\"workspace_run_memory\":\"N1\",\"verdict\":\"approved\",\"summary\":\"Signal Match requirements satisfied\",\"findings\":[],\"verification_summary\":\"index.html exists and contains direct-run Signal Match gameplay controls\",\"idempotency_key\":\"demo-signal-match-review-approved\"}. If the shell check fails, call proxima_code_code_emit_verification_evidence for both keys with status \"failed\", then call the review tool with verdict rejected, summary \"Signal Match requirements missing\", one finding for index.html, correction_instructions \"Create a complete direct-run Signal Match index.html. Failed criteria: static_entrypoint, gameplay_controls\", and idempotency_key \"demo-signal-match-review-rejected\". Then stop.".into()
}

fn todo_cli_verifier_instruction() -> String {
    "Inspect the prepared workspace. Run workspace_shell with command `test -f todo_audit.mjs && test -f test_todo_audit.mjs && test -f examples/tasks.md && node test_todo_audit.mjs && node todo_audit.mjs examples/tasks.md --today 2026-05-18 --json | grep -E '\"total\"|\"open\"|\"byOwner\"|\"nextDue\"'`. If it exits 0, first call proxima_code_code_emit_verification_evidence exactly three times with these JSON objects: {\"workspace_run_memory\":\"N1\",\"criterion_key\":\"cli_entrypoint\",\"status\":\"passed\",\"summary\":\"todo_audit.mjs exists and runs with Node\",\"artifact_refs_json\":{\"paths\":[\"todo_audit.mjs\"]},\"idempotency_key\":\"demo-todo-audit-evidence-entrypoint\"}, {\"workspace_run_memory\":\"N1\",\"criterion_key\":\"parser_tests\",\"status\":\"passed\",\"summary\":\"node test_todo_audit.mjs passed\",\"artifact_refs_json\":{\"paths\":[\"test_todo_audit.mjs\",\"todo_audit.mjs\"]},\"idempotency_key\":\"demo-todo-audit-evidence-tests\"}, and {\"workspace_run_memory\":\"N1\",\"criterion_key\":\"json_summary\",\"status\":\"passed\",\"summary\":\"CLI emitted expected JSON summary fields\",\"artifact_refs_json\":{\"paths\":[\"examples/tasks.md\",\"todo_audit.mjs\"]},\"idempotency_key\":\"demo-todo-audit-evidence-json\"}. Then call proxima_code_code_emit_workspace_review with {\"workspace_run_memory\":\"N1\",\"verdict\":\"approved\",\"summary\":\"Todo Audit CLI requirements satisfied\",\"findings\":[],\"verification_summary\":\"entrypoint, tests, and JSON summary passed\",\"idempotency_key\":\"demo-todo-audit-review-approved\"}. If the shell check fails, first call proxima_code_code_emit_verification_evidence exactly three times with status \"failed\" for cli_entrypoint, parser_tests, and json_summary, using artifact_refs_json objects like {\"paths\":[\"todo_audit.mjs\"]}. Then call the review tool with verdict rejected, summary \"Todo Audit CLI requirements missing\", one finding for todo_audit.mjs, correction_instructions \"Create a complete package-free Node Todo Audit CLI with parser tests and deterministic JSON output. Failed criteria: cli_entrypoint, parser_tests, json_summary\", and idempotency_key \"demo-todo-audit-review-rejected\". Then stop.".into()
}

fn goal_reviewer_instruction() -> String {
    "Read the workspace review payload in Triggering Memory. If verdict is approved, first call proxima_code_code_goal_completion_status with {\"workspace_review_memory\":\"N1\"}. If its child_close is present, call proxima_goal_goal_mark_achieved using exactly child_close.goal, child_close.evidence, and child_close.idempotency_key. If its parent.parent_close is present, call proxima_goal_goal_mark_achieved after the child call using exactly parent.parent_close.goal, parent.parent_close.evidence, and parent.parent_close.idempotency_key. If verdict is rejected, call proxima_code_code_emit_correction_execution_request with {\"workspace_review_memory\":\"N1\",\"target_personality\":\"P1\",\"idempotency_key\":\"demo-signal-match-correction-1\"}. Then stop.".into()
}

fn budgeter_instruction() -> String {
    "You are the Budgeter for this E2E demo. Triggering Memory N1 is a core/budget-review-requested-v1 Fact. First inspect N1. You may call core_walk_lineage with {\"memory\":\"N1\"} if you need the wake trace and triggering Fact context. Automatic continuation is intentionally not enabled in this demo wiring yet, so do not choose continue. If the review indicates max_rounds_reached after concrete progress, downstream evidence, or a likely terminal-but-truncated wake, call core_emit_budget_decision with {\"budget_request\":\"N1\",\"decision\":\"accept_terminal\",\"rationale\":\"<short evidence-based reason>\",\"idempotency_key\":\"demo-budgeter-accept-N1\"}. If the wake appears to be looping, blocked, or making no useful progress, call core_emit_budget_decision with decision \"stop\" and idempotency_key \"demo-budgeter-stop-N1\". Then stop.".into()
}

fn deterministic_checks(
    challenge: DemoChallenge,
    achieved: bool,
    goal_graph: &GoalGraphMetrics,
    diff: &GitDiffStats,
    changed_files: &[String],
) -> BTreeMap<String, bool> {
    let mut checks = BTreeMap::new();
    checks.insert(
        "required_files_exist".into(),
        challenge
            .required_files()
            .iter()
            .all(|file| changed_files.iter().any(|changed| changed == file)),
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
        "planner_decomposed_parent_goal".into(),
        goal_graph.child_goal_count >= 2,
    );
    checks.insert(
        "all_child_goals_achieved_before_parent_completion".into(),
        goal_graph.child_goal_count >= 2
            && goal_graph.achieved_child_goal_count == goal_graph.child_goal_count,
    );
    checks.insert(
        "child_execution_requests_observed".into(),
        goal_graph.child_execution_request_count >= 2,
    );
    checks.insert(
        "child_workspace_runs_observed".into(),
        goal_graph.child_workspace_run_count >= 2,
    );
    checks.insert(
        "child_workspace_reviews_observed".into(),
        goal_graph.child_workspace_review_count >= 2,
    );
    checks.insert(
        "deterministic_verifier_evidence_observed".into(),
        goal_graph.verification_evidence_count >= 1,
    );
    checks.insert(
        "final_diff_modifies_only_demo_repo_files".into(),
        changed_files
            .iter()
            .all(|f| !f.starts_with('/') && !f.contains("..")),
    );
    checks.insert(
        "primary_entrypoint_exists".into(),
        changed_files
            .iter()
            .any(|f| f == challenge.required_files()[0]),
    );
    checks.insert(
        "nonempty_diff".into(),
        diff.files_changed > 0 && diff.insertions > 0,
    );
    checks
}

fn render_report(metrics: &Metrics, flow_graph: &FlowGraph) -> String {
    format!(
        "# Proxima Demo Wheel Report\n\n- run_dir: `{}`\n- repo_path: `{}`\n- db_name: `{}`\n- ticks: `{}`\n- corrections: `{}`\n- goal_state: `{}`\n- deterministic_pass: `{}`\n- functional_pass: `{}`\n- budget_pass: `{}`\n- overall_pass: `{}`\n- reviewer_score: `{}`\n- overall_score: `{}`\n- score_per_model_round: `{:?}`\n- score_per_wall_clock_second: `{:.4}`\n\n## Role Budgets\n\n```json\n{}\n```\n\n## Goal Graph\n\n```json\n{}\n```\n\n## Request Flow Counts\n\n```json\n{}\n```\n\n## Terminal Guard Hits\n\n```json\n{}\n```\n\n## Flow Graph\n\n- graph_json: `{}`\n- graph_mermaid: `{}`\n- nodes: `{}`\n- edges: `{}`\n- budget_reviews: `{}`\n- budget_decisions: `{}`\n- unresolved_endpoints: `{}`\n\n```mermaid\n{}\n```\n\n## Auto Merge\n\n```json\n{}\n```\n\n## Diff\n\n- files_changed: `{}`\n- insertions: `{}`\n- deletions: `{}`\n- files: `{:?}`\n\n## Wake Invocations\n\n```json\n{}\n```\n\n## Checks\n\n```json\n{}\n```\n",
        metrics.run_dir,
        metrics.repo_path,
        metrics.db_name,
        metrics.dispatcher_tick_count,
        metrics.correction_loop_count,
        metrics.final_goal_state,
        metrics.deterministic_pass,
        metrics.functional_pass,
        metrics.budget_pass,
        metrics.overall_pass,
        metrics
            .reviewer_score
            .as_ref()
            .map(|s| s.score.to_string())
            .unwrap_or_else(|| "null".into()),
        metrics.overall_score,
        metrics.score_per_model_round,
        metrics.score_per_wall_clock_second,
        serde_json::to_string_pretty(&metrics.role_max_rounds).unwrap_or_default(),
        serde_json::to_string_pretty(&metrics.goal_graph).unwrap_or_default(),
        serde_json::to_string_pretty(&metrics.request_flow_counts).unwrap_or_default(),
        serde_json::to_string_pretty(&metrics.terminal_guard_hits).unwrap_or_default(),
        metrics.flow_graph_json,
        metrics.flow_graph_mermaid,
        flow_graph.summary.node_count,
        flow_graph.summary.edge_count,
        flow_graph.summary.budget_review_count,
        flow_graph.summary.budget_decision_count,
        flow_graph.summary.unresolved_endpoint_count,
        render_flow_mermaid(flow_graph),
        serde_json::to_string_pretty(&metrics.auto_merge).unwrap_or_default(),
        metrics.git_diff_stats.files_changed,
        metrics.git_diff_stats.insertions,
        metrics.git_diff_stats.deletions,
        metrics.final_changed_files,
        serde_json::to_string_pretty(&metrics.wake_invocations).unwrap_or_default(),
        serde_json::to_string_pretty(&metrics.deterministic_checks).unwrap_or_default()
    )
}

fn render_flow_mermaid(graph: &FlowGraph) -> String {
    let mut out = String::from("graph TD\n");
    for node in &graph.nodes {
        out.push_str(&format!(
            "  {}[\"{}\"]\n",
            mermaid_id(&node.id),
            mermaid_label(&node.label)
        ));
    }
    for edge in &graph.edges {
        out.push_str(&format!(
            "  {} -->|{}| {}\n",
            mermaid_id(&edge.source),
            mermaid_label(&edge.relation),
            mermaid_id(&edge.target)
        ));
    }
    out
}

fn entity_node_id(kind: &str, id: Uuid) -> String {
    format!("{kind}:{id}")
}

fn flow_endpoint(memory_id: Option<Uuid>, goal_id: Option<Uuid>) -> String {
    if let Some(memory_id) = memory_id {
        entity_node_id("memory", memory_id)
    } else if let Some(goal_id) = goal_id {
        entity_node_id("goal", goal_id)
    } else {
        "missing:endpoint".into()
    }
}

fn role_for_personality(
    role_ids: &BTreeMap<String, PersonalityInstanceId>,
    personality_id: Uuid,
) -> Option<String> {
    role_ids
        .iter()
        .find_map(|(role, id)| (id.into_inner() == personality_id).then(|| role.clone()))
}

fn mermaid_id(raw: &str) -> String {
    let mut id = String::from("n");
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() {
            id.push(ch);
        } else {
            id.push('_');
        }
    }
    id
}

fn mermaid_label(raw: &str) -> String {
    raw.replace('\\', "\\\\").replace('"', "\\\"")
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

fn env_optional_u16(name: &str) -> Result<Option<u16>, Box<dyn std::error::Error>> {
    match std::env::var(name) {
        Ok(value) => Ok(Some(value.parse()?)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(err) => Err(err.into()),
    }
}

fn env_u16_with_fallback(
    name: &str,
    default: u16,
    fallback: Option<u16>,
) -> Result<u16, Box<dyn std::error::Error>> {
    match std::env::var(name) {
        Ok(value) => Ok(value.parse()?),
        Err(std::env::VarError::NotPresent) => Ok(fallback.unwrap_or(default)),
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
