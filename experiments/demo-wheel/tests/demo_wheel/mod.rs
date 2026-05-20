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
    InstantiatePersonalityRequest, SetReadScopeRequest, SetWakeEntriesRequest, WakeEntryDraft,
};
use proxima_core::storage::Storage;
use proxima_core::{
    AbstractionPayload, BindInferenceTierRequest, CORE_DERIVED_FROM_RELATION,
    CORE_INSPIRES_RELATION, CoreWorkspaceRunV1, Credentials, EdgeAuthorshipKind, Engine,
    EntityKind, FactPayload, FlavorRegistry, GoalId, InferenceTargetConfig, InterventionDecisionV1,
    InterventionPolicy, InterventionRequestedV1, MemoryId, MistralChatConfig, ModelTier, OrgId,
    Owner, PersonalityInstanceId, Principal, RegisterInferenceTargetRequest, UserId,
    WakeEntryAuthoredBy, WakeEntryGoalScope, WakeEntryTriggerKind, WakeExecutionMode,
    WakeWorkspaceBinding, WakeWorkspaceFinalize,
};
use proxima_flavor_intent::VisionBriefV1;
use proxima_harness::HarnessLoop;
use proxima_mcp_server::McpToolHost;
use proxima_storage_pg::PgStorage;
use proxima_storage_pg::settings::EmbeddingModel;
use proxima_storage_pg::verbs::edge_append::{EdgeDraft, append_edge_in_tx};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{Connection, Executor, PgConnection, Row};
use uuid::Uuid;

mod outputs;
mod prompts;
mod render;
mod runtime;
mod setup;
mod world;
mod world_metrics;

use prompts::*;
use render::*;
use runtime::*;

