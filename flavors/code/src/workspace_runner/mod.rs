//! Code-flavor workspace runner.
//!
//! Core owns only wake dispatch and the runner trait. This module owns
//! repo, branch, worktree, and workspace-run Fact semantics.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::time::Instant;

use proxima_core::verbs::event_ingest::{CitationMappingHint, CitedObjectHint, EventDraft};
use proxima_core::{
    CORE_DERIVED_FROM_RELATION, FactPayload, MemoryId, Owner, SchemaId, SchemaVersion,
    SourceBatchId, SourceId, WorkspaceFinalizeInput, WorkspacePrepareInput, WorkspacePreparedRun,
    WorkspaceRunRecord, WorkspaceRunner, WorkspaceRunnerError,
};
use proxima_storage_pg::verbs::edge_append::{EdgeDraft, append_edge_in_tx};
use proxima_storage_pg::verbs::event_ingest::ingest_event_in_tx;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use tokio::process::Command;
use uuid::Uuid;

use crate::payloads::{
    ExecutionRequestV1, WorkspaceDecision, WorkspaceDecisionV1, WorkspaceDiffFile,
    WorkspaceDiffStat, WorkspaceReviewV1, WorkspaceReviewVerdict, WorkspaceRunV1,
};
use crate::repos::owner_columns_pub;

pub const WORKSPACE_RUNNER_SOURCE_ID: &str = "proxima-code/workspace-runner";
pub const WORKSPACE_RUN_OBJECT_SCHEMA: &str = "proxima-code/workspace-run-object-v1";
pub const WORKSPACE_RUN_WHOLE_SCHEMA: &str = "proxima-code/workspace-run-whole-v1";
const MAX_PRELOADED_FILES: usize = 3;
const MAX_PRELOADED_FILE_BYTES: u64 = 24 * 1024;
const MAX_PRELOADED_TOTAL_BYTES: u64 = 48 * 1024;
const TOOL_OUTPUT_TAIL_BYTES: usize = 4 * 1024;
const REVIEW_DIFF_MAX_BYTES: usize = 96 * 1024;

#[derive(Debug, Default, Clone)]
pub struct CodeWorkspaceRunner {
    pool: Option<PgPool>,
    worktrees_root: Option<PathBuf>,
    pnpm_store_root: Option<PathBuf>,
    pnpm_executable: Option<PathBuf>,
}

