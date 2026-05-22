//! Per-entry wake fire path backed by the in-process harness.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::process::Command;
use uuid::Uuid;

use crate::engine::Engine;
use crate::error::ProtocolError;
use crate::harness::{
    HarnessAdapter, HarnessContext, HarnessProgram, HarnessToolProjection,
    build_wake_tool_projection,
};
use crate::mcp::{HandleTable, MemoryHandleClass};
use crate::personality::workspace::{
    WorkspaceFinalizeInput, WorkspaceOutcome, WorkspacePrepareInput, WorkspacePreparedRun,
    WorkspaceRunnerError,
};
use crate::personality::{
    PersonalityInstanceId, WakeChainDepth, WakeEntryExecutionMode, WakeInvocationContinuation,
    WakeInvocationLogStatus, WakeInvocationStart, WakeInvocationStatus, WakeWorkspaceBinding,
    WakeWorkspaceFinalize,
};
use crate::verbs::persist_wake_trace::WakeTracePersistOutcome;
use crate::verbs::query::{QueryRequest, SupersessionStatus};
use crate::wake::context::{WakeContext, assemble_wake_context};
use crate::wake::contract::build_wake_contract;
use crate::wake::token_store::WakeTokenContext;
use crate::wake::trace::emit::{
    ProviderTargetBuildError, TraceTiming, emit_trace_from_failed_preflight,
    emit_trace_from_outcome, provider_target_from_config,
};
use crate::workspace_run::{
    CORE_WORKSPACE_RUN_SOURCE_ID, CoreWorkspaceDiffFile, CoreWorkspaceDiffStat,
    CoreWorkspaceRunPersistInput, CoreWorkspaceRunV1,
};
use crate::{
    GoalId, InterventionRequestPersistInput, InterventionRequestedV1, MemoryId, Owner,
    SourceBatchId, SourceId, chat,
};

use super::finalize::{
    append_session_artifact_log, append_session_log_error_if_present, finalize,
    wake_session_log_path,
};
use super::input::{FireWakeEntryInput, per_invocation_timeout};
use super::outcome::{WakeInvocationFinalizeOutcome, wake_outcome_from_harness_outcome};
use super::resolve::{ResolvedTarget, collect_sidecars, resolve_target};

const ACTIVE_PERSPECTIVE_TEXT_LIMIT: usize = 2_000;

pub async fn fire_wake_entry(
    engine: &Engine,
    adapter: &dyn HarnessAdapter,
    input: FireWakeEntryInput,
) -> Result<bool, ProtocolError> {
    let change_event = engine
        .storage()
        .fetch_change_event_for_wake(&input.owner, input.change_event_seq)
        .await
        .map_err(|e| ProtocolError::internal(format!("fetch_change_event_for_wake: {e}")))?
        .ok_or_else(|| {
            ProtocolError::not_found(format!(
                "change event not found: seq={}",
                input.change_event_seq
            ))
        })?;
    if change_event
        .event
        .authoring_personality_instance_id
        .map(PersonalityInstanceId::new)
        == Some(input.personality_instance_id)
    {
        return Ok(false);
    }

    let sidecars = collect_sidecars(engine);
    let wake_context_seq = wake_context_change_event_seq(&input);
    let wake_context = assemble_wake_context(
        engine.storage().as_ref(),
        &input.owner,
        input.personality_instance_id,
        wake_context_seq,
        &sidecars,
    )
    .await?;
    let resolved = resolve_target(engine, &input).await?;

    let invocation_id_for_dispatch = Uuid::now_v7();
    let max_rounds = u32::from(input.wake_entry.max_rounds);
    let invocation_timeout = per_invocation_timeout(max_rounds);
    let (wake_token, seeded_handles, handle_table) = mint_wake_token(
        engine,
        &input,
        &wake_context,
        &resolved,
        invocation_id_for_dispatch,
        invocation_timeout,
    )
    .await;

    let inserted = engine
        .storage()
        .start_wake_invocation(&WakeInvocationStart {
            invocation_id: invocation_id_for_dispatch,
            owner: input.owner.clone(),
            personality_instance_id: input.personality_instance_id,
            wake_entry_id: input.wake_entry.wake_entry_id,
            change_event_seq: input.change_event_seq,
            wake_token,
            resolved_inference_target_ref: resolved.target_ref.clone(),
            continuation: input.continuation.as_ref().map(|continuation| {
                WakeInvocationContinuation {
                    intervention_decision_memory_id: continuation
                        .intervention_decision_memory_id
                        .into_inner(),
                    original_invocation_id: continuation.original_invocation_id,
                }
            }),
        })
        .await
        .map_err(|e| ProtocolError::internal(format!("start_wake_invocation: {e}")))?;
    if !inserted {
        engine.wake_token_store().revoke(wake_token).await;
        return Ok(false);
    }

    let session_log_path = wake_session_log_path(&input.owner, invocation_id_for_dispatch);
    append_session_artifact_log(
        engine,
        &input,
        invocation_id_for_dispatch,
        WakeInvocationLogStatus::Started,
        session_log_path.display().to_string(),
    )
    .await;

    let started_at = time::OffsetDateTime::now_utc();
    let tool_projection = match build_wake_tool_projection(
        engine.registry(),
        &input.wake_entry.substrate_tool_palette,
    ) {
        Ok(projection) => projection,
        Err(err) => {
            let timing = TraceTiming {
                started_at,
                finished_at: time::OffsetDateTime::now_utc(),
            };
            finalize_failed_started_wake(
                engine,
                &input,
                &wake_context,
                &resolved,
                StartedWakeFailure {
                    invocation_id: invocation_id_for_dispatch,
                    wake_token,
                    timing,
                    failure_reason: format!("tool_projection:{err}"),
                },
            )
            .await?;
            return Ok(true);
        }
    };
    let context_params = match build_context_params(
        engine,
        &input,
        &wake_context,
        &seeded_handles,
        &handle_table,
        &tool_projection,
    )
    .await
    {
        Ok(params) => params,
        Err(err) => {
            let timing = TraceTiming {
                started_at,
                finished_at: time::OffsetDateTime::now_utc(),
            };
            finalize_failed_started_wake(
                engine,
                &input,
                &wake_context,
                &resolved,
                StartedWakeFailure {
                    invocation_id: invocation_id_for_dispatch,
                    wake_token,
                    timing,
                    failure_reason: format!("context_param_serialization:{err}"),
                },
            )
            .await?;
            return Ok(true);
        }
    };

    if matches!(
        input.wake_entry.execution_mode,
        WakeEntryExecutionMode::Workspace
    ) {
        return handle_workspace_mode(
            engine,
            adapter,
            WorkspaceModeState {
                input,
                wake_token,
                seeded_handles,
                handles: handle_table,
                wake_context,
                resolved,
                context_params,
                tool_projection,
                session_log_path,
                invocation_id_for_dispatch,
                invocation_timeout,
                started_at,
            },
        )
        .await;
    }

    let provider_target = match provider_target_from_config(&resolved.config) {
        Ok(target) => target,
        Err(err) => {
            let timing = TraceTiming {
                started_at,
                finished_at: time::OffsetDateTime::now_utc(),
            };
            finalize_failed_started_wake(
                engine,
                &input,
                &wake_context,
                &resolved,
                StartedWakeFailure {
                    invocation_id: invocation_id_for_dispatch,
                    wake_token,
                    timing,
                    failure_reason: provider_target_failure_reason(&err),
                },
            )
            .await?;
            return Ok(true);
        }
    };
    let program = HarnessProgram {
        system_prompt: build_system_prompt(
            &wake_context,
            &seeded_handles,
            input.continuation.as_ref(),
        ),
        instructions: input.wake_entry.instructions.clone(),
        context_params,
        tool_projection,
        required_fulfillment_schema_ids: input.wake_entry.required_produced_schema_ids.clone(),
        substrate_tool_palette: input.wake_entry.substrate_tool_palette.clone(),
        workspace_root: None,
        workspace_tool_palette: Vec::new(),
        max_rounds,
        provider: provider_target,
    };
    let hctx = harness_context(
        &input,
        &wake_context,
        invocation_id_for_dispatch,
        wake_token,
        invocation_timeout,
    );

    let outcome_result = adapter.run(program, hctx).await;
    let timing = TraceTiming {
        started_at,
        finished_at: time::OffsetDateTime::now_utc(),
    };

    engine.wake_token_store().revoke(wake_token).await;
    write_session_jsonl_to_disk(&session_log_path, &outcome_result).await;
    append_session_log_error_if_present(
        engine,
        &input,
        invocation_id_for_dispatch,
        &outcome_result,
    )
    .await;
    let trace_outcome = emit_trace_from_outcome(
        engine,
        &input,
        &wake_context,
        &resolved,
        invocation_id_for_dispatch,
        &outcome_result,
        timing,
    )
    .await
    .ok();

    let outcome = wake_outcome_from_harness_outcome(&input, outcome_result);
    warn_if_failed(&input, &outcome);
    maybe_emit_intervention_request(
        engine,
        &input,
        &wake_context,
        invocation_id_for_dispatch,
        trace_outcome.as_ref(),
        &outcome,
    )
    .await;
    finalize(engine, &input, invocation_id_for_dispatch, outcome).await?;
    Ok(true)
}

