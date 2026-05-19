//! Per-entry wake fire path backed by the in-process harness.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use uuid::Uuid;

use crate::engine::Engine;
use crate::error::ProtocolError;
use crate::harness::{
    HarnessAdapter, HarnessContext, HarnessProgram, HarnessToolProjection,
    build_wake_tool_projection,
};
use crate::mcp::HandleTable;
use crate::personality::workspace::{
    WorkspaceFinalizeInput, WorkspaceOutcome, WorkspacePrepareInput, WorkspacePreparedRun,
    WorkspaceRunnerError,
};
use crate::personality::{
    PersonalityInstanceId, WakeChainDepth, WakeEntryExecutionMode, WakeInvocationContinuation,
    WakeInvocationLogStatus, WakeInvocationStart, WakeInvocationStatus,
};
use crate::verbs::persist_wake_trace::WakeTracePersistOutcome;
use crate::wake::context::{WakeContext, assemble_wake_context};
use crate::wake::contract::build_wake_contract;
use crate::wake::token_store::WakeTokenContext;
use crate::wake::trace::emit::{
    ProviderTargetBuildError, TraceTiming, emit_trace_from_failed_preflight,
    emit_trace_from_outcome, provider_target_from_config,
};
use crate::{
    GoalId, InterventionRequestPersistInput, InterventionRequestedV1, MemoryId, SourceBatchId,
    SourceId, inquiry,
};

use super::finalize::{
    append_session_artifact_log, append_session_log_error_if_present, finalize,
    wake_session_log_path,
};
use super::input::{FireWakeEntryInput, per_invocation_timeout};
use super::outcome::{WakeInvocationFinalizeOutcome, wake_outcome_from_harness_outcome};
use super::resolve::{ResolvedTarget, collect_sidecars, resolve_target};

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
    let wake_context = assemble_wake_context(
        engine.storage().as_ref(),
        &input.owner,
        input.personality_instance_id,
        input.change_event_seq,
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
        substrate_tool_palette: input.wake_entry.substrate_tool_palette.clone(),
        workspace_root: None,
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
    wake_context: WakeContext,
    resolved: ResolvedTarget,
    context_params: HashMap<String, serde_json::Value>,
    tool_projection: Vec<HarnessToolProjection>,
    session_log_path: std::path::PathBuf,
    invocation_id_for_dispatch: Uuid,
    invocation_timeout: Duration,
    started_at: time::OffsetDateTime,
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
        wake_context,
        resolved,
        mut context_params,
        tool_projection,
        session_log_path,
        invocation_id_for_dispatch,
        invocation_timeout,
        started_at,
    } = state;

    let flavor_id = input.wake_entry.trigger_id.split('/').next().unwrap_or("");
    let Some(runner) = engine.registry().workspace_runner(flavor_id) else {
        engine.wake_token_store().revoke(wake_token).await;
        finalize(
            engine,
            &input,
            invocation_id_for_dispatch,
            WakeInvocationFinalizeOutcome::failed(format!(
                "workspace_no_runner_for_flavor:{flavor_id}"
            )),
        )
        .await?;
        return Ok(true);
    };

    if !engine
        .registry()
        .is_workspace_trigger(&input.wake_entry.trigger_id)
    {
        engine.wake_token_store().revoke(wake_token).await;
        finalize(
            engine,
            &input,
            invocation_id_for_dispatch,
            WakeInvocationFinalizeOutcome::failed(format!(
                "workspace_trigger_not_eligible:{}",
                input.wake_entry.trigger_id
            )),
        )
        .await?;
        return Ok(true);
    }

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
        workspace_tool_palette: &input.wake_entry.workspace_tool_palette,
    };
    let prepared = match runner.prepare(prepare_input).await {
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
        context_params.insert("workspace_context".to_string(), ws_ctx);
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
        substrate_tool_palette: input.wake_entry.substrate_tool_palette.clone(),
        workspace_root: Some(prepared.work_dir.clone()),
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
        triggering_event_memory_id: MemoryId::new(wake_context.triggering_memory.memory_id),
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
                .assign_memory(continuation.intervention_decision_memory_id),
        );
        seeded.continuation_request = Some(
            token_ctx
                .handles
                .assign_memory(continuation.intervention_request_memory_id),
        );
        seeded.continuation_wake_trace = Some(
            token_ctx
                .handles
                .assign_memory(continuation.wake_trace_memory_id),
        );
        seeded.continuation_original_triggering = Some(
            token_ctx
                .handles
                .assign_memory(continuation.original_triggering_memory_id),
        );
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
    let flavor_id = input.wake_entry.trigger_id.split('/').next().unwrap_or("");
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
    let mut prompt = crate::wake::handles::format_wake_context_preamble(seeded, schema_arg);
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
        "\nWake continuation context:\n\
         - This invocation continues a prior truncated wake. Use persisted Proxima state as the continuity source; provider chat session state is not available.\n\
         - intervention_request_memory: {}\n\
         - intervention_decision_memory: {}\n\
         - wake_trace_memory: {}\n\
         - original_triggering_memory: {}\n\
         - granted_rounds: {}\n\
         - supervisor_rationale: {}\n\
         - Inspect the prior trace or lineage before repeating work.\n\n",
        seeded
            .continuation_request
            .as_ref()
            .map(crate::mcp::Handle::as_str)
            .unwrap_or("<unavailable>"),
        seeded
            .continuation_decision
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
        "active_goals".into(),
        project_active_goals(wake_context, handles),
    );
    context_params.insert("trigger_event".into(), project_trigger_event(wake_context));
    context_params.insert(
        "triggering_memory".into(),
        project_triggering_memory(wake_context, handles),
    );
    context_params.insert(
        "wake_contract".into(),
        context_value(build_wake_contract(
            &input.wake_entry,
            tool_projection,
            handles,
        ))?,
    );
    let coordination_context = inquiry::build_wake_coordination_context(
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
            .assign_memory(MemoryId::new(wake_context.root_perspective.memory_id))
            .as_str()
            .to_string(),
        "display_name": wake_context.root_perspective.display_name.as_str(),
        "purpose": wake_context.root_perspective.purpose.as_str(),
        "system_prompt": wake_context.root_perspective.system_prompt.as_str(),
    })
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
                        .map(|id| handles.assign_memory(MemoryId::new(id)).as_str().to_string()),
                    "title": goal.title.as_str(),
                    "motivation_via": goal
                        .motivation_via
                        .iter()
                        .map(|id| handles.assign_memory(MemoryId::new(*id)).as_str().to_string())
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
) -> serde_json::Value {
    serde_json::json!({
        "memory": handles
            .assign_memory(MemoryId::new(wake_context.triggering_memory.memory_id))
            .as_str()
            .to_string(),
        "schema_id": wake_context.triggering_memory.schema_id.as_str(),
        "schema_version": wake_context.triggering_memory.schema_version,
        "typed_payload": project_model_value(&wake_context.triggering_memory.typed_payload, None, handles),
    })
}