impl CodeWorkspaceRunner {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool: Some(pool),
            worktrees_root: None,
            pnpm_store_root: None,
            pnpm_executable: None,
        }
    }

    #[must_use]
    pub fn with_worktrees_root(mut self, root: PathBuf) -> Self {
        self.worktrees_root = Some(root);
        self
    }

    #[must_use]
    pub fn with_pnpm_store_root(mut self, root: PathBuf) -> Self {
        self.pnpm_store_root = Some(root);
        self
    }

    #[must_use]
    pub fn with_pnpm_executable(mut self, executable: PathBuf) -> Self {
        self.pnpm_executable = Some(executable);
        self
    }

    fn pool(&self) -> Result<&PgPool, WorkspaceRunnerError> {
        self.pool
            .as_ref()
            .ok_or(WorkspaceRunnerError::Unimplemented)
    }

    fn worktrees_root(&self) -> PathBuf {
        self.worktrees_root.clone().unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map_or_else(std::env::temp_dir, PathBuf::from)
                .join(".proxima")
                .join("worktrees")
        })
    }

    fn pnpm_store_root(&self) -> PathBuf {
        self.pnpm_store_root.clone().unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map_or_else(std::env::temp_dir, PathBuf::from)
                .join(".proxima")
                .join("pnpm-store")
        })
    }

    fn pnpm_executable(&self) -> PathBuf {
        self.pnpm_executable
            .clone()
            .unwrap_or_else(|| PathBuf::from("pnpm"))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PreparedState {
    repo_id: Uuid,
    canonical_path: String,
    target_branch: String,
    branch_name: String,
    parent_sha: String,
    worktree_path: String,
}

#[derive(Debug, sqlx::FromRow)]
struct RunnerRepoRow {
    canonical_path: String,
    target_branch: Option<String>,
}

#[async_trait::async_trait]
impl WorkspaceRunner for CodeWorkspaceRunner {
    async fn prepare(
        &self,
        input: WorkspacePrepareInput<'_>,
    ) -> Result<WorkspacePreparedRun, WorkspaceRunnerError> {
        match input.triggering_memory_schema_id {
            ExecutionRequestV1::SCHEMA_ID
            | "proxima-code/commit-v1"
            | "proxima-code/file-revision-v1"
            | "proxima-code/code-chunk-v1" => self.prepare_execution_request(input).await,
            WorkspaceRunV1::SCHEMA_ID => self.prepare_workspace_run_review(input).await,
            WorkspaceReviewV1::SCHEMA_ID => self.prepare_workspace_review_correction(input).await,
            WorkspaceDecisionV1::SCHEMA_ID => {
                self.prepare_workspace_decision_correction(input).await
            }
            other => Err(WorkspaceRunnerError::TriggerNotEligible(format!(
                "unsupported Code workspace trigger: {other}"
            ))),
        }
    }

    async fn finalize(
        &self,
        input: WorkspaceFinalizeInput<'_>,
    ) -> Result<WorkspaceRunRecord, WorkspaceRunnerError> {
        let pool = self.pool()?;
        let state: PreparedState = serde_json::from_value(input.prepared.runner_state.clone())
            .map_err(|err| {
                WorkspaceRunnerError::FinalizeFailed(format!("decode runner state: {err}"))
            })?;
        let worktree = Path::new(&state.worktree_path);
        let head_sha = git_output(worktree, &["rev-parse", "HEAD"])
            .await
            .map_err(|stderr| {
                WorkspaceRunnerError::FinalizeFailed(format!("rev-parse HEAD: {stderr}"))
            })?;
        let diff_stat = diff_stat(worktree, &state.parent_sha, &head_sha).await?;
        let payload = WorkspaceRunV1 {
            wake_invocation_id: input.invocation_id,
            repo_id: state.repo_id,
            target_branch: state.target_branch,
            worktree_path: state.worktree_path,
            branch_name: state.branch_name,
            parent_sha: state.parent_sha,
            head_sha: head_sha.clone(),
            diff_stat_json: diff_stat,
            exit_code: input.outcome.exit_code,
            stdout_tail: input.outcome.stdout_tail.clone(),
            stderr_tail: input.outcome.stderr_tail.clone(),
            duration_ms: input.outcome.duration_ms,
        };
        let memory_id = ingest_workspace_run(pool, &payload, input).await?;
        Ok(WorkspaceRunRecord {
            primary_memory_id: Some(memory_id),
        })
    }
}

impl CodeWorkspaceRunner {
    async fn prepare_execution_request(
        &self,
        input: WorkspacePrepareInput<'_>,
    ) -> Result<WorkspacePreparedRun, WorkspaceRunnerError> {
        let pool = self.pool()?;
        let repo_id = repo_id_from_payload(input.triggering_memory_payload)?;
        if let Some(prior_run) = load_continuation_workspace_run_for_request(
            pool,
            input.owner,
            input.triggering_memory_id,
        )
        .await?
        {
            if prior_run.repo_id != repo_id {
                return Err(WorkspaceRunnerError::PrepareFailed(format!(
                    "continuation workspace run repo {} does not match execution request repo {repo_id}",
                    prior_run.repo_id
                )));
            }
            let mut repo = load_repo(pool, input.owner, repo_id).await?;
            if repo.target_branch.is_none() {
                repo.target_branch = Some(prior_run.target_branch.clone());
            }
            let worktree_path = PathBuf::from(&prior_run.worktree_path);
            ensure_worktree_head(&worktree_path, &prior_run.branch_name, &prior_run.head_sha)
                .await?;
            let tooling = json!({
                "frontend": {
                    "pnpm": {
                        "status": "reused",
                        "reason": "continuation_worktree",
                    },
                },
            });
            let mut workspace_context = build_workspace_context(
                &input,
                repo_id,
                &repo,
                &prior_run.target_branch,
                &prior_run.parent_sha,
                &prior_run.branch_name,
                &worktree_path,
                tooling,
            )
            .await?;
            if let Some(object) = workspace_context.as_object_mut() {
                object.insert("mode".into(), json!("continue_execution_request"));
                object.insert(
                    "continuation_from".into(),
                    json!({
                        "workspace_run_memory_id": prior_run.memory_id.into_inner().to_string(),
                        "worktree_path": prior_run.worktree_path,
                        "branch_name": prior_run.branch_name,
                        "head_sha": prior_run.head_sha,
                    }),
                );
            }
            let state = PreparedState {
                repo_id,
                canonical_path: repo.canonical_path,
                target_branch: prior_run.target_branch.clone(),
                branch_name: prior_run.branch_name.clone(),
                parent_sha: prior_run.parent_sha.clone(),
                worktree_path: worktree_path.to_string_lossy().to_string(),
            };
            let runner_state = serde_json::to_value(&state).map_err(|err| {
                WorkspaceRunnerError::Internal(format!("serialize runner state: {err}"))
            })?;
            return Ok(WorkspacePreparedRun {
                work_dir: worktree_path,
                effective_recipe_path: input.effective_recipe_path.to_path_buf(),
                workspace_context: Some(workspace_context),
                runner_state,
            });
        }
        let mut repo = load_repo(pool, input.owner, repo_id).await?;
        if repo.target_branch.is_none() {
            let inferred = crate::repos::infer_missing_target_branch(pool, input.owner, repo_id)
                .await
                .map_err(|err| {
                    WorkspaceRunnerError::PrepareFailed(format!(
                        "repo {repo_id} target_branch inference failed: {err}"
                    ))
                })?;
            repo.target_branch = inferred.target_branch;
        }
        let target_branch = repo.target_branch.clone().ok_or_else(|| {
            WorkspaceRunnerError::PrepareFailed(format!("repo {repo_id} has no target_branch"))
        })?;
        let parent_sha = git_output(
            Path::new(&repo.canonical_path),
            &["rev-parse", &target_branch],
        )
        .await
        .map_err(|stderr| {
            WorkspaceRunnerError::PrepareFailed(format!(
                "target branch {target_branch} invalid: {stderr}"
            ))
        })?;
        let branch_name = format!("proxima/wake/{}", input.invocation_id);
        let owner_component = owner_component(input.owner);
        let worktree_path = self
            .worktrees_root()
            .join(owner_component)
            .join(input.invocation_id.to_string());
        if let Some(parent) = worktree_path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|err| {
                WorkspaceRunnerError::PrepareFailed(format!("create worktree parent: {err}"))
            })?;
        }
        let worktree_arg = worktree_path.to_string_lossy().to_string();
        git_output(
            Path::new(&repo.canonical_path),
            &[
                "worktree",
                "add",
                "-b",
                &branch_name,
                &worktree_arg,
                &parent_sha,
            ],
        )
        .await
        .map_err(|stderr| WorkspaceRunnerError::PrepareFailed(format!("worktree add: {stderr}")))?;
        let tooling = hydrate_workspace_tooling(
            &worktree_path,
            &self.pnpm_store_root(),
            &self.pnpm_executable(),
        )
        .await;

        let workspace_context = build_workspace_context(
            &input,
            repo_id,
            &repo,
            &target_branch,
            &parent_sha,
            &branch_name,
            &worktree_path,
            tooling,
        )
        .await?;

        let state = PreparedState {
            repo_id,
            canonical_path: repo.canonical_path,
            target_branch,
            branch_name,
            parent_sha,
            worktree_path: worktree_arg,
        };
        let runner_state = serde_json::to_value(&state).map_err(|err| {
            WorkspaceRunnerError::Internal(format!("serialize runner state: {err}"))
        })?;
        Ok(WorkspacePreparedRun {
            work_dir: worktree_path,
            effective_recipe_path: input.effective_recipe_path.to_path_buf(),
            workspace_context: Some(workspace_context),
            runner_state,
        })
    }

    async fn prepare_workspace_run_review(
        &self,
        input: WorkspacePrepareInput<'_>,
    ) -> Result<WorkspacePreparedRun, WorkspaceRunnerError> {
        let pool = self.pool()?;
        let run = parse_payload::<WorkspaceRunV1>(
            input.triggering_memory_payload,
            WorkspaceRunV1::SCHEMA_ID,
        )?;
        let repo = load_repo(pool, input.owner, run.repo_id).await?;
        let worktree_path = PathBuf::from(&run.worktree_path);
        ensure_worktree_head(&worktree_path, &run.branch_name, &run.head_sha).await?;
        let original_request =
            load_execution_request_for_run(pool, input.owner, input.triggering_memory_id).await?;
        let active_goal =
            load_goal_context_for_request(pool, input.owner, original_request.memory_id).await?;
        let veto_count =
            veto_count_for_request(pool, input.owner, original_request.memory_id).await?;
        let diff = build_review_diff_context(&worktree_path, &run).await?;
        let context = json!({
            "mode": "verify_workspace_run",
            "repo_id": run.repo_id.to_string(),
            "canonical_path": repo.canonical_path,
            "target_branch": run.target_branch,
            "worktree_path": run.worktree_path,
            "branch_name": run.branch_name,
            "parent_sha": run.parent_sha,
            "head_sha": run.head_sha,
            "workspace_run_memory_id": input.triggering_memory_id.into_inner().to_string(),
            "original_request": original_request.to_json(),
            "active_goal": active_goal,
            "diff_stat": run.diff_stat_json,
            "diff": diff,
            "log_tails": {
                "stdout_tail": run.stdout_tail,
                "stderr_tail": run.stderr_tail,
                "exit_code": run.exit_code,
                "duration_ms": run.duration_ms,
            },
            "veto_count": veto_count,
            "max_veto_rounds": crate::mcp::MAX_WORKSPACE_VETO_ROUNDS,
        });
        self.prepared_from_existing_run(input, &run, context)
    }

    async fn prepare_workspace_review_correction(
        &self,
        input: WorkspacePrepareInput<'_>,
    ) -> Result<WorkspacePreparedRun, WorkspaceRunnerError> {
        let pool = self.pool()?;
        let review = parse_payload::<WorkspaceReviewV1>(
            input.triggering_memory_payload,
            WorkspaceReviewV1::SCHEMA_ID,
        )?;
        if review.verdict != WorkspaceReviewVerdict::Rejected {
            return Err(WorkspaceRunnerError::TriggerNotEligible(
                "workspace review is not rejected".into(),
            ));
        }
        let original_request = load_execution_request(
            pool,
            input.owner,
            MemoryId::new(review.execution_request_memory_id),
        )
        .await?;
        let run = load_workspace_run(
            pool,
            input.owner,
            MemoryId::new(review.workspace_run_memory_id),
        )
        .await?;
        let repo = load_repo(pool, input.owner, run.repo_id).await?;
        let veto_count =
            veto_count_for_request(pool, input.owner, original_request.memory_id).await?;
        let prior_reviews =
            load_reviews_for_request(pool, input.owner, original_request.memory_id).await?;
        let prior_decisions =
            load_decisions_for_request(pool, input.owner, original_request.memory_id).await?;
        let target_worker = load_target_worker_personality_for_request(
            pool,
            input.owner,
            original_request.memory_id,
        )
        .await?;
        let context = json!({
            "mode": "plan_workspace_correction",
            "trigger_kind": "workspace_review",
            "repo_id": run.repo_id.to_string(),
            "canonical_path": repo.canonical_path,
            "target_branch": run.target_branch,
            "worktree_path": run.worktree_path,
            "branch_name": run.branch_name,
            "parent_sha": run.parent_sha,
            "head_sha": run.head_sha,
            "workspace_run_memory_id": review.workspace_run_memory_id.to_string(),
            "workspace_review_memory_id": input.triggering_memory_id.into_inner().to_string(),
            "original_request": original_request.to_json(),
            "rejected_review": review,
            "prior_reviews": prior_reviews,
            "prior_decisions": prior_decisions,
            "veto_count": veto_count,
            "max_veto_rounds": crate::mcp::MAX_WORKSPACE_VETO_ROUNDS,
            "target_worker_personality": target_worker.map(|id| id.to_string()),
        });
        self.prepared_from_existing_run(input, &run, context)
    }

    async fn prepare_workspace_decision_correction(
        &self,
        input: WorkspacePrepareInput<'_>,
    ) -> Result<WorkspacePreparedRun, WorkspaceRunnerError> {
        let pool = self.pool()?;
        let decision = parse_payload::<WorkspaceDecisionV1>(
            input.triggering_memory_payload,
            WorkspaceDecisionV1::SCHEMA_ID,
        )?;
        if decision.decision != WorkspaceDecision::RetryRequested {
            return Err(WorkspaceRunnerError::TriggerNotEligible(
                "workspace decision is not a retry request".into(),
            ));
        }
        let run = load_workspace_run(
            pool,
            input.owner,
            MemoryId::new(decision.workspace_run_memory_id),
        )
        .await?;
        let repo = load_repo(pool, input.owner, run.repo_id).await?;
        let original_request = load_execution_request_for_run(
            pool,
            input.owner,
            MemoryId::new(decision.workspace_run_memory_id),
        )
        .await?;
        let latest_review = load_latest_review_for_run(
            pool,
            input.owner,
            MemoryId::new(decision.workspace_run_memory_id),
        )
        .await?;
        let latest_rejected_review = load_latest_rejected_review_for_run(
            pool,
            input.owner,
            MemoryId::new(decision.workspace_run_memory_id),
        )
        .await?;
        let veto_count =
            veto_count_for_request(pool, input.owner, original_request.memory_id).await?;
        let prior_reviews =
            load_reviews_for_request(pool, input.owner, original_request.memory_id).await?;
        let prior_decisions =
            load_decisions_for_request(pool, input.owner, original_request.memory_id).await?;
        let target_worker = load_target_worker_personality_for_request(
            pool,
            input.owner,
            original_request.memory_id,
        )
        .await?;
        let context = json!({
            "mode": "plan_workspace_correction",
            "trigger_kind": "workspace_decision",
            "repo_id": run.repo_id.to_string(),
            "canonical_path": repo.canonical_path,
            "target_branch": run.target_branch,
            "worktree_path": run.worktree_path,
            "branch_name": run.branch_name,
            "parent_sha": run.parent_sha,
            "head_sha": run.head_sha,
            "workspace_run_memory_id": decision.workspace_run_memory_id.to_string(),
            "workspace_decision_memory_id": input.triggering_memory_id.into_inner().to_string(),
            "original_request": original_request.to_json(),
            "retry_requested_decision": decision,
            "latest_review": latest_review,
            "latest_rejected_review": latest_rejected_review,
            "prior_reviews": prior_reviews,
            "prior_decisions": prior_decisions,
            "veto_count": veto_count,
            "max_veto_rounds": crate::mcp::MAX_WORKSPACE_VETO_ROUNDS,
            "target_worker_personality": target_worker.map(|id| id.to_string()),
        });
        self.prepared_from_existing_run(input, &run, context)
    }

    fn prepared_from_existing_run(
        &self,
        input: WorkspacePrepareInput<'_>,
        run: &WorkspaceRunV1,
        workspace_context: serde_json::Value,
    ) -> Result<WorkspacePreparedRun, WorkspaceRunnerError> {
        let state = PreparedState {
            repo_id: run.repo_id,
            canonical_path: String::new(),
            target_branch: run.target_branch.clone(),
            branch_name: run.branch_name.clone(),
            parent_sha: run.parent_sha.clone(),
            worktree_path: run.worktree_path.clone(),
        };
        let runner_state = serde_json::to_value(&state).map_err(|err| {
            WorkspaceRunnerError::Internal(format!("serialize runner state: {err}"))
        })?;
        Ok(WorkspacePreparedRun {
            work_dir: PathBuf::from(&run.worktree_path),
            effective_recipe_path: input.effective_recipe_path.to_path_buf(),
            workspace_context: Some(workspace_context),
            runner_state,
        })
    }
}

