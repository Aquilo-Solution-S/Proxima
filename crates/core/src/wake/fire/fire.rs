//! Per-entry wake fire path backed by the in-process harness.

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use uuid::Uuid;

use crate::engine::Engine;
use crate::error::ProtocolError;
use crate::harness::{HarnessAdapter, HarnessContext, HarnessProgram, build_wake_tool_projection};
use crate::mcp::{HandleTable, MemoryHandleClass, PreSeededHandles};
use crate::personality::{
    PersonalityInstanceId, WakeChainDepth, WakeInvocationContinuation, WakeInvocationLogStatus,
    WakeInvocationStart, WakeInvocationStatus,
};
use crate::verbs::persist_wake_trace::WakeTracePersistOutcome;
use crate::wake::context::{WakeContext, assemble_wake_context};
use crate::wake::token_store::WakeTokenContext;
use crate::wake::trace::emit::{
    ProviderTargetBuildError, TraceTiming, emit_trace_from_failed_preflight,
    emit_trace_from_outcome, provider_target_from_config,
};
use crate::{
    GoalId, InterventionRequestPersistInput, InterventionRequestedV1, MemoryId, SourceBatchId,
    SourceId,
};

use super::context::{build_context_params, build_system_prompt};
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

fn wake_context_change_event_seq(input: &FireWakeEntryInput) -> Uuid {
    input
        .continuation
        .as_ref()
        .map_or(input.change_event_seq, |continuation| {
            continuation.original_change_event_seq
        })
}

async fn mint_wake_token(
    engine: &Engine,
    input: &FireWakeEntryInput,
    wake_context: &WakeContext,
    resolved: &ResolvedTarget,
    invocation_id: Uuid,
    invocation_timeout: Duration,
) -> (Uuid, PreSeededHandles, Arc<HandleTable>) {
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
        handles: std::sync::Arc::new(HandleTable::new()),
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

pub(super) fn context_value<T: serde::Serialize>(value: T) -> Result<Value, ProtocolError> {
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
    use crate::Owner;
    use crate::personality::WakeEntryExecutionMode;

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
                execution_mode: WakeEntryExecutionMode::SubstrateOnly,
                authored_by: crate::personality::WakeEntryAuthoredBy::Any,
                probability_promille: 1000,
                goal_scope: crate::personality::WakeEntryGoalScope::None,
                instructions: "test".into(),
                model_tier: crate::ModelTier::Standard,
                inference_target_ref: None,
                substrate_tool_palette: Vec::new(),
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
}