struct WorkspaceModeState {
    input: FireWakeEntryInput,
    wake_token: Uuid,
    seeded_handles: crate::mcp::PreSeededHandles,
    handles: Arc<HandleTable>,
    wake_context: WakeContext,
    resolved: ResolvedTarget,
    context_params: HashMap<String, serde_json::Value>,
    tool_projection: Vec<HarnessToolProjection>,
    session_log_path: std::path::PathBuf,
    invocation_id_for_dispatch: Uuid,
    invocation_timeout: Duration,
    started_at: time::OffsetDateTime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum CoreWorkspaceRunnerState {
    /// A per-wake full `git clone` of the repo. Unlike a linked worktree,
    /// a clone has a real `.git` directory, so `git` functions inside a
    /// container bind-mounting only the clone. Changes return to the real
    /// repo as a fetched branch; the real working tree is never touched.
    GitClone {
        /// Canonical path of the real repository.
        repo_path: String,
        /// The disposable per-wake clone the harness runs against.
        staging_dir: String,
        /// `proxima/wake/<invocation_id>` — the branch changes land on.
        wake_branch: String,
        /// The ref the wake was based on (e.g. `HEAD`).
        base_ref: String,
        /// The resolved commit `base_ref` pointed at when the wake started.
        base_sha: String,
        finalize: WakeWorkspaceFinalize,
    },
}

async fn handle_workspace_mode(
    engine: &Engine,
    adapter: &dyn HarnessAdapter,
    state: WorkspaceModeState,
) -> Result<bool, ProtocolError> {
    let WorkspaceModeState {
        input,
        wake_token,
        seeded_handles,
        handles,
        wake_context,
        resolved,
        mut context_params,
        tool_projection,
        session_log_path,
        invocation_id_for_dispatch,
        invocation_timeout,
        started_at,
    } = state;

    let mcp_url = engine.mcp_url().unwrap_or_default();
    let prepare_input = WorkspacePrepareInput {
        invocation_id: invocation_id_for_dispatch,
        owner: &input.owner,
        wake_token,
        mcp_url: &mcp_url,
        root_perspective_memory_id: MemoryId::new(wake_context.root_perspective.memory_id),
        triggering_memory_id: MemoryId::new(wake_context.triggering_memory.memory_id),
        triggering_memory_schema_id: input.wake_entry.trigger_id.as_str(),
        triggering_memory_payload: &wake_context.triggering_memory.typed_payload,
        is_continuation: input.continuation.is_some(),
        workspace_tool_palette: &input.wake_entry.workspace_tool_palette,
    };
    let prepared_result = match input.wake_entry.workspace_binding.as_ref() {
        Some(binding) => prepare_workspace_binding(engine, prepare_input, binding).await,
        None => Err(WorkspaceRunnerError::TriggerNotEligible(
            "workspace_binding_required".into(),
        )),
    };
    let prepared = match prepared_result {
        Ok(prepared) => prepared,
        Err(WorkspaceRunnerError::Unimplemented) => {
            engine.wake_token_store().revoke(wake_token).await;
            finalize(
                engine,
                &input,
                invocation_id_for_dispatch,
                WakeInvocationFinalizeOutcome::failed(
                    "workspace_mode_not_yet_implemented".to_string(),
                ),
            )
            .await?;
            return Ok(true);
        }
        Err(err) => {
            engine.wake_token_store().revoke(wake_token).await;
            finalize(
                engine,
                &input,
                invocation_id_for_dispatch,
                WakeInvocationFinalizeOutcome::failed(format!("workspace_runner_prepare:{err}")),
            )
            .await?;
            return Ok(true);
        }
    };

    if let Some(ws_ctx) = prepared.workspace_context.clone() {
        let workspace_memory_classes =
            load_payload_memory_classes(engine, &input.owner, &ws_ctx).await?;
        context_params.insert(
            "workspace_context".to_string(),
            project_model_value(&ws_ctx, None, handles.as_ref(), &workspace_memory_classes),
        );
    }

    let provider_target = match provider_target_from_config(&resolved.config) {
        Ok(target) => target,
        Err(err) => {
            let timing = TraceTiming {
                started_at,
                finished_at: time::OffsetDateTime::now_utc(),
            };
            let failure_reason = provider_target_failure_reason(&err);
            let finalize_outcome = WakeInvocationFinalizeOutcome::failed(failure_reason);
            finalize_pre_run_workspace(
                engine,
                &input,
                &wake_context,
                &resolved,
                PreRunWorkspaceFailure {
                    invocation_id: invocation_id_for_dispatch,
                    wake_token,
                    prepared,
                    timing,
                    finalize_outcome,
                },
            )
            .await?;
            return Ok(true);
        }
    };
    let program = HarnessProgram {
        system_prompt: build_system_prompt(
            &wake_context,
            &seeded_handles,
            input.continuation.as_ref(),
        ),
        instructions: input.wake_entry.instructions.clone(),
        context_params,
        tool_projection,
        required_fulfillment_schema_ids: input.wake_entry.required_produced_schema_ids.clone(),
        substrate_tool_palette: input.wake_entry.substrate_tool_palette.clone(),
        workspace_root: Some(prepared.work_dir.clone()),
        workspace_tool_palette: input.wake_entry.workspace_tool_palette.clone(),
        max_rounds: u32::from(input.wake_entry.max_rounds),
        provider: provider_target,
    };
    let hctx = harness_context(
        &input,
        &wake_context,
        invocation_id_for_dispatch,
        wake_token,
        invocation_timeout,
    );
    let outcome_result = adapter.run(program, hctx).await;
    let timing = TraceTiming {
        started_at,
        finished_at: time::OffsetDateTime::now_utc(),
    };

    write_session_jsonl_to_disk(&session_log_path, &outcome_result).await;
    append_session_log_error_if_present(
        engine,
        &input,
        invocation_id_for_dispatch,
        &outcome_result,
    )
    .await;
    let trace_outcome = emit_trace_from_outcome(
        engine,
        &input,
        &wake_context,
        &resolved,
        invocation_id_for_dispatch,
        &outcome_result,
        timing,
    )
    .await
    .ok();

    let finalize_outcome = wake_outcome_from_harness_outcome(&input, outcome_result);
    let workspace_outcome = WorkspaceOutcome {
        exit_code: finalize_outcome.exit_code,
        stdout_tail: finalize_outcome.stdout_tail.clone(),
        stderr_tail: finalize_outcome.stderr_tail.clone(),
        duration_ms: finalize_outcome.duration_ms,
    };
    let finalized = finalize_workspace_runner(
        engine,
        &input,
        &wake_context,
        invocation_id_for_dispatch,
        prepared,
        workspace_outcome,
    )
    .await;

    engine.wake_token_store().revoke(wake_token).await;

    let outcome = match finalized {
        Ok(()) => finalize_outcome,
        Err(err) => {
            WakeInvocationFinalizeOutcome::failed(format!("workspace_runner_finalize:{err}"))
        }
    };
    maybe_emit_intervention_request(
        engine,
        &input,
        &wake_context,
        invocation_id_for_dispatch,
        trace_outcome.as_ref(),
        &outcome,
    )
    .await;
    finalize(engine, &input, invocation_id_for_dispatch, outcome).await?;
    Ok(true)
}

fn wake_context_change_event_seq(input: &FireWakeEntryInput) -> Uuid {
    input
        .continuation
        .as_ref()
        .map_or(input.change_event_seq, |continuation| {
            continuation.original_change_event_seq
        })
}

async fn prepare_registered_workspace_runner(
    engine: &Engine,
    prepare_input: WorkspacePrepareInput<'_>,
    flavor_id: &str,
) -> Result<WorkspacePreparedRun, WorkspaceRunnerError> {
    let runner = engine
        .registry()
        .workspace_runner(flavor_id)
        .ok_or_else(|| {
            WorkspaceRunnerError::TriggerNotEligible(format!(
                "workspace_no_runner_for_flavor:{flavor_id}"
            ))
        })?;

    if !engine
        .registry()
        .is_workspace_trigger(prepare_input.triggering_memory_schema_id)
    {
        return Err(WorkspaceRunnerError::TriggerNotEligible(format!(
            "workspace_trigger_not_eligible:{}",
            prepare_input.triggering_memory_schema_id
        )));
    }

    runner.prepare(prepare_input).await
}

async fn prepare_workspace_binding(
    engine: &Engine,
    input: WorkspacePrepareInput<'_>,
    binding: &WakeWorkspaceBinding,
) -> Result<WorkspacePreparedRun, WorkspaceRunnerError> {
    match binding {
        WakeWorkspaceBinding::GitWorktree {
            repo_path,
            base_ref,
            finalize,
            worktrees_root,
        } => prepare_core_git_clone(input, repo_path, base_ref, *finalize, worktrees_root).await,
        WakeWorkspaceBinding::RegisteredRunner { flavor_id } => {
            prepare_registered_workspace_runner(engine, input, flavor_id).await
        }
    }
}

/// Prepare a disposable per-wake **full clone** of the repo.
///
/// A linked git worktree shares its parent's object store and its `.git`
/// is only a pointer file — `git` does not work inside a container that
/// bind-mounts the worktree alone. A `git clone --local` produces a real
/// `.git` directory (hardlinked objects, near-instant, no network), so the
/// clone is self-contained and container-portable.
async fn prepare_core_git_clone(
    input: WorkspacePrepareInput<'_>,
    repo_path: &str,
    base_ref: &str,
    finalize: WakeWorkspaceFinalize,
    clones_root: &Option<String>,
) -> Result<WorkspacePreparedRun, WorkspaceRunnerError> {
    let repo = std::fs::canonicalize(repo_path).map_err(|err| {
        WorkspaceRunnerError::PrepareFailed(format!("canonicalize repo_path {repo_path}: {err}"))
    })?;
    let base_sha = git_output(&repo, &["rev-parse", base_ref])
        .await
        .map_err(|stderr| {
            WorkspaceRunnerError::PrepareFailed(format!("rev-parse {base_ref}: {stderr}"))
        })?;
    let root = clones_root
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(default_core_wake_clones_root);
    let staging_path = root.join(input.invocation_id.to_string());
    let wake_branch = format!("proxima/wake/{}", input.invocation_id);
    clone_repo_to_staging(&repo, &staging_path, &wake_branch, &base_sha)
        .await
        .map_err(WorkspaceRunnerError::PrepareFailed)?;
    let staging_arg = staging_path.to_string_lossy().to_string();

    let state = CoreWorkspaceRunnerState::GitClone {
        repo_path: repo.to_string_lossy().to_string(),
        staging_dir: staging_arg.clone(),
        wake_branch: wake_branch.clone(),
        base_ref: base_ref.to_string(),
        base_sha: base_sha.clone(),
        finalize,
    };
    let workspace_context = json!({
        "mode": "core_git_clone",
        "repo_path": repo.to_string_lossy(),
        "repo_handle": repo.to_string_lossy(),
        "staging_dir": staging_arg,
        "wake_branch": wake_branch,
        "base_ref": base_ref,
        "base_sha": base_sha,
        "finalize": finalize.as_str(),
        "triggering_memory_schema_id": input.triggering_memory_schema_id,
        "triggering_memory_id": input.triggering_memory_id.into_inner().to_string(),
        "is_continuation": input.is_continuation,
    });
    let runner_state = serde_json::to_value(state).map_err(|err| {
        WorkspaceRunnerError::Internal(format!("serialize core workspace state: {err}"))
    })?;
    Ok(WorkspacePreparedRun {
        work_dir: staging_path,
        workspace_context: Some(workspace_context),
        runner_state,
    })
}

/// `git clone --local` the repo into a fresh `staging_path`, then position
/// it on `wake_branch` at exactly `base_sha`.
async fn clone_repo_to_staging(
    repo: &Path,
    staging_path: &Path,
    wake_branch: &str,
    base_sha: &str,
) -> Result<(), String> {
    if let Some(parent) = staging_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("create staging parent: {err}"))?;
    }
    // `git clone` requires the target not to exist; clear any stale dir
    // (e.g. a clone left behind by a crashed prior run).
    if staging_path.exists() {
        std::fs::remove_dir_all(staging_path)
            .map_err(|err| format!("clear stale staging dir: {err}"))?;
    }
    let repo_arg = repo.to_string_lossy().to_string();
    let staging_arg = staging_path.to_string_lossy().to_string();
    git_output(repo, &["clone", "--local", &repo_arg, &staging_arg])
        .await
        .map_err(|stderr| format!("git clone --local: {stderr}"))?;
    git_output(staging_path, &["checkout", "-B", wake_branch, base_sha])
        .await
        .map_err(|stderr| format!("git checkout -B {wake_branch}: {stderr}"))?;
    Ok(())
}