async fn build_workspace_context(
    input: &WorkspacePrepareInput<'_>,
    repo_id: Uuid,
    repo: &RunnerRepoRow,
    target_branch: &str,
    parent_sha: &str,
    branch_name: &str,
    worktree_path: &Path,
    tooling: serde_json::Value,
) -> Result<serde_json::Value, WorkspaceRunnerError> {
    let instructions = input
        .triggering_memory_payload
        .get("instructions")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let mentioned_paths = extract_mentioned_paths(instructions);
    let preloaded_files = preload_mentioned_files(worktree_path, &mentioned_paths).await?;
    Ok(json!({
        "repo_id": repo_id.to_string(),
        "canonical_path": repo.canonical_path,
        "target_branch": target_branch,
        "worktree_path": worktree_path.to_string_lossy(),
        "branch_name": branch_name,
        "parent_sha": parent_sha,
        "request_memory_id": input.triggering_memory_id.into_inner().to_string(),
        "request_key": input
            .triggering_memory_payload
            .get("request_key")
            .and_then(serde_json::Value::as_str),
        "tooling": tooling,
        "mentioned_paths": mentioned_paths,
        "preloaded_files": preloaded_files,
    }))
}

async fn hydrate_workspace_tooling(
    worktree_path: &Path,
    pnpm_store_root: &Path,
    pnpm_executable: &Path,
) -> serde_json::Value {
    let pnpm_lock = worktree_path.join("pnpm-lock.yaml");
    if tokio::fs::metadata(&pnpm_lock).await.is_err() {
        return json!({
            "frontend": {
                "pnpm": {
                    "status": "skipped",
                    "reason": "no_pnpm_lock",
                },
            },
        });
    }

    let started = Instant::now();
    let store_dir = pnpm_store_root.to_string_lossy().to_string();
    if let Err(err) = tokio::fs::create_dir_all(pnpm_store_root).await {
        return json!({
            "frontend": {
                "pnpm": {
                    "status": "failed",
                    "reason": "create_store_dir_failed",
                    "store_dir": store_dir,
                    "duration_ms": duration_ms(started),
                    "stderr_tail": err.to_string(),
                },
            },
        });
    }

    let output = Command::new(pnpm_executable)
        .args([
            "install",
            "--frozen-lockfile",
            "--prefer-offline",
            "--store-dir",
            &store_dir,
        ])
        .current_dir(worktree_path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await;
    match output {
        Ok(output) => json!({
            "frontend": {
                "pnpm": {
                    "status": if output.status.success() { "succeeded" } else { "failed" },
                    "command": [
                        "pnpm",
                        "install",
                        "--frozen-lockfile",
                        "--prefer-offline",
                        "--store-dir",
                        store_dir,
                    ],
                    "store_dir": store_dir,
                    "exit_code": output.status.code(),
                    "duration_ms": duration_ms(started),
                    "stdout_tail": utf8_tail(&output.stdout, TOOL_OUTPUT_TAIL_BYTES),
                    "stderr_tail": utf8_tail(&output.stderr, TOOL_OUTPUT_TAIL_BYTES),
                },
            },
        }),
        Err(err) => json!({
            "frontend": {
                "pnpm": {
                    "status": "failed",
                    "reason": "spawn_failed",
                    "command": [
                        "pnpm",
                        "install",
                        "--frozen-lockfile",
                        "--prefer-offline",
                        "--store-dir",
                        store_dir,
                    ],
                    "store_dir": store_dir,
                    "duration_ms": duration_ms(started),
                    "stderr_tail": err.to_string(),
                },
            },
        }),
    }
}

fn duration_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn utf8_tail(bytes: &[u8], limit: usize) -> String {
    let start = bytes.len().saturating_sub(limit);
    String::from_utf8_lossy(&bytes[start..]).trim().to_string()
}

fn extract_mentioned_paths(instructions: &str) -> Vec<String> {
    let mut paths = BTreeSet::new();
    let mut rest = instructions;
    while let Some(start) = rest.find('`') {
        let after_start = &rest[start + 1..];
        let Some(end) = after_start.find('`') else {
            break;
        };
        maybe_insert_path(&mut paths, after_start[..end].trim());
        rest = &after_start[end + 1..];
    }

    for token in instructions.split_whitespace() {
        let candidate = token.trim_matches(|ch: char| {
            matches!(
                ch,
                ',' | '.' | ':' | ';' | '!' | '?' | ')' | '(' | '[' | ']' | '{' | '}' | '"' | '\''
            )
        });
        maybe_insert_path(&mut paths, candidate);
    }

    paths.into_iter().take(64).collect()
}

fn maybe_insert_path(paths: &mut BTreeSet<String>, candidate: &str) {
    if candidate.is_empty()
        || candidate.starts_with('/')
        || candidate.starts_with('-')
        || candidate.contains("://")
        || candidate.contains('\n')
        || candidate.len() > 240
    {
        return;
    }
    let looks_like_path = candidate.contains('/')
        || Path::new(candidate)
            .extension()
            .and_then(std::ffi::OsStr::to_str)
            .is_some_and(|ext| (1..=8).contains(&ext.len()));
    if !looks_like_path || !is_safe_relative_path(candidate) {
        return;
    }
    paths.insert(candidate.to_string());
}

fn is_safe_relative_path(candidate: &str) -> bool {
    let path = Path::new(candidate);
    !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_) | Component::CurDir))
}

