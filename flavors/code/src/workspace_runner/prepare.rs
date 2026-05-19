use std::path::{Path, PathBuf};

use proxima_core::{
    FactPayload, MemoryId, WorkspaceFinalizeInput, WorkspacePrepareInput, WorkspacePreparedRun,
    WorkspaceRunRecord, WorkspaceRunner, WorkspaceRunnerError,
};
use serde_json::json;

use crate::payloads::{
    ExecutionRequestV1, WorkspaceDecision, WorkspaceDecisionV1, WorkspaceReviewV1,
    WorkspaceReviewVerdict, WorkspaceRunV1,
};

use super::context::{build_workspace_context, hydrate_workspace_tooling};
use super::git::{
    build_review_diff_context, diff_stat, ensure_worktree_head, git_output, owner_component,
};
use super::ingest::ingest_workspace_run;
use super::loaders::{
    goal_close_candidate, latest_review_verdict_for_request, load_acceptance_criteria_for_request,
    load_continuation_workspace_run_for_request, load_decisions_for_request,
    load_execution_request, load_execution_request_for_run, load_goal_context_for_request,
    load_latest_rejected_review_for_run, load_latest_review_for_run, load_repo,
    load_reviews_for_request, load_target_worker_personality_for_request,
    load_verification_evidence_for_request, load_workspace_run, parse_payload,
    repo_id_from_payload, request_has_direct_workspace_run, request_is_correction_request,
    veto_count_for_request,
};
use super::{CodeWorkspaceRunner, FinalizePolicy, PreparedState};

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
            WorkspaceDecisionV1::SCHEMA_ID => self.prepare_workspace_decision(input).await,
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
        match &state.finalize_policy {
            FinalizePolicy::EmitWorkspaceRun => {}
            FinalizePolicy::InspectOnly {
                head_sha: expected_head,
                status_porcelain: expected_status,
            } => {
                let status = git_output(worktree, &["status", "--porcelain"])
                    .await
                    .map_err(|stderr| {
                        WorkspaceRunnerError::FinalizeFailed(format!(
                            "git status --porcelain: {stderr}"
                        ))
                    })?;
                if &head_sha != expected_head || &status != expected_status {
                    return Err(WorkspaceRunnerError::FinalizeFailed(format!(
                        "workspace_inspect_modified_worktree: expected head/status {expected_head:?}/{expected_status:?}, got {head_sha:?}/{status:?}"
                    )));
                }
                return Ok(WorkspaceRunRecord {
                    primary_memory_id: None,
                });
            }
        }
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
    #[allow(clippy::too_many_lines)]
    async fn prepare_execution_request(
        &self,
        input: WorkspacePrepareInput<'_>,
    ) -> Result<WorkspacePreparedRun, WorkspaceRunnerError> {
        let pool = self.pool()?;
        let repo_id = repo_id_from_payload(input.triggering_memory_payload)?;
        if let Some(verdict) =
            latest_review_verdict_for_request(pool, input.owner, input.triggering_memory_id).await?
        {
            match verdict {
                WorkspaceReviewVerdict::Approved | WorkspaceReviewVerdict::NeedsUser => {
                    return Err(WorkspaceRunnerError::TriggerNotEligible(format!(
                        "execution request already has terminal workspace review: {}",
                        verdict.as_str()
                    )));
                }
                WorkspaceReviewVerdict::Rejected => {}
            }
        }
        if request_has_direct_workspace_run(pool, input.owner, input.triggering_memory_id).await? {
            return Err(WorkspaceRunnerError::TriggerNotEligible(
                "execution request already has a workspace run".into(),
            ));
        }
        if let Some(prior_run) = load_continuation_workspace_run_for_request(
            pool,
            input.owner,
            input.triggering_memory_id,
        )
        .await?
        {
            if !request_is_correction_request(pool, input.owner, input.triggering_memory_id).await?
            {
                return Err(WorkspaceRunnerError::TriggerNotEligible(
                    "execution request already has a derived workspace run".into(),
                ));
            }
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
            let acceptance_criteria =
                load_acceptance_criteria_for_request(pool, input.owner, input.triggering_memory_id)
                    .await?;
            let verification_evidence = load_verification_evidence_for_request(
                pool,
                input.owner,
                input.triggering_memory_id,
            )
            .await?;
            if let Some(object) = workspace_context.as_object_mut() {
                object.insert("mode".into(), json!("continue_execution_request"));
                object.insert("acceptance_criteria".into(), json!(acceptance_criteria));
                object.insert("verification_evidence".into(), json!(verification_evidence));
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
                finalize_policy: FinalizePolicy::EmitWorkspaceRun,
            };
            let runner_state = serde_json::to_value(&state).map_err(|err| {
                WorkspaceRunnerError::Internal(format!("serialize runner state: {err}"))
            })?;
            return Ok(WorkspacePreparedRun {
                work_dir: worktree_path,
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

        let mut workspace_context = build_workspace_context(
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
        let acceptance_criteria =
            load_acceptance_criteria_for_request(pool, input.owner, input.triggering_memory_id)
                .await?;
        let verification_evidence =
            load_verification_evidence_for_request(pool, input.owner, input.triggering_memory_id)
                .await?;
        if let Some(object) = workspace_context.as_object_mut() {
            object.insert("acceptance_criteria".into(), json!(acceptance_criteria));
            object.insert("verification_evidence".into(), json!(verification_evidence));
        }

        let state = PreparedState {
            repo_id,
            canonical_path: repo.canonical_path,
            target_branch,
            branch_name,
            parent_sha,
            worktree_path: worktree_arg,
            finalize_policy: FinalizePolicy::EmitWorkspaceRun,
        };
        let runner_state = serde_json::to_value(&state).map_err(|err| {
            WorkspaceRunnerError::Internal(format!("serialize runner state: {err}"))
        })?;
        Ok(WorkspacePreparedRun {
            work_dir: worktree_path,
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
        let acceptance_criteria =
            load_acceptance_criteria_for_request(pool, input.owner, original_request.memory_id)
                .await?;
        let verification_evidence =
            load_verification_evidence_for_request(pool, input.owner, original_request.memory_id)
                .await?;
        let veto_count =
            veto_count_for_request(pool, input.owner, original_request.memory_id).await?;
        let diff = build_review_diff_context(&worktree_path, &run).await?;
        let diff_range_to_head = format!("{}..HEAD", run.parent_sha);
        let diff_inspection_commands = vec![
            "git status --short".to_string(),
            format!("git diff --stat {diff_range_to_head}"),
            format!("git diff --name-only {diff_range_to_head}"),
            format!("git diff --unified=80 {diff_range_to_head}"),
        ];
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
            "acceptance_criteria": acceptance_criteria,
            "verification_evidence": verification_evidence,
            "diff_stat": run.diff_stat_json,
            "diff": diff,
            "diff_inspection_commands": diff_inspection_commands,
            "log_tails": {
                "stdout_tail": run.stdout_tail,
                "stderr_tail": run.stderr_tail,
                "exit_code": run.exit_code,
                "duration_ms": run.duration_ms,
            },
            "veto_count": veto_count,
            "max_veto_rounds": crate::mcp::MAX_WORKSPACE_VETO_ROUNDS,
        });
        self.prepared_from_existing_run(&run, context).await
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
        self.prepared_from_existing_run(&run, context).await
    }

    async fn prepare_workspace_decision(
        &self,
        input: WorkspacePrepareInput<'_>,
    ) -> Result<WorkspacePreparedRun, WorkspaceRunnerError> {
        let decision = parse_payload::<WorkspaceDecisionV1>(
            input.triggering_memory_payload,
            WorkspaceDecisionV1::SCHEMA_ID,
        )?;
        match decision.decision {
            WorkspaceDecision::RetryRequested => {
                self.prepare_workspace_retry_decision(input, decision).await
            }
            WorkspaceDecision::Merged => {
                self.prepare_workspace_merge_goal_close(input, decision)
                    .await
            }
            WorkspaceDecision::Rejected | WorkspaceDecision::Accepted => {
                Err(WorkspaceRunnerError::TriggerNotEligible(format!(
                    "workspace-decision variant {:?} has no workspace prep",
                    decision.decision
                )))
            }
        }
    }

    async fn prepare_workspace_retry_decision(
        &self,
        input: WorkspacePrepareInput<'_>,
        decision: WorkspaceDecisionV1,
    ) -> Result<WorkspacePreparedRun, WorkspaceRunnerError> {
        let pool = self.pool()?;
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
        self.prepared_from_existing_run(&run, context).await
    }

    async fn prepare_workspace_merge_goal_close(
        &self,
        input: WorkspacePrepareInput<'_>,
        decision: WorkspaceDecisionV1,
    ) -> Result<WorkspacePreparedRun, WorkspaceRunnerError> {
        let pool = self.pool()?;
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
        let active_goal =
            load_goal_context_for_request(pool, input.owner, original_request.memory_id).await?;
        let latest_review = load_latest_review_for_run(
            pool,
            input.owner,
            MemoryId::new(decision.workspace_run_memory_id),
        )
        .await?;
        let prior_reviews =
            load_reviews_for_request(pool, input.owner, original_request.memory_id).await?;
        let prior_decisions =
            load_decisions_for_request(pool, input.owner, original_request.memory_id).await?;
        let close_candidate = goal_close_candidate(
            active_goal.as_ref(),
            latest_review.as_ref(),
            input.triggering_memory_id,
        );
        let context = json!({
            "mode": "close_goal_after_merge",
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
            "merged_decision": decision,
            "latest_review": latest_review,
            "prior_reviews": prior_reviews,
            "prior_decisions": prior_decisions,
            "active_goal": active_goal,
            "goal_close": close_candidate,
        });
        self.prepared_from_existing_run(&run, context).await
    }

    async fn prepared_from_existing_run(
        &self,
        run: &WorkspaceRunV1,
        workspace_context: serde_json::Value,
    ) -> Result<WorkspacePreparedRun, WorkspaceRunnerError> {
        let worktree_path = PathBuf::from(&run.worktree_path);
        ensure_worktree_head(&worktree_path, &run.branch_name, &run.head_sha).await?;
        let head_sha = git_output(&worktree_path, &["rev-parse", "HEAD"])
            .await
            .map_err(|stderr| {
                WorkspaceRunnerError::PrepareFailed(format!("rev-parse HEAD: {stderr}"))
            })?;
        let status_porcelain = git_output(&worktree_path, &["status", "--porcelain"])
            .await
            .map_err(|stderr| {
                WorkspaceRunnerError::PrepareFailed(format!("git status --porcelain: {stderr}"))
            })?;
        let state = PreparedState {
            repo_id: run.repo_id,
            canonical_path: String::new(),
            target_branch: run.target_branch.clone(),
            branch_name: run.branch_name.clone(),
            parent_sha: run.parent_sha.clone(),
            worktree_path: run.worktree_path.clone(),
            finalize_policy: FinalizePolicy::InspectOnly {
                head_sha,
                status_porcelain,
            },
        };
        let runner_state = serde_json::to_value(&state).map_err(|err| {
            WorkspaceRunnerError::Internal(format!("serialize runner state: {err}"))
        })?;
        Ok(WorkspacePreparedRun {
            work_dir: worktree_path,
            workspace_context: Some(workspace_context),
            runner_state,
        })
    }
}
