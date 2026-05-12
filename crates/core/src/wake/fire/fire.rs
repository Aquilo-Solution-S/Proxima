//! Per-entry wake fire path backed by the in-process harness.

use std::collections::HashMap;
use std::time::Duration;

use uuid::Uuid;

use crate::MemoryId;
use crate::engine::Engine;
use crate::error::ProtocolError;
use crate::harness::{HarnessAdapter, HarnessContext, HarnessProgram};
use crate::personality::workspace::{
    WorkspaceFinalizeInput, WorkspaceOutcome, WorkspacePrepareInput, WorkspacePreparedRun,
    WorkspaceRunnerError,
};
use crate::personality::{
    PersonalityInstanceId, WakeChainDepth, WakeEntryExecutionMode, WakeInvocationStart,
    WakeInvocationStatus,
};
use crate::wake::context::{WakeContext, assemble_wake_context};
use crate::wake::token_store::WakeTokenContext;
use crate::wake::trace::emit::{
    ProviderTargetBuildError, emit_trace_from_failed_preflight, emit_trace_from_outcome,
    provider_target_from_config,
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
    let wake_token = mint_wake_token(
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
            owner: input.owner.clone(),
            personality_instance_id: input.personality_instance_id,
            wake_entry_id: input.wake_entry.wake_entry_id,
            change_event_seq: input.change_event_seq,
            wake_token,
            resolved_inference_target_ref: resolved.target_ref.clone(),
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
        "started",
        session_log_path.display().to_string(),
    )
    .await;

    let started_at = time::OffsetDateTime::now_utc();
    let context_params = match build_context_params(&wake_context) {
        Ok(params) => params,
        Err(err) => {
            let finished_at = time::OffsetDateTime::now_utc();
            finalize_failed_started_wake(
                engine,
                &input,
                &wake_context,
                &resolved,
                invocation_id_for_dispatch,
                wake_token,
                started_at,
                finished_at,
                format!("context_param_serialization:{err}"),
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
            input,
            wake_token,
            wake_context,
            resolved,
            context_params,
            invocation_id_for_dispatch,
            invocation_timeout,
            started_at,
        )
        .await;
    }

    let provider_target = match provider_target_from_config(&resolved.config) {
        Ok(target) => target,
        Err(err) => {
            let finished_at = time::OffsetDateTime::now_utc();
            finalize_failed_started_wake(
                engine,
                &input,
                &wake_context,
                &resolved,
                invocation_id_for_dispatch,
                wake_token,
                started_at,
                finished_at,
                provider_target_failure_reason(&err),
            )
            .await?;
            return Ok(true);
        }
    };
    let program = HarnessProgram {
        system_prompt: wake_context.root_perspective.system_prompt.clone(),
        instructions: input.wake_entry.instructions.clone(),
        context_params,
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
    let finished_at = time::OffsetDateTime::now_utc();

    engine.wake_token_store().revoke(wake_token).await;
    append_session_log_error_if_present(engine, &input, &outcome_result).await;
    emit_trace_from_outcome(
        engine,
        &input,
        &wake_context,
        &resolved,
        invocation_id_for_dispatch,
        &outcome_result,
        started_at,
        finished_at,
    )
    .await
    .ok();

    let outcome = wake_outcome_from_harness_outcome(&input, outcome_result);
    warn_if_failed(&input, &outcome);
    finalize(engine, &input, outcome).await?;
    Ok(true)
}

#[expect(
    clippy::too_many_arguments,
    reason = "workspace fire state is explicit"
)]
async fn handle_workspace_mode(
    engine: &Engine,
    adapter: &dyn HarnessAdapter,
    input: FireWakeEntryInput,
    wake_token: Uuid,
    wake_context: WakeContext,
    resolved: ResolvedTarget,
    mut context_params: HashMap<String, serde_json::Value>,
    invocation_id_for_dispatch: Uuid,
    invocation_timeout: Duration,
    started_at: time::OffsetDateTime,
) -> Result<bool, ProtocolError> {
    let flavor_id = input.wake_entry.trigger_id.split('/').next().unwrap_or("");
    let Some(runner) = engine.registry().workspace_runner(flavor_id) else {
        engine.wake_token_store().revoke(wake_token).await;
        finalize(
            engine,
            &input,
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
            let finished_at = time::OffsetDateTime::now_utc();
            let failure_reason = provider_target_failure_reason(&err);
            let finalize_outcome = WakeInvocationFinalizeOutcome::failed(failure_reason);
            finalize_pre_run_workspace(
                engine,
                &input,
                &wake_context,
                &resolved,
                invocation_id_for_dispatch,
                wake_token,
                prepared,
                started_at,
                finished_at,
                finalize_outcome,
            )
            .await?;
            return Ok(true);
        }
    };
    let program = HarnessProgram {
        system_prompt: wake_context.root_perspective.system_prompt.clone(),
        instructions: input.wake_entry.instructions.clone(),
        context_params,
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
    let finished_at = time::OffsetDateTime::now_utc();

    append_session_log_error_if_present(engine, &input, &outcome_result).await;
    emit_trace_from_outcome(
        engine,
        &input,
        &wake_context,
        &resolved,
        invocation_id_for_dispatch,
        &outcome_result,
        started_at,
        finished_at,
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
    finalize(engine, &input, outcome).await?;
    Ok(true)
}

async fn mint_wake_token(
    engine: &Engine,
    input: &FireWakeEntryInput,
    wake_context: &WakeContext,
    resolved: &ResolvedTarget,
    invocation_id: Uuid,
    invocation_timeout: Duration,
) -> Uuid {
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
    };
    engine
        .wake_token_store()
        .mint_with_max_lifetime(token_ctx, invocation_timeout)
        .await
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

async fn finalize_failed_started_wake(
    engine: &Engine,
    input: &FireWakeEntryInput,
    wake_context: &WakeContext,
    resolved: &ResolvedTarget,
    invocation_id: Uuid,
    wake_token: Uuid,
    started_at: time::OffsetDateTime,
    finished_at: time::OffsetDateTime,
    failure_reason: String,
) -> Result<(), ProtocolError> {
    emit_trace_from_failed_preflight(
        engine,
        input,
        wake_context,
        resolved,
        invocation_id,
        started_at,
        finished_at,
        failure_reason.clone(),
    )
    .await
    .ok();

    engine.wake_token_store().revoke(wake_token).await;
    finalize(
        engine,
        input,
        WakeInvocationFinalizeOutcome::failed(failure_reason),
    )
    .await
}

#[expect(
    clippy::too_many_arguments,
    reason = "failure cleanup state is explicit"
)]
async fn finalize_pre_run_workspace(
    engine: &Engine,
    input: &FireWakeEntryInput,
    wake_context: &WakeContext,
    resolved: &ResolvedTarget,
    invocation_id: Uuid,
    wake_token: Uuid,
    prepared: WorkspacePreparedRun,
    started_at: time::OffsetDateTime,
    finished_at: time::OffsetDateTime,
    finalize_outcome: WakeInvocationFinalizeOutcome,
) -> Result<(), ProtocolError> {
    emit_trace_from_failed_preflight(
        engine,
        input,
        wake_context,
        resolved,
        invocation_id,
        started_at,
        finished_at,
        finalize_outcome
            .failure_reason
            .clone()
            .unwrap_or_else(|| "pre_run_failure".to_string()),
    )
    .await
    .ok();
    let workspace_outcome = WorkspaceOutcome {
        exit_code: finalize_outcome.exit_code,
        stdout_tail: finalize_outcome.stdout_tail.clone(),
        stderr_tail: finalize_outcome.stderr_tail.clone(),
        duration_ms: finalize_outcome.duration_ms,
    };
    let _ = finalize_workspace_runner(
        engine,
        input,
        wake_context,
        invocation_id,
        prepared,
        workspace_outcome,
    )
    .await;
    engine.wake_token_store().revoke(wake_token).await;
    finalize(engine, input, finalize_outcome).await
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
    }
}

fn build_context_params(
    wake_context: &WakeContext,
) -> Result<HashMap<String, serde_json::Value>, serde_json::Error> {
    let mut context_params: HashMap<String, serde_json::Value> = HashMap::new();
    context_params.insert(
        "root_perspective".into(),
        serde_json::to_value(&wake_context.root_perspective)?,
    );
    context_params.insert(
        "active_goals".into(),
        serde_json::to_value(&wake_context.active_goals)?,
    );
    context_params.insert(
        "trigger_event".into(),
        serde_json::to_value(&wake_context.trigger_event)?,
    );
    context_params.insert(
        "triggering_memory".into(),
        serde_json::to_value(&wake_context.triggering_memory)?,
    );
    Ok(context_params)
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