async fn preload_mentioned_files(
    worktree_path: &Path,
    mentioned_paths: &[String],
) -> Result<serde_json::Value, WorkspaceRunnerError> {
    let mut files = Vec::new();
    let mut omitted = Vec::new();
    let mut total_bytes = 0u64;

    for relative in mentioned_paths {
        let path = Path::new(relative);
        if !is_safe_relative_path(relative) {
            continue;
        }
        let full_path = worktree_path.join(path);
        let metadata = match tokio::fs::metadata(&full_path).await {
            Ok(metadata) if metadata.is_file() => metadata,
            _ => continue,
        };
        let size = metadata.len();
        if files.len() >= MAX_PRELOADED_FILES {
            omitted.push(json!({
                "path": relative,
                "size_bytes": size,
                "reason": "file_count_limit",
            }));
            continue;
        }
        if size > MAX_PRELOADED_FILE_BYTES {
            omitted.push(json!({
                "path": relative,
                "size_bytes": size,
                "reason": "file_too_large",
            }));
            continue;
        }
        if total_bytes.saturating_add(size) > MAX_PRELOADED_TOTAL_BYTES {
            omitted.push(json!({
                "path": relative,
                "size_bytes": size,
                "reason": "total_bytes_limit",
            }));
            continue;
        }
        let bytes = tokio::fs::read(&full_path).await.map_err(|err| {
            WorkspaceRunnerError::PrepareFailed(format!("read mentioned file {relative}: {err}"))
        })?;
        total_bytes = total_bytes.saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
        files.push(json!({
            "path": relative,
            "size_bytes": size,
            "line_count": String::from_utf8_lossy(&bytes).lines().count(),
            "sha256": hex::encode(Sha256::digest(&bytes)),
            "content": String::from_utf8_lossy(&bytes),
        }));
    }

    Ok(json!({
        "files": files,
        "omitted": omitted,
        "limits": {
            "max_files": MAX_PRELOADED_FILES,
            "max_file_bytes": MAX_PRELOADED_FILE_BYTES,
            "max_total_bytes": MAX_PRELOADED_TOTAL_BYTES,
        },
    }))
}

fn parse_payload<T: DeserializeOwned>(
    payload: &serde_json::Value,
    schema_id: &str,
) -> Result<T, WorkspaceRunnerError> {
    serde_json::from_value(payload.clone()).map_err(|err| {
        WorkspaceRunnerError::PrepareFailed(format!("decode {schema_id} payload: {err}"))
    })
}

#[derive(Debug, Clone)]
struct LoadedExecutionRequest {
    memory_id: MemoryId,
    payload: ExecutionRequestV1,
}

impl LoadedExecutionRequest {
    fn to_json(&self) -> serde_json::Value {
        json!({
            "memory_id": self.memory_id.into_inner().to_string(),
            "payload": self.payload,
        })
    }
}

async fn load_execution_request(
    pool: &PgPool,
    owner: &Owner,
    memory_id: MemoryId,
) -> Result<LoadedExecutionRequest, WorkspaceRunnerError> {
    let (owner_kind, owner_principal_id, _) = owner_columns_pub(owner);
    let row = sqlx::query(
        "SELECT COALESCE(m.kind, 'Fact') AS kind,
                m.schema_id,
                r.repo_id,
                r.title,
                r.instructions,
                r.request_key
         FROM proxima_core.memories m
         LEFT JOIN proxima_code.execution_request_v1 r USING (memory_id)
         WHERE m.memory_id = $1
           AND m.owner_principal_kind = $2
           AND m.owner_principal_id = $3",
    )
    .bind(memory_id.into_inner())
    .bind(owner_kind)
    .bind(owner_principal_id)
    .fetch_optional(pool)
    .await
    .map_err(|err| WorkspaceRunnerError::Internal(format!("load execution request: {err}")))?;
    let Some(row) = row else {
        return Err(WorkspaceRunnerError::PrepareFailed(format!(
            "execution request not found: {}",
            memory_id.into_inner()
        )));
    };
    let kind: String = row.try_get("kind").map_err(map_sqlx_internal)?;
    let schema_id: String = row.try_get("schema_id").map_err(map_sqlx_internal)?;
    if kind != "Fact" || schema_id != ExecutionRequestV1::SCHEMA_ID {
        return Err(WorkspaceRunnerError::PrepareFailed(format!(
            "memory {} is not an execution request",
            memory_id.into_inner()
        )));
    }
    let repo_id: Option<Uuid> = row.try_get("repo_id").map_err(map_sqlx_internal)?;
    let title: Option<String> = row.try_get("title").map_err(map_sqlx_internal)?;
    let instructions: Option<String> = row.try_get("instructions").map_err(map_sqlx_internal)?;
    let request_key: Option<String> = row.try_get("request_key").map_err(map_sqlx_internal)?;
    let (Some(repo_id), Some(title), Some(instructions), Some(request_key)) =
        (repo_id, title, instructions, request_key)
    else {
        return Err(WorkspaceRunnerError::PrepareFailed(format!(
            "execution request sidecar missing: {}",
            memory_id.into_inner()
        )));
    };
    Ok(LoadedExecutionRequest {
        memory_id,
        payload: ExecutionRequestV1 {
            repo_id,
            title,
            instructions,
            request_key,
        },
    })
}

#[derive(Debug, Clone)]
struct LoadedWorkspaceRun {
    memory_id: MemoryId,
    payload: WorkspaceRunV1,
}

impl std::ops::Deref for LoadedWorkspaceRun {
    type Target = WorkspaceRunV1;

    fn deref(&self) -> &Self::Target {
        &self.payload
    }
}

async fn load_execution_request_for_run(
    pool: &PgPool,
    owner: &Owner,
    run_memory_id: MemoryId,
) -> Result<LoadedExecutionRequest, WorkspaceRunnerError> {
    let (owner_kind, owner_principal_id, _) = owner_columns_pub(owner);
    let request_id: Option<Uuid> = sqlx::query_scalar(
        "WITH RECURSIVE ancestry(memory_id, depth, path) AS (
             SELECT e.target_memory_id, 1, ARRAY[$4::uuid, e.target_memory_id]
             FROM proxima_core.edges e
             WHERE e.owner_principal_kind = $1
               AND e.owner_principal_id = $2
               AND e.relation = $3
               AND e.source_kind = 'Fact'
               AND e.source_memory_id = $4
               AND e.target_kind = 'Fact'
               AND e.target_memory_id IS NOT NULL
             UNION ALL
             SELECT e.target_memory_id, a.depth + 1, a.path || e.target_memory_id
             FROM ancestry a
             JOIN proxima_core.edges e
               ON e.owner_principal_kind = $1
              AND e.owner_principal_id = $2
              AND e.relation = $3
              AND e.source_kind = 'Fact'
              AND e.source_memory_id = a.memory_id
              AND e.target_kind = 'Fact'
              AND e.target_memory_id IS NOT NULL
             WHERE NOT e.target_memory_id = ANY(a.path)
         )
         SELECT a.memory_id
         FROM ancestry a
         JOIN proxima_core.memories m
           ON m.memory_id = a.memory_id
          AND m.owner_principal_kind = $1
          AND m.owner_principal_id = $2
         WHERE m.schema_id = $5
         ORDER BY a.depth DESC, a.memory_id DESC
         LIMIT 1",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(CORE_DERIVED_FROM_RELATION)
    .bind(run_memory_id.into_inner())
    .bind(ExecutionRequestV1::SCHEMA_ID)
    .fetch_optional(pool)
    .await
    .map_err(|err| {
        WorkspaceRunnerError::Internal(format!("find execution request for run: {err}"))
    })?;
    let Some(request_id) = request_id else {
        return Err(WorkspaceRunnerError::PrepareFailed(format!(
            "workspace run has no derived-from execution request: {}",
            run_memory_id.into_inner()
        )));
    };
    load_execution_request(pool, owner, MemoryId::new(request_id)).await
}