const ADMIN_URL: &str = "postgres://proxima:proxima@localhost/postgres";
const TARGET_REF: &str = "demo/mistral-medium-3.5";
const MODEL_ID: &str = "mistral-medium-3.5";
const EMBED_VENDOR: &str = "Ollama";
const EMBED_MODEL: &str = "qwen3-embedding:8b";
const SIGNAL_MATCH_REPO_HANDLE: &str = "signal-match-demo";
const SIGNAL_MATCH_GOAL_TITLE: &str = "Signal Match static SPA demo";
const TODO_CLI_REPO_HANDLE: &str = "todo-audit-demo";
const TODO_CLI_GOAL_TITLE: &str = "Todo Audit CLI demo";
const KANBAN_REPO_HANDLE: &str = "kanban-board-demo";
const KANBAN_GOAL_TITLE: &str = "Kanban Board frontend demo";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DemoChallenge {
    SignalMatch,
    TodoCli,
    KanbanBoard,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum DemoInterventionMode {
    Normal,
    ForceContinue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum DemoPlannerMode {
    Scripted,
    Real,
    VisionDocument,
}

impl DemoChallenge {
    fn from_env() -> Result<Self, Box<dyn std::error::Error>> {
        match std::env::var("PROXIMA_DEMO_CHALLENGE")
            .unwrap_or_else(|_| "signal_match".into())
            .as_str()
        {
            "signal_match" => Ok(Self::SignalMatch),
            "todo_cli" => Ok(Self::TodoCli),
            "kanban_board" => Ok(Self::KanbanBoard),
            value => Err(format!(
                "unsupported PROXIMA_DEMO_CHALLENGE {value:?}; expected signal_match, todo_cli, or kanban_board"
            )
            .into()),
        }
    }

    fn repo_handle(self) -> &'static str {
        match self {
            Self::SignalMatch => SIGNAL_MATCH_REPO_HANDLE,
            Self::TodoCli => TODO_CLI_REPO_HANDLE,
            Self::KanbanBoard => KANBAN_REPO_HANDLE,
        }
    }

    fn default_repo_name(self) -> &'static str {
        match self {
            Self::SignalMatch => "signal-match-repo",
            Self::TodoCli => "todo-audit-repo",
            Self::KanbanBoard => "kanban-board-repo",
        }
    }

    fn marker(self) -> &'static str {
        match self {
            Self::SignalMatch => "signal-match\n",
            Self::TodoCli => "todo-audit-cli\n",
            Self::KanbanBoard => "kanban-board\n",
        }
    }

    fn goal_title(self) -> &'static str {
        match self {
            Self::SignalMatch => SIGNAL_MATCH_GOAL_TITLE,
            Self::TodoCli => TODO_CLI_GOAL_TITLE,
            Self::KanbanBoard => KANBAN_GOAL_TITLE,
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
            Self::KanbanBoard => {
                "Build a package-free static Kanban frontend in index.html with executable tests. It must run by opening index.html directly, render seeded tasks, support search and status filtering, move tasks between columns with accessible controls, update counters, persist state in localStorage, and include repo-native tests runnable from shell."
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
            Self::KanbanBoard => &["index.html", "test_kanban.mjs"],
        }
    }

    fn required_child_goal_count(self) -> i64 {
        match self {
            Self::SignalMatch | Self::TodoCli => 2,
            Self::KanbanBoard => 3,
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
            (Self::KanbanBoard, Some(worktree)) => {
                let mut text = String::new();
                for file in ["index.html", "test_kanban.mjs", "data/tasks.json"] {
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
            Self::KanbanBoard => {
                "direct index.html run, responsive Kanban layout, seeded tasks, search and status filtering, move controls, counters, localStorage persistence, and runnable repo-native frontend tests"
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

#[derive(Debug, Clone)]
struct DemoConfig {
    intervention_mode: DemoInterventionMode,
    planner_mode: DemoPlannerMode,
    challenge: DemoChallenge,
    repo_path: PathBuf,
    run_dir: PathBuf,
    base_url: String,
    api_key_env: String,
    max_ticks: u32,
    max_wall_clock_seconds: Option<u64>,
    max_correction_loops: u32,
    role_max_rounds: RoleMaxRounds,
}

#[derive(Debug, Clone, Copy, Serialize)]
struct RoleMaxRounds {
    visionary: u16,
    planner: u16,
    worker: u16,
    verifier: u16,
    goal_reviewer: u16,
    wake_supervisor: u16,
}

#[derive(Debug, Serialize)]
struct Metrics {
    intervention_mode: DemoInterventionMode,
    planner_mode: DemoPlannerMode,
    run_dir: String,
    repo_path: String,
    db_name: String,
    max_ticks: u32,
    max_wall_clock_seconds: Option<u64>,
    max_correction_loops: u32,
    role_max_rounds: RoleMaxRounds,
    dispatcher_tick_count: u32,
    wake_invocation_count_by_role: BTreeMap<String, u32>,
    wake_invocations: Vec<WakeInvocationMetric>,
    terminal_guard_hits: BTreeMap<String, u32>,
    correction_loop_count: u32,
    output_sidecar_counts_by_schema: BTreeMap<String, i64>,
    workspace_run_count: i64,
    core_workspace_run_count: i64,
    request_flow_counts: Vec<RequestFlowCount>,
    review_verdicts: BTreeMap<String, i64>,
    final_goal_state: String,
    goal_achieved_fact_exists: bool,
    goal_graph: GoalGraphMetrics,
    git_diff_stats: GitDiffStats,
    final_changed_files: Vec<String>,
    deterministic_checks: BTreeMap<String, bool>,
    deterministic_pass: bool,
    forced_continuation_checks: BTreeMap<String, bool>,
    forced_continuation_pass: bool,
    real_planner_checks: BTreeMap<String, bool>,
    real_planner_pass: bool,
    functional_pass: bool,
    flow_graph_json: String,
    flow_graph_mermaid: String,
    flow_graph_summary: FlowGraphSummary,
    conversation_index_json: String,
    conversation_invocation_count: usize,
    conversation_missing_log_count: usize,
    reviewer_score: Option<ReviewerScore>,
    reviewer_score_error: Option<String>,
    auto_merge: Option<AutoMergeMetric>,
    overall_score: u32,
    total_model_rounds: u32,
    wall_clock_seconds: f64,
    wall_clock_timed_out: bool,
    score_per_model_round: Option<f64>,
    score_per_wall_clock_second: f64,
    intervention_pass: bool,
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

#[derive(Debug, Serialize)]
struct ConversationIndex {
    run_dir: String,
    index_path: String,
    invocation_count: usize,
    missing_log_count: usize,
    invocations: Vec<ConversationInvocationArtifact>,
}

#[derive(Debug, Serialize)]
struct ConversationInvocationArtifact {
    invocation_id: String,
    role: String,
    personality_instance_id: String,
    wake_entry_id: String,
    trigger_schema_id: String,
    change_event_seq: String,
    execution_mode: String,
    status: String,
    source_jsonl_path: Option<String>,
    copied_jsonl_path: Option<String>,
    missing_log: bool,
    copy_error: Option<String>,
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
    vision_brief_count: usize,
    intervention_request_count: usize,
    intervention_decision_count: usize,
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
    fn complete(&self, parent_achieved: bool, required_child_goal_count: i64) -> bool {
        parent_achieved
            && self.child_goal_count >= required_child_goal_count
            && self.achieved_child_goal_count == self.child_goal_count
            && self.child_execution_request_count >= required_child_goal_count
            && self.child_workspace_run_count >= required_child_goal_count
            && self.child_workspace_review_count >= required_child_goal_count
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

type DemoRunResult = Result<(), Box<dyn std::error::Error>>;

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

pub async fn run_from_env() -> Result<(), Box<dyn std::error::Error>> {
    run_with_modes_from_env(
        DemoInterventionMode::Normal,
        DemoPlannerMode::Scripted,
        None,
        None,
    )
    .await
}

pub async fn run_forced_continue_from_env() -> Result<(), Box<dyn std::error::Error>> {
    run_with_modes_from_env(
        DemoInterventionMode::ForceContinue,
        DemoPlannerMode::Scripted,
        None,
        None,
    )
    .await
}

pub async fn run_real_planner_signal_match_target_from_env() -> DemoRunResult {
    run_with_modes_from_env(
        DemoInterventionMode::Normal,
        DemoPlannerMode::Real,
        Some(DemoChallenge::SignalMatch),
        Some(600),
    )
    .await
}

pub async fn run_goal_to_vision_document_from_env() -> DemoRunResult {
    run_with_modes_from_env(
        DemoInterventionMode::Normal,
        DemoPlannerMode::VisionDocument,
        Some(DemoChallenge::SignalMatch),
        Some(240),
    )
    .await
}

async fn run_with_modes_from_env(
    intervention_mode: DemoInterventionMode,
    planner_mode: DemoPlannerMode,
    default_challenge: Option<DemoChallenge>,
    default_max_wall_clock_seconds: Option<u64>,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(cfg) = DemoConfig::from_env(
        intervention_mode,
        planner_mode,
        default_challenge,
        default_max_wall_clock_seconds,
    )?
    else {
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
    fn from_env(
        intervention_mode: DemoInterventionMode,
        planner_mode: DemoPlannerMode,
        default_challenge: Option<DemoChallenge>,
        default_max_wall_clock_seconds: Option<u64>,
    ) -> Result<Option<Self>, Box<dyn std::error::Error>> {
        if std::env::var("PROXIMA_LIVE_MISTRAL").ok().as_deref() != Some("1") {
            eprintln!("skipping demo wheel: set PROXIMA_LIVE_MISTRAL=1");
            return Ok(None);
        }
        std::env::var("MISTRAL_API_KEY")
            .map_err(|_| "MISTRAL_API_KEY must be set for demo wheel")?;
        let challenge = if let Some(challenge) = default_challenge {
            challenge
        } else {
            match std::env::var("PROXIMA_DEMO_CHALLENGE") {
                Ok(_) => DemoChallenge::from_env()?,
                Err(std::env::VarError::NotPresent) => DemoChallenge::SignalMatch,
                Err(err) => return Err(err.into()),
            }
        };

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
            intervention_mode,
            planner_mode,
            challenge,
            repo_path,
            run_dir,
            base_url,
            api_key_env: "MISTRAL_API_KEY".into(),
            max_ticks: env_u32("PROXIMA_DEMO_MAX_TICKS", 24)?,
            max_wall_clock_seconds: env_optional_u64(
                "PROXIMA_DEMO_MAX_WALL_CLOCK_SECONDS",
                default_max_wall_clock_seconds,
            )?,
            max_correction_loops: env_u32("PROXIMA_DEMO_MAX_CORRECTION_LOOPS", 2)?,
            role_max_rounds: RoleMaxRounds::from_env(intervention_mode)?,
        }))
    }

    fn required_child_goal_count(&self) -> i64 {
        if self.planner_mode == DemoPlannerMode::Real
            && self.challenge == DemoChallenge::SignalMatch
        {
            1
        } else {
            self.challenge.required_child_goal_count()
        }
    }
}

impl RoleMaxRounds {
    fn from_env(mode: DemoInterventionMode) -> Result<Self, Box<dyn std::error::Error>> {
        let fallback = env_optional_u16("PROXIMA_DEMO_WAKE_MAX_ROUNDS")?;
        let mut rounds = Self {
            visionary: env_u16_with_fallback("PROXIMA_DEMO_VISIONARY_MAX_ROUNDS", 5, fallback)?,
            planner: env_u16_with_fallback("PROXIMA_DEMO_PLANNER_MAX_ROUNDS", 8, fallback)?,
            worker: env_u16_with_fallback("PROXIMA_DEMO_WORKER_MAX_ROUNDS", 14, fallback)?,
            verifier: env_u16_with_fallback("PROXIMA_DEMO_VERIFIER_MAX_ROUNDS", 10, fallback)?,
            goal_reviewer: env_u16_with_fallback(
                "PROXIMA_DEMO_GOAL_REVIEWER_MAX_ROUNDS",
                5,
                fallback,
            )?,
            wake_supervisor: env_u16_with_fallback(
                "PROXIMA_DEMO_WAKE_SUPERVISOR_MAX_ROUNDS",
                3,
                fallback,
            )?,
        };
        if mode == DemoInterventionMode::ForceContinue {
            rounds.visionary = 1;
            rounds.wake_supervisor = rounds.wake_supervisor.max(3);
        }
        Ok(rounds)
    }
}