fn default_core_wake_clones_root() -> PathBuf {
    std::env::var_os("HOME")
        .map_or_else(std::env::temp_dir, PathBuf::from)
        .join(".proxima")
        .join("wake-clones")
        .join("core")
}

async fn mint_wake_token(
    engine: &Engine,
    input: &FireWakeEntryInput,
    wake_context: &WakeContext,
    resolved: &ResolvedTarget,
    invocation_id: Uuid,
    invocation_timeout: Duration,
) -> (Uuid, crate::mcp::PreSeededHandles, Arc<HandleTable>) {
    let token_ctx = WakeTokenContext {
        invocation_id,
        personality_instance_id: input.personality_instance_id.into_inner(),
        wake_entry_id: input.wake_entry.wake_entry_id,
        change_event_seq: input.change_event_seq,
        owner: input.owner.clone(),
        palette: input.wake_entry.substrate_tool_palette.clone(),
        model_id: resolved.config_model_id.clone().unwrap_or_default(),
        max_rounds: u32::from(input.wake_entry.max_rounds),
        current_root_perspective_memory_id: MemoryId::new(wake_context.root_perspective.memory_id),
        current_root_perspective_memory_class: MemoryHandleClass::Perspective,
        triggering_event_memory_id: MemoryId::new(wake_context.triggering_memory.memory_id),
        triggering_event_memory_class: MemoryHandleClass::from_memory_kind(
            &wake_context.triggering_memory.kind,
        )
        .unwrap_or(MemoryHandleClass::Fact),
        triggering_event_depth: WakeChainDepth::new(
            u16::try_from(wake_context.trigger_event.wake_chain_depth).unwrap_or(0),
        ),
        read_log: std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new())),
        handles: std::sync::Arc::new(crate::mcp::HandleTable::new()),
    };
    // Force pre-seed so handle counter state is deterministic before
    // any other code touches the table. Capture the seeded struct so
    // the wake bootstrap can render the round-1 system-prompt preamble
    // from the assigned handle strings.
    let mut seeded = crate::wake::handles::pre_seed_wake_handles(&token_ctx);
    if let Some(continuation) = input.continuation.as_ref() {
        seeded.continuation_decision = Some(
            token_ctx
                .handles
                .assign_fact_memory(continuation.intervention_decision_memory_id),
        );
        seeded.continuation_request = Some(
            token_ctx
                .handles
                .assign_fact_memory(continuation.intervention_request_memory_id),
        );
        seeded.continuation_wake_trace = Some(
            token_ctx
                .handles
                .assign_fact_memory(continuation.wake_trace_memory_id),
        );
        seeded.continuation_original_triggering = Some(token_ctx.handles.assign_memory_with_class(
            continuation.original_triggering_memory_id,
            token_ctx.triggering_event_memory_class,
        ));
    }
    let handles = token_ctx.handles.clone();
    let wake_token = engine
        .wake_token_store()
        .mint_with_max_lifetime(token_ctx, invocation_timeout)
        .await;
    (wake_token, seeded, handles)
}

fn harness_context(
    input: &FireWakeEntryInput,
    wake_context: &WakeContext,
    invocation_id: Uuid,
    wake_token: Uuid,
    invocation_timeout: Duration,
) -> HarnessContext {
    HarnessContext {
        owner: input.owner.clone(),
        invocation_id,
        wake_entry_id: input.wake_entry.wake_entry_id,
        personality_instance_id: input.personality_instance_id,
        change_event_seq: input.change_event_seq,
        root_perspective_memory_id: MemoryId::new(wake_context.root_perspective.memory_id),
        wake_token,
        invocation_timeout,
    }
}

struct StartedWakeFailure {
    invocation_id: Uuid,
    wake_token: Uuid,
    timing: TraceTiming,
    failure_reason: String,
}

struct PreRunWorkspaceFailure {
    invocation_id: Uuid,
    wake_token: Uuid,
    prepared: WorkspacePreparedRun,
    timing: TraceTiming,
    finalize_outcome: WakeInvocationFinalizeOutcome,
}

async fn finalize_failed_started_wake(
    engine: &Engine,
    input: &FireWakeEntryInput,
    wake_context: &WakeContext,
    resolved: &ResolvedTarget,
    failure: StartedWakeFailure,
) -> Result<(), ProtocolError> {
    emit_trace_from_failed_preflight(
        engine,
        input,
        wake_context,
        resolved,
        failure.invocation_id,
        failure.timing,
        failure.failure_reason.clone(),
    )
    .await
    .ok();

    engine.wake_token_store().revoke(failure.wake_token).await;
    finalize(
        engine,
        input,
        failure.invocation_id,
        WakeInvocationFinalizeOutcome::failed(failure.failure_reason),
    )
    .await
}

async fn finalize_pre_run_workspace(
    engine: &Engine,
    input: &FireWakeEntryInput,
    wake_context: &WakeContext,
    resolved: &ResolvedTarget,
    failure: PreRunWorkspaceFailure,
) -> Result<(), ProtocolError> {
    emit_trace_from_failed_preflight(
        engine,
        input,
        wake_context,
        resolved,
        failure.invocation_id,
        failure.timing,
        failure
            .finalize_outcome
            .failure_reason
            .clone()
            .unwrap_or_else(|| "pre_run_failure".to_string()),
    )
    .await
    .ok();
    let workspace_outcome = WorkspaceOutcome {
        exit_code: failure.finalize_outcome.exit_code,
        stdout_tail: failure.finalize_outcome.stdout_tail.clone(),
        stderr_tail: failure.finalize_outcome.stderr_tail.clone(),
        duration_ms: failure.finalize_outcome.duration_ms,
    };
    let _ = finalize_workspace_runner(
        engine,
        input,
        wake_context,
        failure.invocation_id,
        failure.prepared,
        workspace_outcome,
    )
    .await;
    engine.wake_token_store().revoke(failure.wake_token).await;
    finalize(
        engine,
        input,
        failure.invocation_id,
        failure.finalize_outcome,
    )
    .await
}