async fn load_continuation_workspace_run_for_request(
    pool: &PgPool,
    owner: &Owner,
    request_memory_id: MemoryId,
) -> Result<Option<LoadedWorkspaceRun>, WorkspaceRunnerError> {
    let (owner_kind, owner_principal_id, _) = owner_columns_pub(owner);
    let run_id: Option<Uuid> = sqlx::query_scalar(
        "WITH RECURSIVE ancestry(memory_id, depth, path) AS (
             SELECT e.target_memory_id, 1, ARRAY[$4::uuid, e.target_memory_id]
             FROM proxima_core.edges e
             WHERE e.owner_principal_kind = $1
               AND e.owner_principal_id = $2
               AND e.relation = $3
               AND e.source_kind = 'Fact'
               AND e.source_memory_id = $4
               AND e.target_kind = 'Fact'
               AND e.target_memory_id IS NOT NULL
             UNION ALL
             SELECT e.target_memory_id, a.depth + 1, a.path || e.target_memory_id
             FROM ancestry a
             JOIN proxima_core.edges e
               ON e.owner_principal_kind = $1
              AND e.owner_principal_id = $2
              AND e.relation = $3
              AND e.source_kind = 'Fact'
              AND e.source_memory_id = a.memory_id
              AND e.target_kind = 'Fact'
              AND e.target_memory_id IS NOT NULL
             WHERE NOT e.target_memory_id = ANY(a.path)
         )
         SELECT a.memory_id
         FROM ancestry a
         JOIN proxima_core.memories m
           ON m.memory_id = a.memory_id
          AND m.owner_principal_kind = $1
          AND m.owner_principal_id = $2
         WHERE m.schema_id = $5
         ORDER BY a.depth, a.memory_id DESC
         LIMIT 1",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(CORE_DERIVED_FROM_RELATION)
    .bind(request_memory_id.into_inner())
    .bind(WorkspaceRunV1::SCHEMA_ID)
    .fetch_optional(pool)
    .await
    .map_err(|err| {
        WorkspaceRunnerError::Internal(format!("find continuation workspace run: {err}"))
    })?;
    match run_id {
        Some(run_id) => {
            let memory_id = MemoryId::new(run_id);
            let payload = load_workspace_run(pool, owner, memory_id).await?;
            Ok(Some(LoadedWorkspaceRun { memory_id, payload }))
        }
        None => Ok(None),
    }
}

async fn load_goal_context_for_request(
    pool: &PgPool,
    owner: &Owner,
    request_memory_id: MemoryId,
) -> Result<Option<serde_json::Value>, WorkspaceRunnerError> {
    let goal_tables_exist: bool = sqlx::query_scalar(
        "SELECT to_regclass('proxima_goal.goal_activated_v1') IS NOT NULL
             AND to_regclass('proxima_core.goals') IS NOT NULL",
    )
    .fetch_one(pool)
    .await
    .map_err(|err| WorkspaceRunnerError::Internal(format!("check goal tables: {err}")))?;
    if !goal_tables_exist {
        return Ok(None);
    }

    let (owner_kind, owner_principal_id, _) = owner_columns_pub(owner);
    let row = sqlx::query(
        "WITH RECURSIVE ancestry(memory_id, depth, path) AS (
             SELECT e.target_memory_id, 1, ARRAY[$4::uuid, e.target_memory_id]
             FROM proxima_core.edges e
             WHERE e.owner_principal_kind = $1
               AND e.owner_principal_id = $2
               AND e.relation = $3
               AND e.source_kind = 'Fact'
               AND e.source_memory_id = $4
               AND e.target_kind = 'Fact'
               AND e.target_memory_id IS NOT NULL
             UNION ALL
             SELECT e.target_memory_id, a.depth + 1, a.path || e.target_memory_id
             FROM ancestry a
             JOIN proxima_core.edges e
               ON e.owner_principal_kind = $1
              AND e.owner_principal_id = $2
              AND e.relation = $3
              AND e.source_kind = 'Fact'
              AND e.source_memory_id = a.memory_id
              AND e.target_kind = 'Fact'
              AND e.target_memory_id IS NOT NULL
             WHERE NOT e.target_memory_id = ANY(a.path)
         ),
         activated AS (
             SELECT a.memory_id,
                    g.goal_id,
                    g.schema_id,
                    g.title,
                    g.accepted_at,
                    g.evidence_count
             FROM ancestry a
             JOIN proxima_core.memories m
               ON m.memory_id = a.memory_id
              AND m.owner_principal_kind = $1
              AND m.owner_principal_id = $2
              AND m.schema_id = 'proxima-goal/goal-activated-v1'
             JOIN proxima_goal.goal_activated_v1 g
               ON g.memory_id = a.memory_id
             ORDER BY a.depth, a.memory_id DESC
             LIMIT 1
         ),
         goal_lineage(goal_id, depth, path) AS (
             SELECT goal_id, 0, ARRAY[goal_id]
             FROM activated
             UNION ALL
             SELECT child.goal_id, gl.depth + 1, gl.path || child.goal_id
             FROM goal_lineage gl
             JOIN proxima_core.goals child
               ON child.supersedes = gl.goal_id
              AND child.owner_principal_kind = $1
              AND child.owner_principal_id = $2
             WHERE NOT child.goal_id = ANY(gl.path)
         )
         SELECT a.memory_id AS activated_memory_id,
                a.goal_id AS activated_goal_id,
                a.schema_id AS activated_schema_id,
                a.title AS activated_title,
                a.accepted_at,
                a.evidence_count,
                gh.goal_id AS head_goal_id,
                gh.schema_id AS head_schema_id,
                gh.schema_version AS head_schema_version,
                gh.title AS head_title,
                gh.text AS head_text,
                gh.state AS head_state,
                gh.supersedes AS head_supersedes,
                gh.created_at AS head_created_at
         FROM activated a
         JOIN goal_lineage gl ON true
         JOIN proxima_core.goals gh ON gh.goal_id = gl.goal_id
         ORDER BY gl.depth DESC, gh.created_at DESC
         LIMIT 1",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(CORE_DERIVED_FROM_RELATION)
    .bind(request_memory_id.into_inner())
    .fetch_optional(pool)
    .await
    .map_err(|err| WorkspaceRunnerError::Internal(format!("load active goal context: {err}")))?;

    let Some(row) = row else {
        return Ok(None);
    };
    let activated_memory_id: Uuid = row
        .try_get("activated_memory_id")
        .map_err(map_sqlx_internal)?;
    let activated_goal_id: Uuid = row
        .try_get("activated_goal_id")
        .map_err(map_sqlx_internal)?;
    let activated_schema_id: String = row
        .try_get("activated_schema_id")
        .map_err(map_sqlx_internal)?;
    let activated_title: String = row.try_get("activated_title").map_err(map_sqlx_internal)?;
    let accepted_at: time::OffsetDateTime =
        row.try_get("accepted_at").map_err(map_sqlx_internal)?;
    let evidence_count: i32 = row.try_get("evidence_count").map_err(map_sqlx_internal)?;
    let head_goal_id: Uuid = row.try_get("head_goal_id").map_err(map_sqlx_internal)?;
    let head_schema_id: String = row.try_get("head_schema_id").map_err(map_sqlx_internal)?;
    let head_schema_version: i32 = row
        .try_get("head_schema_version")
        .map_err(map_sqlx_internal)?;
    let head_title: String = row.try_get("head_title").map_err(map_sqlx_internal)?;
    let head_text: String = row.try_get("head_text").map_err(map_sqlx_internal)?;
    let head_state: String = row.try_get("head_state").map_err(map_sqlx_internal)?;
    let head_supersedes: Option<Uuid> =
        row.try_get("head_supersedes").map_err(map_sqlx_internal)?;
    let head_created_at: time::OffsetDateTime =
        row.try_get("head_created_at").map_err(map_sqlx_internal)?;
    Ok(Some(json!({
        "activated_memory_id": activated_memory_id.to_string(),
        "activated": {
            "goal_id": activated_goal_id.to_string(),
            "schema_id": activated_schema_id,
            "title": activated_title,
            "accepted_at": accepted_at,
            "evidence_count": evidence_count,
        },
        "head": {
            "goal_id": head_goal_id.to_string(),
            "schema_id": head_schema_id,
            "schema_version": head_schema_version,
            "title": head_title,
            "text": head_text,
            "state": head_state,
            "supersedes": head_supersedes.map(|id| id.to_string()),
            "created_at": head_created_at,
        },
    })))
}