fn project_coordination_context(
    context: &inquiry::WakeCoordinationContext,
    handles: &HandleTable,
) -> serde_json::Value {
    serde_json::json!({
        "askable_personalities": context
            .askable_personalities
            .iter()
            .map(|target| {
                serde_json::json!({
                    "personality": handles
                        .assign_personality(PersonalityInstanceId::new(target.personality_instance_id))
                        .as_str()
                        .to_string(),
                    "display_name": target.display_name.as_str(),
                    "root_perspective": handles
                        .assign_memory(MemoryId::new(target.root_perspective_memory_id))
                        .as_str()
                        .to_string(),
                    "directed_question_wake_entries": target
                        .directed_question_wake_entry_ids
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

fn project_wake_path_node(
    node: &inquiry::WakePathNode,
    handles: &HandleTable,
) -> serde_json::Value {
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
                    .assign_memory(MemoryId::new(node.root_perspective_memory_id))
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
) -> serde_json::Value {
    match value {
        serde_json::Value::String(raw) => project_model_string(raw, key, handles),
        serde_json::Value::Array(values) => serde_json::Value::Array(
            values
                .iter()
                .map(|value| project_model_value(value, key, handles))
                .collect(),
        ),
        serde_json::Value::Object(map) => serde_json::Value::Object(
            map.iter()
                .map(|(field, value)| {
                    (
                        field.clone(),
                        project_model_value(value, Some(field.as_str()), handles),
                    )
                })
                .collect(),
        ),
        other => other.clone(),
    }
}

fn project_model_string(raw: &str, key: Option<&str>, handles: &HandleTable) -> serde_json::Value {
    let Some(uuid) = Uuid::parse_str(raw).ok() else {
        return serde_json::Value::String(raw.to_string());
    };
    let Some(key) = key else {
        return serde_json::Value::String("<opaque-uuid>".to_string());
    };
    let normalized = key.strip_suffix('s').unwrap_or(key);
    if normalized == "goal_id" || normalized.ends_with("_goal_id") {
        return serde_json::Value::String(
            handles.assign_goal(GoalId::new(uuid)).as_str().to_string(),
        );
    }
    if normalized == "memory_id" || normalized.ends_with("_memory_id") {
        return serde_json::Value::String(
            handles
                .assign_memory(MemoryId::new(uuid))
                .as_str()
                .to_string(),
        );
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
            triggering: handles.assign_memory(MemoryId::new(Uuid::now_v7())),
            root_perspective: handles.assign_memory(MemoryId::new(Uuid::now_v7())),
            self_instance: handles.assign_personality(PersonalityInstanceId::new(Uuid::now_v7())),
            continuation_decision: Some(
                handles.assign_memory(continuation.intervention_decision_memory_id),
            ),
            continuation_request: Some(
                handles.assign_memory(continuation.intervention_request_memory_id),
            ),
            continuation_wake_trace: Some(handles.assign_memory(continuation.wake_trace_memory_id)),
            continuation_original_triggering: Some(
                handles.assign_memory(continuation.original_triggering_memory_id),
            ),
        };

        let preamble = format_continuation_preamble(&seeded, &continuation);

        assert!(preamble.contains("persisted Proxima state"));
        assert!(preamble.contains("provider chat session state is not available"));
        assert!(preamble.contains("wake_trace_memory"));
        assert!(preamble.contains("intervention_decision_memory"));
        assert!(preamble.contains("original_triggering_memory"));
        assert!(preamble.contains("granted_rounds: 3"));
        assert!(preamble.contains("supervisor_rationale: made progress"));
        assert!(!preamble.contains(&continuation.original_invocation_id.to_string()));
        assert!(!preamble.contains(&continuation.original_change_event_seq.to_string()));
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
        );

        assert_eq!(projected["goal_id"], "G1");
        assert_eq!(projected["goal_activated_memory_id"], "N1");
        assert_eq!(projected["repo_id"], "<opaque-uuid>");
        assert_eq!(
            handles
                .resolve_goal("G1")
                .expect("goal handle")
                .into_inner(),
            goal_id
        );
        assert_eq!(
            handles
                .resolve_memory("N1")
                .expect("memory handle")
                .into_inner(),
            memory_id
        );
    }
}