async fn finalize_workspace_runner(
    engine: &Engine,
    input: &FireWakeEntryInput,
    wake_context: &WakeContext,
    invocation_id: Uuid,
    prepared: WorkspacePreparedRun,
    outcome: WorkspaceOutcome,
) -> Result<(), String> {
    let Some(binding) = input.wake_entry.workspace_binding.as_ref() else {
        return Err("workspace_binding_required".into());
    };
    if matches!(binding, WakeWorkspaceBinding::GitWorktree { .. }) {
        return finalize_core_workspace_binding(
            engine,
            input,
            wake_context,
            invocation_id,
            prepared,
            outcome,
        )
        .await;
    }
    let WakeWorkspaceBinding::RegisteredRunner { flavor_id } = binding else {
        unreachable!("all core workspace bindings handled above")
    };
    let runner = engine
        .registry()
        .workspace_runner(flavor_id)
        .ok_or_else(|| format!("workspace_no_runner_for_flavor:{flavor_id}"))?;
    let authored_relation = engine
        .registry()
        .resolve_relation(crate::CORE_AUTHORED_RELATION)
        .ok_or_else(|| "missing core/authored relation".to_string())?;
    let derived_from_relation = engine
        .registry()
        .resolve_relation(crate::CORE_DERIVED_FROM_RELATION)
        .ok_or_else(|| "missing core/derived-from relation".to_string())?;
    runner
        .finalize(WorkspaceFinalizeInput {
            owner: &input.owner,
            invocation_id,
            wake_entry_id: input.wake_entry.wake_entry_id,
            personality_instance_id: input.personality_instance_id,
            root_perspective_memory_id: MemoryId::new(wake_context.root_perspective.memory_id),
            triggering_memory_id: MemoryId::new(wake_context.triggering_memory.memory_id),
            authored_relation,
            derived_from_relation,
            prepared,
            outcome,
        })
        .await
        .map(|_| ())
        .map_err(|err| err.to_string())
}

#[derive(Debug, Clone)]
struct CoreGitWorkspaceFinalization {
    head_sha: String,
    committed: bool,
    diff_stat: CoreWorkspaceDiffStat,
}

async fn finalize_core_workspace_binding(
    engine: &Engine,
    input: &FireWakeEntryInput,
    wake_context: &WakeContext,
    invocation_id: Uuid,
    prepared: WorkspacePreparedRun,
    outcome: WorkspaceOutcome,
) -> Result<(), String> {
    let state: CoreWorkspaceRunnerState = serde_json::from_value(prepared.runner_state)
        .map_err(|err| format!("decode core workspace state: {err}"))?;
    match state {
        CoreWorkspaceRunnerState::GitClone {
            repo_path,
            staging_dir,
            wake_branch,
            base_ref,
            base_sha,
            finalize,
        } => {
            let finalization = finalize_core_git_clone(
                &PathBuf::from(&staging_dir),
                &PathBuf::from(&repo_path),
                &wake_branch,
                &base_sha,
                finalize,
            )
            .await?;
            let run = CoreWorkspaceRunV1 {
                wake_invocation_id: invocation_id,
                wake_entry_id: input.wake_entry.wake_entry_id,
                personality_instance_id: input.personality_instance_id.into_inner(),
                binding_kind: "git_clone".to_string(),
                finalize: finalize.as_str().to_string(),
                repo_path,
                base_ref,
                // `CoreWorkspaceRunV1.worktree_path` / `parent_sha` field
                // names are retained for now — Phase D renames the columns.
                worktree_path: staging_dir,
                branch_name: wake_branch,
                parent_sha: base_sha,
                head_sha: finalization.head_sha,
                committed: finalization.committed,
                diff_stat_json: finalization.diff_stat,
                exit_code: outcome.exit_code,
                stdout_tail: outcome.stdout_tail,
                stderr_tail: outcome.stderr_tail,
                duration_ms: outcome.duration_ms,
            };
            let observed_at = time::OffsetDateTime::now_utc();
            engine
                .persist_core_workspace_run_internal(CoreWorkspaceRunPersistInput {
                    owner: input.owner.clone(),
                    root_perspective_memory_id: MemoryId::new(
                        wake_context.root_perspective.memory_id,
                    ),
                    triggering_memory_id: MemoryId::new(wake_context.triggering_memory.memory_id),
                    triggering_memory_kind: workspace_triggering_memory_kind(
                        &wake_context.triggering_memory.kind,
                    )?,
                    run,
                    source_batch_id: SourceBatchId::new(Uuid::now_v7()),
                    source_id: SourceId::new(CORE_WORKSPACE_RUN_SOURCE_ID.to_string()),
                    observed_at,
                })
                .await
                .map_err(|err| format!("persist core workspace run: {err}"))?;
            Ok(())
        }
    }
}

fn workspace_triggering_memory_kind(kind: &str) -> Result<crate::EntityKind, String> {
    match kind {
        "Fact" => Ok(crate::EntityKind::Fact),
        "Abstraction" => Ok(crate::EntityKind::Abstraction),
        "Perspective" => Ok(crate::EntityKind::Perspective),
        other => Err(format!(
            "unsupported workspace triggering memory kind: {other}"
        )),
    }
}

/// Finalize a per-wake clone: commit its changes onto the wake branch,
/// fetch that branch back into the real repo, then discard the clone.
///
/// `git fetch` moves commits, not uncommitted state, so a workspace wake
/// always commits. The `finalize` mode no longer decides *whether* to
/// commit — only how the commit is labelled: `LeaveDirty` marks it WIP.
async fn finalize_core_git_clone(
    staging: &Path,
    repo: &Path,
    wake_branch: &str,
    base_sha: &str,
    finalize: WakeWorkspaceFinalize,
) -> Result<CoreGitWorkspaceFinalization, String> {
    // 1. Commit any working-tree changes in the staging clone.
    let status = git_output(staging, &["status", "--porcelain"])
        .await
        .map_err(|stderr| format!("git status --porcelain: {stderr}"))?;
    let committed = if status.trim().is_empty() {
        false
    } else {
        git_output(staging, &["add", "-A"])
            .await
            .map_err(|stderr| format!("git add -A: {stderr}"))?;
        let message = match finalize {
            WakeWorkspaceFinalize::CommitAll => {
                "chore(proxima): record wake workspace changes"
            }
            WakeWorkspaceFinalize::LeaveDirty => {
                "chore(proxima): record wake workspace changes [WIP - leave_dirty]"
            }
        };
        git_output(
            staging,
            &[
                "-c",
                "user.name=Proxima Wake",
                "-c",
                "user.email=wake@proxima.local",
                "commit",
                "-m",
                message,
            ],
        )
        .await
        .map_err(|stderr| format!("git commit: {stderr}"))?;
        true
    };

    // 2. Return the wake branch to the real repo. A clone has its own
    //    object store, so unlike a worktree this fetch is required; the
    //    real repo's working tree and current branch are untouched.
    let staging_arg = staging.to_string_lossy().to_string();
    let refspec = format!("{wake_branch}:refs/heads/{wake_branch}");
    git_output(repo, &["fetch", staging_arg.as_str(), refspec.as_str()])
        .await
        .map_err(|stderr| format!("git fetch wake branch: {stderr}"))?;

    // 3. Diff stat of the committed branch against its base, read from the
    //    real repo now that the branch lives there.
    let head_sha = git_output(repo, &["rev-parse", wake_branch])
        .await
        .map_err(|stderr| format!("git rev-parse {wake_branch}: {stderr}"))?;
    let numstat = git_output(repo, &["diff", "--numstat", base_sha, wake_branch])
        .await
        .map_err(|stderr| format!("git diff --numstat {base_sha} {wake_branch}: {stderr}"))?;
    let diff_stat = parse_numstat(&numstat);

    // 4. Discard the disposable clone.
    std::fs::remove_dir_all(staging)
        .map_err(|err| format!("remove staging clone {staging_arg}: {err}"))?;

    Ok(CoreGitWorkspaceFinalization {
        head_sha,
        committed,
        diff_stat,
    })
}

fn parse_numstat(numstat: &str) -> CoreWorkspaceDiffStat {
    let mut files = Vec::new();
    let mut insertions = 0_u64;
    let mut deletions = 0_u64;
    for line in numstat.lines().filter(|line| !line.trim().is_empty()) {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() < 3 {
            continue;
        }
        let file_insertions = parts[0].parse::<u64>().unwrap_or(0);
        let file_deletions = parts[1].parse::<u64>().unwrap_or(0);
        insertions = insertions.saturating_add(file_insertions);
        deletions = deletions.saturating_add(file_deletions);
        files.push(CoreWorkspaceDiffFile {
            path: parts[2..].join("\t"),
            insertions: file_insertions,
            deletions: file_deletions,
        });
    }
    CoreWorkspaceDiffStat {
        files_changed: files.len() as u64,
        insertions,
        deletions,
        files,
    }
}