async fn load_workspace_run(
    pool: &PgPool,
    owner: &Owner,
    memory_id: MemoryId,
) -> Result<WorkspaceRunV1, WorkspaceRunnerError> {
    let (owner_kind, owner_principal_id, _) = owner_columns_pub(owner);
    let row = sqlx::query(
        "SELECT COALESCE(m.kind, 'Fact') AS kind,
                m.schema_id,
                r.wake_invocation_id,
                r.repo_id,
                r.target_branch,
                r.worktree_path,
                r.branch_name,
                r.parent_sha,
                r.head_sha,
                r.diff_stat_json,
                r.exit_code,
                r.stdout_tail,
                r.stderr_tail,
                r.duration_ms
         FROM proxima_core.memories m
         LEFT JOIN proxima_code.workspace_run_v1 r USING (memory_id)
         WHERE m.memory_id = $1
           AND m.owner_principal_kind = $2
           AND m.owner_principal_id = $3",
    )
    .bind(memory_id.into_inner())
    .bind(owner_kind)
    .bind(owner_principal_id)
    .fetch_optional(pool)
    .await
    .map_err(|err| WorkspaceRunnerError::Internal(format!("load workspace run: {err}")))?;
    let Some(row) = row else {
        return Err(WorkspaceRunnerError::PrepareFailed(format!(
            "workspace run not found: {}",
            memory_id.into_inner()
        )));
    };
    let kind: String = row.try_get("kind").map_err(map_sqlx_internal)?;
    let schema_id: String = row.try_get("schema_id").map_err(map_sqlx_internal)?;
    if kind != "Fact" || schema_id != WorkspaceRunV1::SCHEMA_ID {
        return Err(WorkspaceRunnerError::PrepareFailed(format!(
            "memory {} is not a workspace run",
            memory_id.into_inner()
        )));
    }
    let wake_invocation_id: Option<Uuid> = row
        .try_get("wake_invocation_id")
        .map_err(map_sqlx_internal)?;
    let repo_id: Option<Uuid> = row.try_get("repo_id").map_err(map_sqlx_internal)?;
    let target_branch: Option<String> = row.try_get("target_branch").map_err(map_sqlx_internal)?;
    let worktree_path: Option<String> = row.try_get("worktree_path").map_err(map_sqlx_internal)?;
    let branch_name: Option<String> = row.try_get("branch_name").map_err(map_sqlx_internal)?;
    let parent_sha: Option<String> = row.try_get("parent_sha").map_err(map_sqlx_internal)?;
    let head_sha: Option<String> = row.try_get("head_sha").map_err(map_sqlx_internal)?;
    let diff_stat_json: Option<serde_json::Value> =
        row.try_get("diff_stat_json").map_err(map_sqlx_internal)?;
    let exit_code: Option<i32> = row.try_get("exit_code").map_err(map_sqlx_internal)?;
    let stdout_tail: Option<String> = row.try_get("stdout_tail").map_err(map_sqlx_internal)?;
    let stderr_tail: Option<String> = row.try_get("stderr_tail").map_err(map_sqlx_internal)?;
    let duration_ms_raw: Option<i64> = row.try_get("duration_ms").map_err(map_sqlx_internal)?;
    let (
        Some(wake_invocation_id),
        Some(repo_id),
        Some(target_branch),
        Some(worktree_path),
        Some(branch_name),
        Some(parent_sha),
        Some(head_sha),
        Some(diff_stat_json),
    ) = (
        wake_invocation_id,
        repo_id,
        target_branch,
        worktree_path,
        branch_name,
        parent_sha,
        head_sha,
        diff_stat_json,
    )
    else {
        return Err(WorkspaceRunnerError::PrepareFailed(format!(
            "workspace run sidecar missing: {}",
            memory_id.into_inner()
        )));
    };
    let diff_stat_json = serde_json::from_value(diff_stat_json).map_err(|err| {
        WorkspaceRunnerError::PrepareFailed(format!("decode workspace run diff_stat_json: {err}"))
    })?;
    let duration_ms = duration_ms_raw.and_then(|value| u64::try_from(value).ok());
    Ok(WorkspaceRunV1 {
        wake_invocation_id,
        repo_id,
        target_branch,
        worktree_path,
        branch_name,
        parent_sha,
        head_sha,
        diff_stat_json,
        exit_code,
        stdout_tail,
        stderr_tail,
        duration_ms,
    })
}

async fn load_latest_review_for_run(
    pool: &PgPool,
    owner: &Owner,
    run_memory_id: MemoryId,
) -> Result<Option<serde_json::Value>, WorkspaceRunnerError> {
    let mut reviews = load_reviews_for_run(pool, owner, run_memory_id).await?;
    Ok(reviews.pop())
}

async fn load_latest_rejected_review_for_run(
    pool: &PgPool,
    owner: &Owner,
    run_memory_id: MemoryId,
) -> Result<Option<serde_json::Value>, WorkspaceRunnerError> {
    let mut reviews = load_review_rows(
        pool,
        owner,
        "r.workspace_run_memory_id = $4 AND r.verdict = 'rejected'",
        run_memory_id.into_inner(),
    )
    .await?;
    Ok(reviews.pop())
}

async fn load_reviews_for_request(
    pool: &PgPool,
    owner: &Owner,
    request_memory_id: MemoryId,
) -> Result<Vec<serde_json::Value>, WorkspaceRunnerError> {
    load_review_rows(
        pool,
        owner,
        "r.execution_request_memory_id = $4",
        request_memory_id.into_inner(),
    )
    .await
}

async fn load_reviews_for_run(
    pool: &PgPool,
    owner: &Owner,
    run_memory_id: MemoryId,
) -> Result<Vec<serde_json::Value>, WorkspaceRunnerError> {
    load_review_rows(
        pool,
        owner,
        "r.workspace_run_memory_id = $4",
        run_memory_id.into_inner(),
    )
    .await
}

async fn load_review_rows(
    pool: &PgPool,
    owner: &Owner,
    predicate: &str,
    predicate_id: Uuid,
) -> Result<Vec<serde_json::Value>, WorkspaceRunnerError> {
    let (owner_kind, owner_principal_id, _) = owner_columns_pub(owner);
    let sql = format!(
        "SELECT r.memory_id,
                r.workspace_run_memory_id,
                r.execution_request_memory_id,
                r.verdict,
                r.round_index,
                r.summary,
                r.findings_json,
                r.correction_instructions,
                r.verification_summary,
                r.reviewed_at
         FROM proxima_code.workspace_review_v1 r
         JOIN proxima_core.memories m USING (memory_id)
         WHERE m.owner_principal_kind = $1
           AND m.owner_principal_id = $2
           AND m.schema_id = $3
           AND {predicate}
         ORDER BY r.created_at, r.memory_id"
    );
    let rows = sqlx::query(&sql)
        .bind(owner_kind)
        .bind(owner_principal_id)
        .bind(WorkspaceReviewV1::SCHEMA_ID)
        .bind(predicate_id)
        .fetch_all(pool)
        .await
        .map_err(|err| WorkspaceRunnerError::Internal(format!("load reviews: {err}")))?;
    rows.into_iter()
        .map(review_row_to_json)
        .collect::<Result<Vec<_>, _>>()
}

fn review_row_to_json(
    row: sqlx::postgres::PgRow,
) -> Result<serde_json::Value, WorkspaceRunnerError> {
    let memory_id: Uuid = row.try_get("memory_id").map_err(map_sqlx_internal)?;
    let workspace_run_memory_id: Uuid = row
        .try_get("workspace_run_memory_id")
        .map_err(map_sqlx_internal)?;
    let execution_request_memory_id: Uuid = row
        .try_get("execution_request_memory_id")
        .map_err(map_sqlx_internal)?;
    let verdict: String = row.try_get("verdict").map_err(map_sqlx_internal)?;
    let round_index: i32 = row.try_get("round_index").map_err(map_sqlx_internal)?;
    let summary: String = row.try_get("summary").map_err(map_sqlx_internal)?;
    let findings: serde_json::Value = row.try_get("findings_json").map_err(map_sqlx_internal)?;
    let correction_instructions: Option<String> = row
        .try_get("correction_instructions")
        .map_err(map_sqlx_internal)?;
    let verification_summary: Option<String> = row
        .try_get("verification_summary")
        .map_err(map_sqlx_internal)?;
    let reviewed_at: time::OffsetDateTime =
        row.try_get("reviewed_at").map_err(map_sqlx_internal)?;
    Ok(json!({
        "memory_id": memory_id.to_string(),
        "workspace_run_memory_id": workspace_run_memory_id.to_string(),
        "execution_request_memory_id": execution_request_memory_id.to_string(),
        "verdict": verdict,
        "round_index": round_index,
        "summary": summary,
        "findings": findings,
        "correction_instructions": correction_instructions,
        "verification_summary": verification_summary,
        "reviewed_at": reviewed_at,
    }))
}