async fn git_output(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .await
        .map_err(|err| err.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn provider_target_failure_reason(err: &ProviderTargetBuildError) -> String {
    match err {
        ProviderTargetBuildError::MissingCredentials { env } => {
            format!("credentials_missing:{env}")
        }
        ProviderTargetBuildError::NotYetSupported { variant } => {
            format!("provider_not_yet_supported:{variant}")
        }
    }
}

/// Round-1 `system_prompt`: handle-context preamble prepended to the
/// Root Perspective's `system_prompt`. The preamble reads from the
/// pre-seeded handle struct so the model knows which handles refer to
/// the triggering memory, root perspective, and self.
fn build_system_prompt(
    wake_context: &WakeContext,
    seeded: &crate::mcp::PreSeededHandles,
    continuation: Option<&super::input::FireWakeContinuation>,
) -> String {
    let schema_id = wake_context.triggering_memory.schema_id.as_str();
    let schema_arg = if schema_id.is_empty() {
        None
    } else {
        Some(schema_id)
    };
    let mut prompt = crate::wake::handles::format_wake_context_preamble(
        seeded,
        schema_arg,
        wake_context.triggering_memory.kind.as_str(),
    );
    if let Some(continuation) = continuation {
        prompt.push_str(&format_continuation_preamble(seeded, continuation));
    }
    prompt.push_str(&wake_context.root_perspective.system_prompt);
    prompt
}

fn format_continuation_preamble(
    seeded: &crate::mcp::PreSeededHandles,
    continuation: &super::input::FireWakeContinuation,
) -> String {
    format!(
        "\nContinuation:\n\
         - This invocation continues a prior truncated wake. Use persisted Proxima state as the continuity source; provider chat session state is not available.\n\
         - Open these handles before acting:\n\
         - continuation.intervention_decision.handle: {}\n\
         - continuation.intervention_request.handle: {}\n\
         - continuation.prior_wake_trace.handle: {}\n\
         - continuation.original_triggering_memory.handle: {}\n\
         - granted_rounds: {}\n\
         - supervisor_rationale: {}\n\
         - Inspect the prior trace or lineage before repeating work.\n\n",
        seeded
            .continuation_decision
            .as_ref()
            .map(crate::mcp::Handle::as_str)
            .unwrap_or("<unavailable>"),
        seeded
            .continuation_request
            .as_ref()
            .map(crate::mcp::Handle::as_str)
            .unwrap_or("<unavailable>"),
        seeded
            .continuation_wake_trace
            .as_ref()
            .map(crate::mcp::Handle::as_str)
            .unwrap_or("<unavailable>"),
        seeded
            .continuation_original_triggering
            .as_ref()
            .map(crate::mcp::Handle::as_str)
            .unwrap_or("<unavailable>"),
        continuation.grant_rounds,
        continuation.rationale.trim(),
    )
}

fn continuation_context_params(
    seeded: &crate::mcp::PreSeededHandles,
    continuation: &super::input::FireWakeContinuation,
) -> serde_json::Value {
    serde_json::json!({
        "intervention_decision": {
            "handle": seeded.continuation_decision.as_ref().map(crate::mcp::Handle::as_str),
        },
        "intervention_request": {
            "handle": seeded.continuation_request.as_ref().map(crate::mcp::Handle::as_str),
        },
        "prior_wake_trace": {
            "handle": seeded.continuation_wake_trace.as_ref().map(crate::mcp::Handle::as_str),
        },
        "original_triggering_memory": {
            "handle": seeded.continuation_original_triggering.as_ref().map(crate::mcp::Handle::as_str),
        },
        "grant_rounds": continuation.grant_rounds,
        "rationale": continuation.rationale.as_str(),
        "instruction": "Inspect the prior trace or lineage before repeating work. Provider chat session state is unavailable; persisted graph state is the continuity source.",
    })
}

async fn build_context_params(
    engine: &Engine,
    input: &FireWakeEntryInput,
    wake_context: &WakeContext,
    seeded_handles: &crate::mcp::PreSeededHandles,
    handles: &HandleTable,
    tool_projection: &[HarnessToolProjection],
) -> Result<HashMap<String, serde_json::Value>, ProtocolError> {
    let mut context_params: HashMap<String, serde_json::Value> = HashMap::new();
    context_params.insert(
        "root_perspective".into(),
        project_root_perspective(wake_context, handles),
    );
    context_params.insert(
        "active_perspectives".into(),
        project_active_perspectives(wake_context, handles),
    );
    context_params.insert(
        "active_goals".into(),
        project_active_goals(wake_context, handles),
    );
    context_params.insert("trigger_event".into(), project_trigger_event(wake_context));
    let payload_memory_classes = load_payload_memory_classes(
        engine,
        &input.owner,
        &wake_context.triggering_memory.typed_payload,
    )
    .await?;
    context_params.insert(
        "triggering_memory".into(),
        project_triggering_memory(wake_context, handles, &payload_memory_classes),
    );
    context_params.insert(
        "wake_contract".into(),
        context_value(build_wake_contract(
            &input.wake_entry,
            tool_projection,
            handles,
        ))?,
    );
    let coordination_context = chat::build_wake_coordination_context(
        engine,
        &input.owner,
        input.personality_instance_id,
        &input.wake_entry,
    )
    .await
    .map_err(|err| ProtocolError::internal(format!("build_wake_coordination_context: {err}")))?;
    context_params.insert(
        "coordination_context".into(),
        project_coordination_context(&coordination_context, handles),
    );
    if let Some(continuation) = input.continuation.as_ref() {
        context_params.insert(
            "continuation".into(),
            continuation_context_params(seeded_handles, continuation),
        );
    }
    Ok(context_params)
}

fn project_root_perspective(
    wake_context: &WakeContext,
    handles: &HandleTable,
) -> serde_json::Value {
    serde_json::json!({
        "personality": handles
            .assign_personality(PersonalityInstanceId::new(wake_context.root_perspective.instance_id))
            .as_str()
            .to_string(),
        "root_perspective": handles
            .assign_perspective_memory(MemoryId::new(wake_context.root_perspective.memory_id))
            .as_str()
            .to_string(),
        "display_name": wake_context.root_perspective.display_name.as_str(),
        "purpose": wake_context.root_perspective.purpose.as_str(),
        "system_prompt": wake_context.root_perspective.system_prompt.as_str(),
    })
}

fn project_active_perspectives(
    wake_context: &WakeContext,
    handles: &HandleTable,
) -> serde_json::Value {
    serde_json::Value::Array(
        wake_context
            .active_perspectives
            .iter()
            .map(|perspective| {
                let (text, truncated) =
                    truncate_context_text(&perspective.text, ACTIVE_PERSPECTIVE_TEXT_LIMIT);
                serde_json::json!({
                    "perspective": handles
                        .assign_perspective_memory(MemoryId::new(perspective.memory_id))
                        .as_str()
                        .to_string(),
                    "schema_id": perspective.schema_id.as_str(),
                    "schema_version": perspective.schema_version,
                    "text": text,
                    "wake_chain_depth": perspective.wake_chain_depth,
                    "truncated": truncated,
                })
            })
            .collect(),
    )
}

fn truncate_context_text(text: &str, max_chars: usize) -> (String, bool) {
    let mut chars = text.chars();
    let truncated: String = chars.by_ref().take(max_chars).collect();
    let was_truncated = chars.next().is_some();
    (truncated, was_truncated)
}

fn project_active_goals(wake_context: &WakeContext, handles: &HandleTable) -> serde_json::Value {
    serde_json::Value::Array(
        wake_context
            .active_goals
            .iter()
            .map(|goal| {
                serde_json::json!({
                    "goal": handles.assign_goal(GoalId::new(goal.goal_id)).as_str().to_string(),
                    "goal_activated_memory": goal
                        .goal_activated_memory_id
                        .map(|id| handles.assign_fact_memory(MemoryId::new(id)).as_str().to_string()),
                    "title": goal.title.as_str(),
                    "motivation_via": goal
                        .motivation_via
                        .iter()
                        .map(|id| handles.assign_perspective_memory(MemoryId::new(*id)).as_str().to_string())
                        .collect::<Vec<_>>(),
                })
            })
            .collect(),
    )
}

fn project_trigger_event(wake_context: &WakeContext) -> serde_json::Value {
    serde_json::json!({
        "kind": wake_context.trigger_event.kind.as_str(),
        "schema_id": wake_context.trigger_event.schema_id.as_str(),
        "wake_chain_depth": wake_context.trigger_event.wake_chain_depth,
    })
}

fn project_triggering_memory(
    wake_context: &WakeContext,
    handles: &HandleTable,
    memory_classes: &HashMap<Uuid, MemoryHandleClass>,
) -> serde_json::Value {
    serde_json::json!({
        "memory": handles
            .assign_memory_kind(MemoryId::new(wake_context.triggering_memory.memory_id), &wake_context.triggering_memory.kind)
            .as_str()
            .to_string(),
        "kind": wake_context.triggering_memory.kind.as_str(),
        "schema_id": wake_context.triggering_memory.schema_id.as_str(),
        "schema_version": wake_context.triggering_memory.schema_version,
        "typed_payload": project_model_value(&wake_context.triggering_memory.typed_payload, None, handles, memory_classes),
    })
}

fn project_coordination_context(
    context: &chat::WakeCoordinationContext,
    handles: &HandleTable,
) -> serde_json::Value {
    serde_json::json!({
        "chat_targets": context
            .chat_targets
            .iter()
            .map(|target| {
                serde_json::json!({
                    "personality": handles
                        .assign_personality(PersonalityInstanceId::new(target.personality_instance_id))
                        .as_str()
                        .to_string(),
                    "display_name": target.display_name.as_str(),
                    "root_perspective": handles
                        .assign_perspective_memory(MemoryId::new(target.root_perspective_memory_id))
                        .as_str()
                        .to_string(),
                    "chat_message_wake_entries": target
                        .chat_message_wake_entry_ids
                        .iter()
                        .map(|id| handles.assign_wake_entry(*id).as_str().to_string())
                        .collect::<Vec<_>>(),
                })
            })
            .collect::<Vec<_>>(),
        "wake_path": {
            "upstream": context
                .wake_path
                .upstream
                .iter()
                .map(|node| project_wake_path_node(node, handles))
                .collect::<Vec<_>>(),
            "current": project_wake_path_node(&context.wake_path.current, handles),
            "downstream": context
                .wake_path
                .downstream
                .iter()
                .map(|node| project_wake_path_node(node, handles))
                .collect::<Vec<_>>(),
        }
    })
}

fn project_wake_path_node(node: &chat::WakePathNode, handles: &HandleTable) -> serde_json::Value {
    serde_json::json!({
        "personality": handles
            .assign_personality(PersonalityInstanceId::new(node.personality_instance_id))
            .as_str()
            .to_string(),
        "display_name": node.display_name.as_str(),
        "root_perspective": if node.root_perspective_memory_id == Uuid::nil() {
            serde_json::Value::Null
        } else {
            serde_json::Value::String(
                handles
                    .assign_perspective_memory(MemoryId::new(node.root_perspective_memory_id))
                    .as_str()
                    .to_string(),
            )
        },
        "wake_entry": handles.assign_wake_entry(node.wake_entry_id).as_str().to_string(),
        "wake_entry_label": node.wake_entry_label.as_str(),
        "trigger_schema_id": node.trigger_schema_id.as_str(),
        "produces_schema_ids": node.produces_schema_ids.clone(),
    })
}

fn project_model_value(
    value: &serde_json::Value,
    key: Option<&str>,
    handles: &HandleTable,
    memory_classes: &HashMap<Uuid, MemoryHandleClass>,
) -> serde_json::Value {
    match value {
        serde_json::Value::String(raw) => project_model_string(raw, key, handles, memory_classes),
        serde_json::Value::Array(values) => serde_json::Value::Array(
            values
                .iter()
                .map(|value| project_model_value(value, key, handles, memory_classes))
                .collect(),
        ),
        serde_json::Value::Object(map) => serde_json::Value::Object(
            map.iter()
                .map(|(field, value)| {
                    (
                        field.clone(),
                        project_model_value(value, Some(field.as_str()), handles, memory_classes),
                    )
                })
                .collect(),
        ),
        other => other.clone(),
    }
}

fn project_model_string(
    raw: &str,
    key: Option<&str>,
    handles: &HandleTable,
    memory_classes: &HashMap<Uuid, MemoryHandleClass>,
) -> serde_json::Value {
    let Some(uuid) = Uuid::parse_str(raw).ok() else {
        return serde_json::Value::String(redact_uuid_substrings(raw));
    };
    let Some(key) = key else {
        return serde_json::Value::String("<opaque-uuid>".to_string());
    };
    let normalized = normalize_reference_key(key);
    if normalized == "goal_id" || normalized.ends_with("_goal_id") {
        return serde_json::Value::String(
            handles.assign_goal(GoalId::new(uuid)).as_str().to_string(),
        );
    }
    if normalized == "repo_id" || normalized.ends_with("_repo_id") {
        return serde_json::Value::String(
            handles
                .assign_flavor_object("code/repository", uuid, 'R')
                .as_str()
                .to_string(),
        );
    }
    if fact_memory_field(&normalized) {
        return serde_json::Value::String(
            handles
                .assign_fact_memory(MemoryId::new(uuid))
                .as_str()
                .to_string(),
        );
    }
    if perspective_memory_field(&normalized) {
        return serde_json::Value::String(
            handles
                .assign_perspective_memory(MemoryId::new(uuid))
                .as_str()
                .to_string(),
        );
    }
    if generic_memory_field(&normalized) {
        if let Some(class) = memory_classes.get(&uuid).copied() {
            return serde_json::Value::String(
                handles
                    .assign_memory_with_class(MemoryId::new(uuid), class)
                    .as_str()
                    .to_string(),
            );
        }
        return serde_json::Value::String("<opaque-memory-uuid>".to_string());
    }
    if normalized == "personality_instance_id" || normalized.ends_with("_personality_instance_id") {
        return serde_json::Value::String(
            handles
                .assign_personality(PersonalityInstanceId::new(uuid))
                .as_str()
                .to_string(),
        );
    }
    if normalized == "wake_entry_id" || normalized.ends_with("_wake_entry_id") {
        return serde_json::Value::String(handles.assign_wake_entry(uuid).as_str().to_string());
    }
    if normalized == "edge_id" || normalized.ends_with("_edge_id") {
        return serde_json::Value::String(
            handles
                .assign_edge(crate::EdgeId::new(uuid))
                .as_str()
                .to_string(),
        );
    }
    serde_json::Value::String("<opaque-uuid>".to_string())
}

fn redact_uuid_substrings(raw: &str) -> String {
    const UUID_LEN: usize = 36;
    if raw.len() < UUID_LEN {
        return raw.to_string();
    }
    let mut output = String::with_capacity(raw.len());
    let mut cursor = 0;
    while cursor < raw.len() {
        let Some(remaining) = raw.get(cursor..) else {
            break;
        };
        if remaining.len() >= UUID_LEN
            && let Some(candidate) = raw.get(cursor..cursor + UUID_LEN)
            && Uuid::parse_str(candidate).is_ok()
        {
            output.push_str("<opaque-uuid>");
            cursor += UUID_LEN;
            continue;
        }
        let Some(ch) = remaining.chars().next() else {
            break;
        };
        output.push(ch);
        cursor += ch.len_utf8();
    }
    output
}

fn fact_memory_field(normalized: &str) -> bool {
    matches!(
        normalized,
        "goal_activated_memory_id"
            | "intervention_request_memory_id"
            | "intervention_decision_memory_id"
            | "wake_trace_memory_id"
            | "workspace_run_memory_id"
            | "workspace_review_memory_id"
            | "workspace_decision_memory_id"
            | "execution_request_memory_id"
            | "prior_execution_request_memory_id"
            | "message_memory_id"
            | "reply_memory_id"
    )
}

fn perspective_memory_field(normalized: &str) -> bool {
    matches!(
        normalized,
        "root_perspective_memory_id" | "current_root_perspective_memory_id"
    )
}

fn generic_memory_field(normalized: &str) -> bool {
    normalized == "memory_id" || normalized.ends_with("_memory_id")
}

fn normalize_reference_key(key: &str) -> String {
    if let Some(stem) = key.strip_suffix("_ids_used") {
        format!("{stem}_id")
    } else if let Some(stem) = key.strip_suffix("_ids") {
        format!("{stem}_id")
    } else {
        key.strip_suffix('s').unwrap_or(key).to_string()
    }
}

async fn load_payload_memory_classes(
    engine: &Engine,
    owner: &Owner,
    payload: &serde_json::Value,
) -> Result<HashMap<Uuid, MemoryHandleClass>, ProtocolError> {
    let mut ids = HashSet::new();
    collect_generic_memory_ids(payload, None, &mut ids);
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let mut req = QueryRequest::for_owner(owner.clone());
    req.memory_ids = ids.into_iter().map(MemoryId::new).collect();
    req.limit = u32::try_from(req.memory_ids.len()).unwrap_or(u32::MAX);
    req.include_payloads = false;
    req.supersession = SupersessionStatus::IncludeSuperseded;
    let response = engine
        .storage()
        .query_memories(&req, engine.registry().list().as_slice())
        .await
        .map_err(|err| ProtocolError::internal(format!("query payload memory classes: {err}")))?;
    Ok(response
        .memories
        .into_iter()
        .filter_map(|memory| {
            memory_class_for_entity_kind(memory.kind).map(|class| (memory.id.into_inner(), class))
        })
        .collect())
}

fn collect_generic_memory_ids(
    value: &serde_json::Value,
    key: Option<&str>,
    ids: &mut HashSet<Uuid>,
) {
    match value {
        serde_json::Value::String(raw) => {
            let Some(key) = key else {
                return;
            };
            let normalized = normalize_reference_key(key);
            if generic_memory_field(&normalized) {
                if let Ok(uuid) = Uuid::parse_str(raw) {
                    ids.insert(uuid);
                }
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                collect_generic_memory_ids(value, key, ids);
            }
        }
        serde_json::Value::Object(map) => {
            for (field, value) in map {
                collect_generic_memory_ids(value, Some(field.as_str()), ids);
            }
        }
        _ => {}
    }
}

fn memory_class_for_entity_kind(kind: crate::EntityKind) -> Option<MemoryHandleClass> {
    match kind {
        crate::EntityKind::Fact => Some(MemoryHandleClass::Fact),
        crate::EntityKind::Abstraction => Some(MemoryHandleClass::Abstraction),
        crate::EntityKind::Perspective => Some(MemoryHandleClass::Perspective),
        crate::EntityKind::Goal => None,
    }
}

fn context_value<T: serde::Serialize>(value: T) -> Result<serde_json::Value, ProtocolError> {
    serde_json::to_value(value)
        .map_err(|err| ProtocolError::internal(format!("serialize wake context: {err}")))
}

/// Mirror the harness JSONL into `~/.proxima/wake-runs/<owner>/<invocation_id>/worker-session.jsonl`
/// so the Shell UI's "session log" link resolves for native-harness invocations.
/// On error from the harness (no `outcome` to extract bytes from) write a single
/// synthesized `record: "error"` line so the file is never empty.
async fn write_session_jsonl_to_disk(
    path: &std::path::Path,
    outcome_result: &Result<crate::harness::HarnessOutcome, crate::harness::HarnessError>,
) {
    let bytes: Vec<u8> = match outcome_result {
        Ok(outcome) => outcome.jsonl_bytes.clone(),
        Err(err) => {
            let line = serde_json::json!({
                "record": "error",
                "message": err.to_string(),
            });
            let mut s = line.to_string();
            s.push('\n');
            s.into_bytes()
        }
    };
    if bytes.is_empty() {
        return;
    }
    if let Some(parent) = path.parent()
        && let Err(err) = tokio::fs::create_dir_all(parent).await
    {
        tracing::warn!(error = %err, path = %parent.display(), "failed to create wake-run dir");
        return;
    }
    if let Err(err) = tokio::fs::write(path, &bytes).await {
        tracing::warn!(error = %err, path = %path.display(), "failed to write worker-session.jsonl");
    }
}

fn warn_if_failed(input: &FireWakeEntryInput, outcome: &WakeInvocationFinalizeOutcome) {
    if matches!(
        outcome.status,
        WakeInvocationStatus::Failed | WakeInvocationStatus::Truncated
    ) {
        tracing::warn!(
            personality_instance_id = %input.personality_instance_id.into_inner(),
            wake_entry_id = %input.wake_entry.wake_entry_id,
            change_event_seq = %input.change_event_seq,
            status = outcome.status.as_str(),
            failure_reason = outcome.failure_reason.as_deref().unwrap_or(""),
            "wake invocation did not complete successfully"
        );
    }
}

async fn maybe_emit_intervention_request(
    engine: &Engine,
    input: &FireWakeEntryInput,
    wake_context: &WakeContext,
    invocation_id: Uuid,
    trace_outcome: Option<&WakeTracePersistOutcome>,
    outcome: &WakeInvocationFinalizeOutcome,
) {
    if !matches!(outcome.status, WakeInvocationStatus::Truncated) {
        return;
    }
    if outcome.failure_reason.as_deref() != Some("max_rounds_reached") {
        return;
    }
    let Some(policy) = input.wake_entry.intervention_policy.as_ref() else {
        return;
    };
    let Some(trace_outcome) = trace_outcome else {
        tracing::warn!(
            invocation_id = %invocation_id,
            "wake intervention request skipped because wake trace persistence failed"
        );
        return;
    };
    let requested_at = time::OffsetDateTime::now_utc();
    let active_goal_ids = wake_context
        .active_goals
        .iter()
        .map(|goal| GoalId::new(goal.goal_id))
        .collect::<Vec<_>>();
    let request = InterventionRequestedV1 {
        original_invocation_id: invocation_id,
        original_wake_entry_id: input.wake_entry.wake_entry_id,
        original_personality_instance_id: input.personality_instance_id.into_inner(),
        original_change_event_seq: input.change_event_seq,
        triggering_memory_id: wake_context.triggering_memory.memory_id,
        wake_trace_memory_id: trace_outcome.fact_memory_id.into_inner(),
        target_intervention_personality_instance_id: policy.intervention_personality_instance_id,
        max_rounds: input.wake_entry.max_rounds,
        rounds_used: outcome.turn_count.unwrap_or(input.wake_entry.max_rounds),
        intervention_extension_rounds: policy.intervention_extension_rounds,
        intervention_hard_cap_rounds: policy.intervention_hard_cap_rounds,
        continued_rounds_used: 0,
        active_goal_ids: active_goal_ids
            .iter()
            .map(|goal_id| goal_id.into_inner())
            .collect(),
        progress_contract: policy.intervention_progress_contract.clone(),
        idempotency_key: format!("intervention-request:{invocation_id}"),
        requested_at,
    };
    let persist = InterventionRequestPersistInput {
        owner: input.owner.clone(),
        root_perspective_memory_id: MemoryId::new(wake_context.root_perspective.memory_id),
        request,
        source_batch_id: SourceBatchId::new(Uuid::now_v7()),
        source_id: SourceId::new(crate::INTERVENTION_SOURCE_ID),
    };
    if let Err(err) = engine
        .storage()
        .persist_intervention_requested_atomic(engine.registry(), &persist)
        .await
    {
        tracing::warn!(
            invocation_id = %invocation_id,
            error = %err,
            "wake intervention request persistence failed"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn continuation_preamble_uses_persisted_proxima_state_not_provider_state() {
        let handles = HandleTable::new();
        let continuation = crate::wake::fire::input::FireWakeContinuation {
            intervention_decision_memory_id: MemoryId::new(Uuid::now_v7()),
            intervention_request_memory_id: MemoryId::new(Uuid::now_v7()),
            original_invocation_id: Uuid::now_v7(),
            original_change_event_seq: Uuid::now_v7(),
            wake_trace_memory_id: MemoryId::new(Uuid::now_v7()),
            original_triggering_memory_id: MemoryId::new(Uuid::now_v7()),
            grant_rounds: 3,
            rationale: "made progress".into(),
        };
        let seeded = crate::mcp::PreSeededHandles {
            triggering: handles.assign_fact_memory(MemoryId::new(Uuid::now_v7())),
            root_perspective: handles.assign_perspective_memory(MemoryId::new(Uuid::now_v7())),
            self_instance: handles.assign_personality(PersonalityInstanceId::new(Uuid::now_v7())),
            continuation_decision: Some(
                handles.assign_fact_memory(continuation.intervention_decision_memory_id),
            ),
            continuation_request: Some(
                handles.assign_fact_memory(continuation.intervention_request_memory_id),
            ),
            continuation_wake_trace: Some(
                handles.assign_fact_memory(continuation.wake_trace_memory_id),
            ),
            continuation_original_triggering: Some(
                handles.assign_fact_memory(continuation.original_triggering_memory_id),
            ),
        };

        let preamble = format_continuation_preamble(&seeded, &continuation);

        assert!(preamble.contains("persisted Proxima state"));
        assert!(preamble.contains("provider chat session state is not available"));
        assert!(preamble.contains("continuation.intervention_decision.handle"));
        assert!(preamble.contains("continuation.intervention_request.handle"));
        assert!(preamble.contains("continuation.prior_wake_trace.handle"));
        assert!(preamble.contains("continuation.original_triggering_memory.handle"));
        assert!(preamble.contains("granted_rounds: 3"));
        assert!(preamble.contains("supervisor_rationale: made progress"));
        assert!(!preamble.contains(&continuation.original_invocation_id.to_string()));
        assert!(!preamble.contains(&continuation.original_change_event_seq.to_string()));

        let params = continuation_context_params(&seeded, &continuation);
        assert_eq!(
            params["intervention_decision"]["handle"],
            seeded.continuation_decision.as_ref().unwrap().as_str()
        );
        assert_eq!(
            params["intervention_request"]["handle"],
            seeded.continuation_request.as_ref().unwrap().as_str()
        );
        assert_eq!(
            params["prior_wake_trace"]["handle"],
            seeded.continuation_wake_trace.as_ref().unwrap().as_str()
        );
        assert_eq!(
            params["original_triggering_memory"]["handle"],
            seeded
                .continuation_original_triggering
                .as_ref()
                .unwrap()
                .as_str()
        );
    }

    #[test]
    fn model_payload_projection_turns_reference_uuids_into_handles() {
        let handles = HandleTable::new();
        let goal_id = Uuid::now_v7();
        let memory_id = Uuid::now_v7();
        let repo_id = Uuid::now_v7();
        let projected = project_model_value(
            &serde_json::json!({
                "goal_id": goal_id,
                "goal_activated_memory_id": memory_id,
                "repo_id": repo_id,
            }),
            None,
            &handles,
            &HashMap::new(),
        );

        assert_eq!(projected["goal_id"], "G1");
        assert_eq!(projected["goal_activated_memory_id"], "F1");
        assert_eq!(projected["repo_id"], "R1");
        assert_eq!(
            handles
                .resolve_goal("G1")
                .expect("goal handle")
                .into_inner(),
            goal_id
        );
        assert_eq!(
            handles
                .resolve_memory("F1")
                .expect("memory handle")
                .into_inner(),
            memory_id
        );
        assert_eq!(
            handles
                .resolve_flavor_object("R1", "code/repository")
                .expect("repo handle"),
            repo_id
        );
    }

    #[test]
    fn model_payload_projection_preserves_generic_memory_handles_by_class() {
        let handles = HandleTable::new();
        let fact_id = Uuid::now_v7();
        let abstraction_id = Uuid::now_v7();
        let perspective_id = Uuid::now_v7();
        let unknown_id = Uuid::now_v7();
        let mut memory_classes = HashMap::new();
        memory_classes.insert(fact_id, MemoryHandleClass::Fact);
        memory_classes.insert(abstraction_id, MemoryHandleClass::Abstraction);
        memory_classes.insert(perspective_id, MemoryHandleClass::Perspective);

        let projected = project_model_value(
            &serde_json::json!({
                "context_memory_ids": [fact_id, abstraction_id, perspective_id],
                "context_memory_ids_used": [abstraction_id],
                "unrelated_memory_id": unknown_id,
            }),
            None,
            &handles,
            &memory_classes,
        );

        assert_eq!(projected["context_memory_ids"][0], "F1");
        assert_eq!(projected["context_memory_ids"][1], "A1");
        assert_eq!(projected["context_memory_ids"][2], "P1");
        assert_eq!(projected["context_memory_ids_used"][0], "A1");
        assert_eq!(projected["unrelated_memory_id"], "<opaque-memory-uuid>");
    }

    #[test]
    fn model_payload_projection_redacts_embedded_uuid_substrings() {
        let handles = HandleTable::new();
        let raw_uuid = Uuid::now_v7();
        let projected = project_model_value(
            &serde_json::json!({
                "worktree_path": format!("/tmp/worktrees/{raw_uuid}/repo"),
                "branch_name": format!("proxima/wake/{raw_uuid}"),
            }),
            None,
            &handles,
            &HashMap::new(),
        );

        assert_eq!(
            projected["worktree_path"],
            "/tmp/worktrees/<opaque-uuid>/repo"
        );
        assert_eq!(projected["branch_name"], "proxima/wake/<opaque-uuid>");
    }

    #[test]
    fn continuation_wake_context_uses_original_change_event() {
        let decision_event_seq = Uuid::from_u128(1);
        let original_change_event_seq = Uuid::from_u128(2);
        let input = FireWakeEntryInput {
            owner: Owner {
                principal: crate::Principal::User(crate::UserId::new(Uuid::from_u128(3))),
                org_id: crate::OrgId::new(Uuid::from_u128(4)),
            },
            personality_instance_id: PersonalityInstanceId::new(Uuid::from_u128(5)),
            wake_entry: crate::personality::WakeEntryRow {
                wake_entry_id: Uuid::from_u128(6),
                trigger_kind: crate::personality::WakeEntryTriggerKind::OnMemory,
                trigger_id: "proxima-code/test-request-v1".into(),
                label: "Tester".into(),
                enabled: true,
                execution_mode: WakeEntryExecutionMode::Workspace,
                authored_by: crate::personality::WakeEntryAuthoredBy::Any,
                probability_promille: 1000,
                goal_scope: crate::personality::WakeEntryGoalScope::None,
                instructions: "test".into(),
                model_tier: crate::ModelTier::Standard,
                inference_target_ref: None,
                substrate_tool_palette: Vec::new(),
                workspace_tool_palette: Vec::new(),
                workspace_binding: None,
                required_produced_schema_ids: Vec::new(),
                max_rounds: 4,
                intervention_policy: None,
                disabled_reason: None,
            },
            change_event_seq: decision_event_seq,
            triggering_memory_id: Uuid::from_u128(7),
            continuation: Some(crate::wake::fire::input::FireWakeContinuation {
                intervention_decision_memory_id: MemoryId::new(Uuid::from_u128(8)),
                intervention_request_memory_id: MemoryId::new(Uuid::from_u128(9)),
                original_invocation_id: Uuid::from_u128(10),
                original_change_event_seq,
                wake_trace_memory_id: MemoryId::new(Uuid::from_u128(11)),
                original_triggering_memory_id: MemoryId::new(Uuid::from_u128(12)),
                grant_rounds: 4,
                rationale: "continue".into(),
            }),
        };

        assert_eq!(
            wake_context_change_event_seq(&input),
            original_change_event_seq
        );
    }

    /// Build a minimal real repo with one commit; returns `(repo_dir, base_sha)`.
    async fn seed_repo(repo: &Path) -> String {
        git_output(repo, &["init", "-q"]).await.unwrap();
        git_output(repo, &["config", "user.name", "Test"])
            .await
            .unwrap();
        git_output(repo, &["config", "user.email", "test@proxima.local"])
            .await
            .unwrap();
        std::fs::write(repo.join("seed.txt"), "seed\n").unwrap();
        git_output(repo, &["add", "-A"]).await.unwrap();
        git_output(repo, &["commit", "-q", "-m", "seed"])
            .await
            .unwrap();
        git_output(repo, &["rev-parse", "HEAD"]).await.unwrap()
    }

    #[tokio::test]
    async fn wake_clone_returns_changes_as_branch_without_touching_real_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        let base_sha = seed_repo(&repo).await;
        let real_head_before = git_output(&repo, &["rev-parse", "HEAD"]).await.unwrap();
        let branch = "proxima/wake/test";
        let staging = tmp.path().join("wake-clones").join("test");

        clone_repo_to_staging(&repo, &staging, branch, &base_sha)
            .await
            .unwrap();
        // The clone is a real, self-contained repo positioned on the wake branch.
        assert!(staging.join(".git").is_dir(), "clone has a real .git dir");
        assert!(staging.join("seed.txt").is_file());
        let staging_branch = git_output(&staging, &["rev-parse", "--abbrev-ref", "HEAD"])
            .await
            .unwrap();
        assert_eq!(staging_branch, branch);

        // The personality makes a change inside the clone.
        std::fs::write(staging.join("wake.txt"), "wake output\n").unwrap();

        let finalization =
            finalize_core_git_clone(&staging, &repo, branch, &base_sha, WakeWorkspaceFinalize::CommitAll)
                .await
                .unwrap();

        assert!(finalization.committed);
        assert_eq!(finalization.diff_stat.files_changed, 1);
        assert_eq!(finalization.diff_stat.files[0].path, "wake.txt");

        // The wake branch landed in the real repo, pointing at the commit.
        let fetched = git_output(&repo, &["rev-parse", branch]).await.unwrap();
        assert_eq!(fetched, finalization.head_sha);

        // The real repo's working tree and current branch are untouched.
        assert!(!repo.join("wake.txt").exists(), "real working tree untouched");
        let real_status = git_output(&repo, &["status", "--porcelain"]).await.unwrap();
        assert!(real_status.is_empty(), "real repo working tree stays clean");
        let real_head_after = git_output(&repo, &["rev-parse", "HEAD"]).await.unwrap();
        assert_eq!(real_head_after, real_head_before, "real HEAD unmoved");

        // The disposable clone is discarded.
        assert!(!staging.exists(), "staging clone removed after finalize");
    }

    #[tokio::test]
    async fn wake_clone_with_no_changes_reports_uncommitted() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        let base_sha = seed_repo(&repo).await;
        let branch = "proxima/wake/empty";
        let staging = tmp.path().join("clone");

        clone_repo_to_staging(&repo, &staging, branch, &base_sha)
            .await
            .unwrap();
        let finalization =
            finalize_core_git_clone(&staging, &repo, branch, &base_sha, WakeWorkspaceFinalize::CommitAll)
                .await
                .unwrap();

        assert!(!finalization.committed);
        assert_eq!(finalization.head_sha, base_sha, "no commit, head stays at base");
        assert_eq!(finalization.diff_stat.files_changed, 0);
    }

    #[tokio::test]
    async fn wake_clone_leave_dirty_marks_commit_as_wip() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        let base_sha = seed_repo(&repo).await;
        let branch = "proxima/wake/wip";
        let staging = tmp.path().join("clone");

        clone_repo_to_staging(&repo, &staging, branch, &base_sha)
            .await
            .unwrap();
        std::fs::write(staging.join("wake.txt"), "draft\n").unwrap();
        finalize_core_git_clone(&staging, &repo, branch, &base_sha, WakeWorkspaceFinalize::LeaveDirty)
            .await
            .unwrap();

        let subject = git_output(&repo, &["log", "-1", "--format=%s", branch])
            .await
            .unwrap();
        assert!(subject.contains("WIP"), "leave_dirty commit subject marks WIP: {subject}");
    }
}