async fn load_decisions_for_request(
    pool: &PgPool,
    owner: &Owner,
    request_memory_id: MemoryId,
) -> Result<Vec<serde_json::Value>, WorkspaceRunnerError> {
    let (owner_kind, owner_principal_id, _) = owner_columns_pub(owner);
    let rows = sqlx::query(
        "SELECT d.memory_id,
                d.workspace_run_memory_id,
                d.decision,
                d.decided_at,
                d.reason_text,
                d.decided_by_owner_id
         FROM proxima_code.workspace_decision_v1 d
         JOIN proxima_core.memories dm USING (memory_id)
         JOIN proxima_core.edges e
           ON e.source_kind = 'Fact'
          AND e.source_memory_id = d.workspace_run_memory_id
          AND e.target_kind = 'Fact'
          AND e.target_memory_id = $4
          AND e.relation = $5
          AND e.owner_principal_kind = dm.owner_principal_kind
          AND e.owner_principal_id = dm.owner_principal_id
         WHERE dm.owner_principal_kind = $1
           AND dm.owner_principal_id = $2
           AND dm.schema_id = $3
         ORDER BY d.decided_at, d.memory_id",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(WorkspaceDecisionV1::SCHEMA_ID)
    .bind(request_memory_id.into_inner())
    .bind(CORE_DERIVED_FROM_RELATION)
    .fetch_all(pool)
    .await
    .map_err(|err| WorkspaceRunnerError::Internal(format!("load decisions: {err}")))?;
    rows.into_iter()
        .map(decision_row_to_json)
        .collect::<Result<Vec<_>, _>>()
}

fn decision_row_to_json(
    row: sqlx::postgres::PgRow,
) -> Result<serde_json::Value, WorkspaceRunnerError> {
    let memory_id: Uuid = row.try_get("memory_id").map_err(map_sqlx_internal)?;
    let workspace_run_memory_id: Uuid = row
        .try_get("workspace_run_memory_id")
        .map_err(map_sqlx_internal)?;
    let decision: String = row.try_get("decision").map_err(map_sqlx_internal)?;
    let decided_at: time::OffsetDateTime = row.try_get("decided_at").map_err(map_sqlx_internal)?;
    let reason_text: Option<String> = row.try_get("reason_text").map_err(map_sqlx_internal)?;
    let decided_by_owner_id: Uuid = row
        .try_get("decided_by_owner_id")
        .map_err(map_sqlx_internal)?;
    Ok(json!({
        "memory_id": memory_id.to_string(),
        "workspace_run_memory_id": workspace_run_memory_id.to_string(),
        "decision": decision,
        "decided_at": decided_at,
        "reason_text": reason_text,
        "decided_by_owner_id": decided_by_owner_id.to_string(),
    }))
}

async fn veto_count_for_request(
    pool: &PgPool,
    owner: &Owner,
    execution_request_memory_id: MemoryId,
) -> Result<i64, WorkspaceRunnerError> {
    let (owner_kind, owner_principal_id, _) = owner_columns_pub(owner);
    sqlx::query_scalar(
        "WITH review_vetoes AS (
             SELECT r.memory_id
             FROM proxima_code.workspace_review_v1 r
             JOIN proxima_core.memories m USING (memory_id)
             WHERE m.owner_principal_kind = $1
               AND m.owner_principal_id = $2
               AND r.execution_request_memory_id = $3
               AND r.verdict = 'rejected'
         ),
         decision_vetoes AS (
             SELECT d.memory_id
             FROM proxima_code.workspace_decision_v1 d
             JOIN proxima_core.memories dm USING (memory_id)
             JOIN proxima_core.edges e
               ON e.source_kind = 'Fact'
              AND e.source_memory_id = d.workspace_run_memory_id
              AND e.target_kind = 'Fact'
              AND e.target_memory_id = $3
              AND e.relation = $4
              AND e.owner_principal_kind = dm.owner_principal_kind
              AND e.owner_principal_id = dm.owner_principal_id
             WHERE dm.owner_principal_kind = $1
               AND dm.owner_principal_id = $2
               AND d.decision = 'retry_requested'
         )
         SELECT (SELECT count(*) FROM review_vetoes)
              + (SELECT count(*) FROM decision_vetoes)",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(execution_request_memory_id.into_inner())
    .bind(CORE_DERIVED_FROM_RELATION)
    .fetch_one(pool)
    .await
    .map_err(|err| WorkspaceRunnerError::Internal(format!("count vetoes: {err}")))
}

async fn load_target_worker_personality_for_request(
    pool: &PgPool,
    owner: &Owner,
    request_memory_id: MemoryId,
) -> Result<Option<Uuid>, WorkspaceRunnerError> {
    let (owner_kind, owner_principal_id, _) = owner_columns_pub(owner);
    sqlx::query_scalar(
        "SELECT p.personality_instance_id
         FROM proxima_core.edges e
         JOIN proxima_core.personality p
           ON p.current_root_perspective_memory_id = e.source_memory_id
          AND p.owner_principal_kind = e.owner_principal_kind
          AND p.owner_principal_id = e.owner_principal_id
         WHERE e.owner_principal_kind = $1
           AND e.owner_principal_id = $2
           AND e.relation = $3
           AND e.source_kind = 'Perspective'
           AND e.target_kind = 'Fact'
           AND e.target_memory_id = $4
         ORDER BY e.created_at DESC, e.edge_id DESC
         LIMIT 1",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(crate::mcp::CODE_TARGETS_EXECUTION_REQUEST_RELATION)
    .bind(request_memory_id.into_inner())
    .fetch_optional(pool)
    .await
    .map_err(|err| WorkspaceRunnerError::Internal(format!("load target worker: {err}")))
}

async fn ensure_worktree_head(
    worktree_path: &Path,
    branch_name: &str,
    head_sha: &str,
) -> Result<(), WorkspaceRunnerError> {
    if tokio::fs::metadata(worktree_path).await.is_err() {
        return Err(WorkspaceRunnerError::PrepareFailed(format!(
            "workspace worktree missing: {}",
            worktree_path.display()
        )));
    }
    if let Err(stderr) = git_output(worktree_path, &["checkout", branch_name]).await {
        git_output(worktree_path, &["checkout", head_sha])
            .await
            .map_err(|head_stderr| {
                WorkspaceRunnerError::PrepareFailed(format!(
                    "checkout {branch_name} failed: {stderr}; checkout {head_sha} failed: {head_stderr}"
                ))
            })?;
    }
    git_output(worktree_path, &["reset", "--hard", head_sha])
        .await
        .map_err(|stderr| {
            WorkspaceRunnerError::PrepareFailed(format!("reset worktree: {stderr}"))
        })?;
    Ok(())
}

async fn build_review_diff_context(
    worktree_path: &Path,
    run: &WorkspaceRunV1,
) -> Result<serde_json::Value, WorkspaceRunnerError> {
    let range = format!("{}..{}", run.parent_sha, run.head_sha);
    let stat = git_output(worktree_path, &["diff", "--stat", &range])
        .await
        .map_err(|stderr| WorkspaceRunnerError::PrepareFailed(format!("diff --stat: {stderr}")))?;
    let name_only = git_output(worktree_path, &["diff", "--name-only", &range])
        .await
        .map_err(|stderr| {
            WorkspaceRunnerError::PrepareFailed(format!("diff --name-only: {stderr}"))
        })?;
    let patch = git_output(worktree_path, &["diff", "--unified=80", &range])
        .await
        .map_err(|stderr| {
            WorkspaceRunnerError::PrepareFailed(format!("diff --unified=80: {stderr}"))
        })?;
    let (patch, patch_truncated) = truncate_utf8(patch, REVIEW_DIFF_MAX_BYTES);
    Ok(json!({
        "range": range,
        "stat": stat,
        "name_only": name_only
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>(),
        "patch": patch,
        "patch_truncated": patch_truncated,
        "max_patch_bytes": REVIEW_DIFF_MAX_BYTES,
    }))
}

fn truncate_utf8(value: String, max_bytes: usize) -> (String, bool) {
    if value.len() <= max_bytes {
        return (value, false);
    }
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    (value[..end].to_string(), true)
}

fn map_sqlx_internal(err: sqlx::Error) -> WorkspaceRunnerError {
    WorkspaceRunnerError::Internal(err.to_string())
}

fn repo_id_from_payload(payload: &serde_json::Value) -> Result<Uuid, WorkspaceRunnerError> {
    payload
        .get("repo_id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            WorkspaceRunnerError::PrepareFailed("triggering payload has no repo_id".into())
        })
        .and_then(|raw| {
            Uuid::parse_str(raw)
                .map_err(|err| WorkspaceRunnerError::PrepareFailed(format!("repo_id: {err}")))
        })
}

async fn load_repo(
    pool: &PgPool,
    owner: &Owner,
    repo_id: Uuid,
) -> Result<RunnerRepoRow, WorkspaceRunnerError> {
    let (kind, principal_id, org_id) = owner_columns_pub(owner);
    sqlx::query_as::<_, RunnerRepoRow>(
        "SELECT canonical_path, target_branch
         FROM proxima_code.repos
         WHERE owner_principal_kind = $1
           AND owner_principal_id = $2
           AND owner_org_id = $3
           AND repo_id = $4",
    )
    .bind(kind)
    .bind(principal_id)
    .bind(org_id)
    .bind(repo_id)
    .fetch_optional(pool)
    .await
    .map_err(|err| WorkspaceRunnerError::Internal(format!("load repo: {err}")))?
    .ok_or_else(|| WorkspaceRunnerError::PrepareFailed(format!("repo not found: {repo_id}")))
}

async fn git_output(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|err| err.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

async fn diff_stat(
    worktree: &Path,
    parent_sha: &str,
    head_sha: &str,
) -> Result<WorkspaceDiffStat, WorkspaceRunnerError> {
    let range = format!("{parent_sha}..{head_sha}");
    let raw = git_output(worktree, &["diff", "--numstat", &range])
        .await
        .map_err(|stderr| {
            WorkspaceRunnerError::FinalizeFailed(format!("diff numstat: {stderr}"))
        })?;
    let mut files = Vec::new();
    let mut insertions = 0u64;
    let mut deletions = 0u64;
    for line in raw.lines() {
        let mut parts = line.splitn(3, '\t');
        let added = parts.next().unwrap_or("0");
        let deleted = parts.next().unwrap_or("0");
        let path = parts.next().unwrap_or("").to_string();
        let added_n = added.parse::<u64>().unwrap_or(0);
        let deleted_n = deleted.parse::<u64>().unwrap_or(0);
        insertions = insertions.saturating_add(added_n);
        deletions = deletions.saturating_add(deleted_n);
        files.push(WorkspaceDiffFile {
            path,
            insertions: added_n,
            deletions: deleted_n,
        });
    }
    Ok(WorkspaceDiffStat {
        files_changed: u64::try_from(files.len()).unwrap_or(u64::MAX),
        insertions,
        deletions,
        files,
    })
}

#[allow(clippy::too_many_lines)]
async fn ingest_workspace_run(
    pool: &PgPool,
    payload: &WorkspaceRunV1,
    input: WorkspaceFinalizeInput<'_>,
) -> Result<MemoryId, WorkspaceRunnerError> {
    let mut payload_bytes = Vec::new();
    ciborium::ser::into_writer(payload, &mut payload_bytes).map_err(|err| {
        WorkspaceRunnerError::FinalizeFailed(format!("serialize workspace run: {err}"))
    })?;
    let content_hash = blake3::hash(&payload_bytes);
    let observed_at = time::OffsetDateTime::now_utc();
    let draft = EventDraft {
        source_id: SourceId::new(WORKSPACE_RUNNER_SOURCE_ID),
        source_batch_id: SourceBatchId::new(Uuid::now_v7()),
        owner: input.owner.clone(),
        schema_id: SchemaId::new(WorkspaceRunV1::SCHEMA_ID.into()),
        schema_version: SchemaVersion::new(WorkspaceRunV1::SCHEMA_VERSION),
        payload: payload_bytes,
        observed_at,
        occurred_at: observed_at,
        cited_object: CitedObjectHint {
            schema_id: SchemaId::new(WORKSPACE_RUN_OBJECT_SCHEMA.into()),
            schema_version: SchemaVersion::new(1),
            content_hash: *content_hash.as_bytes(),
        },
        citation_mapping: CitationMappingHint {
            schema_id: SchemaId::new(WORKSPACE_RUN_WHOLE_SCHEMA.into()),
            schema_version: SchemaVersion::new(1),
        },
    };

    let mut tx = pool
        .begin()
        .await
        .map_err(|err| WorkspaceRunnerError::Internal(format!("begin workspace tx: {err}")))?;
    let outcome = ingest_event_in_tx(&mut tx, &draft)
        .await
        .map_err(|err| WorkspaceRunnerError::FinalizeFailed(format!("event ingest: {err}")))?;
    if !outcome.idempotent_replay {
        sqlx::query(
            "INSERT INTO proxima_code.workspace_run_v1
                (memory_id, wake_invocation_id, repo_id, target_branch,
                 worktree_path, branch_name, parent_sha, head_sha,
                 diff_stat_json, exit_code, stdout_tail, stderr_tail, duration_ms)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)",
        )
        .bind(outcome.memory_id.into_inner())
        .bind(payload.wake_invocation_id)
        .bind(payload.repo_id)
        .bind(&payload.target_branch)
        .bind(&payload.worktree_path)
        .bind(&payload.branch_name)
        .bind(&payload.parent_sha)
        .bind(&payload.head_sha)
        .bind(
            serde_json::to_value(&payload.diff_stat_json).map_err(|err| {
                WorkspaceRunnerError::FinalizeFailed(format!("serialize diff stat: {err}"))
            })?,
        )
        .bind(payload.exit_code)
        .bind(payload.stdout_tail.as_deref())
        .bind(payload.stderr_tail.as_deref())
        .bind(payload.duration_ms.and_then(|v| i64::try_from(v).ok()))
        .execute(&mut *tx)
        .await
        .map_err(|err| WorkspaceRunnerError::FinalizeFailed(format!("insert sidecar: {err}")))?;

        let authored = EdgeDraft {
            edge_id: Uuid::now_v7(),
            relation: input.authored_relation,
            source_kind: "Perspective",
            source_memory_id: Some(input.root_perspective_memory_id.into_inner()),
            source_goal_id: None,
            target_kind: "Fact",
            target_memory_id: Some(outcome.memory_id.into_inner()),
            target_goal_id: None,
            authorship_kind: "Engine",
            authorship_owner_memory_id: None,
            owner: input.owner,
        };
        append_edge_in_tx(&mut tx, &authored, None)
            .await
            .map_err(|err| {
                WorkspaceRunnerError::FinalizeFailed(format!("append authored edge: {err}"))
            })?;

        let derived = EdgeDraft {
            edge_id: Uuid::now_v7(),
            relation: input.derived_from_relation,
            source_kind: "Fact",
            source_memory_id: Some(outcome.memory_id.into_inner()),
            source_goal_id: None,
            target_kind: "Fact",
            target_memory_id: Some(input.triggering_memory_id.into_inner()),
            target_goal_id: None,
            authorship_kind: "EventSource",
            authorship_owner_memory_id: None,
            owner: input.owner,
        };
        append_edge_in_tx(&mut tx, &derived, None)
            .await
            .map_err(|err| {
                WorkspaceRunnerError::FinalizeFailed(format!("append derived edge: {err}"))
            })?;
    }
    tx.commit()
        .await
        .map_err(|err| WorkspaceRunnerError::Internal(format!("commit workspace tx: {err}")))?;
    Ok(outcome.memory_id)
}

fn owner_component(owner: &Owner) -> String {
    match &owner.principal {
        proxima_core::Principal::User(user) => user.into_inner().to_string(),
        proxima_core::Principal::Group(group) => group.into_inner().to_string(),
    }
}
