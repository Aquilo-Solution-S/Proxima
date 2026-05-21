//! Wake-trace persistence mapping for finished harness invocations.

use crate::harness::{
    FinishReason, HarnessError, HarnessOutcome, HarnessOutcomeKind, ProviderTarget,
};
use crate::personality::WakeTraceOutcomeKind;
use crate::verbs::persist_wake_trace::{WakeTracePersistInput, WakeTracePersistOutcome};
use crate::wake::context::WakeContext;
use crate::wake::fire::input::FireWakeEntryInput;
use crate::wake::fire::resolve::ResolvedTarget;
use crate::wake::trace::WakeTracePayload;
use crate::{Engine, GoalId, MemoryId, SourceBatchId, SourceId, StorageError};

#[derive(Debug, Clone, Copy)]
pub struct TraceTiming {
    pub started_at: time::OffsetDateTime,
    pub finished_at: time::OffsetDateTime,
}

pub async fn emit_trace_from_outcome(
    engine: &Engine,
    input: &FireWakeEntryInput,
    wake_context: &WakeContext,
    resolved: &ResolvedTarget,
    invocation_id: uuid::Uuid,
    outcome_result: &Result<HarnessOutcome, HarnessError>,
    timing: TraceTiming,
) -> Result<WakeTracePersistOutcome, StorageError> {
    let persist = persist_input_from_outcome(
        input,
        wake_context,
        resolved,
        invocation_id,
        outcome_result,
        timing,
    );
    persist_wake_trace(engine, invocation_id, persist).await
}

pub async fn emit_trace_from_failed_preflight(
    engine: &Engine,
    input: &FireWakeEntryInput,
    wake_context: &WakeContext,
    resolved: &ResolvedTarget,
    invocation_id: uuid::Uuid,
    timing: TraceTiming,
    failure_reason: String,
) -> Result<WakeTracePersistOutcome, StorageError> {
    let outcome = HarnessOutcome {
        kind: HarnessOutcomeKind::Failed,
        finish_reason: FinishReason::Stop,
        error_class: crate::harness::ErrorClass::InvalidRequest,
        failure_reason: Some(failure_reason),
        rounds_used: 0,
        duration_ms: 0,
        total_prompt_tokens: None,
        total_completion_tokens: None,
        tool_call_count: 0,
        jsonl_bytes: Vec::new(),
        jsonl_truncated: false,
    };
    let persist = persist_input_from_outcome(
        input,
        wake_context,
        resolved,
        invocation_id,
        &Ok(outcome),
        timing,
    );
    persist_wake_trace(engine, invocation_id, persist).await
}

async fn persist_wake_trace(
    engine: &Engine,
    invocation_id: uuid::Uuid,
    persist: WakeTracePersistInput,
) -> Result<WakeTracePersistOutcome, StorageError> {
    let result = engine.persist_wake_trace_internal(persist).await;
    if let Err(ref err) = result {
        tracing::warn!(
            invocation_id = %invocation_id,
            error = %err,
            "persist_wake_trace failed",
        );
    }
    result
}

fn persist_input_from_outcome(
    input: &FireWakeEntryInput,
    wake_context: &WakeContext,
    resolved: &ResolvedTarget,
    invocation_id: uuid::Uuid,
    outcome_result: &Result<HarnessOutcome, HarnessError>,
    timing: TraceTiming,
) -> WakeTracePersistInput {
    let (
        jsonl_bytes,
        jsonl_truncated,
        outcome_kind,
        failure_reason,
        rounds_used,
        finish_reason,
        prompt_tokens,
        completion_tokens,
        tool_call_count,
    ) = match outcome_result {
        Ok(outcome) => (
            outcome.jsonl_bytes.clone(),
            outcome.jsonl_truncated,
            outcome_kind_typed(outcome.kind),
            outcome.failure_reason.clone(),
            outcome.rounds_used,
            Some(finish_reason_str(outcome.finish_reason).to_string()),
            outcome.total_prompt_tokens,
            outcome.total_completion_tokens,
            outcome.tool_call_count,
        ),
        Err(err) => (
            Vec::new(),
            false,
            WakeTraceOutcomeKind::Failed,
            Some(err.to_string()),
            0,
            None,
            None,
            None,
            0,
        ),
    };

    let jsonl_content_hash = *blake3::hash(&jsonl_bytes).as_bytes();
    let jsonl_line_count = jsonl_bytes.iter().filter(|&&b| b == b'\n').count() as u64;
    let active_goal_ids = wake_context
        .active_goals
        .iter()
        .map(|goal| GoalId::new(goal.goal_id))
        .collect();

    let wake_trace = WakeTracePayload {
        invocation_id,
        wake_entry_id: input.wake_entry.wake_entry_id,
        personality_instance_id: input.personality_instance_id.into_inner(),
        model_target_ref: resolved.target_ref.clone(),
        model_id: model_id(&resolved.config),
        started_at: timing.started_at,
        finished_at: timing.finished_at,
        outcome_kind,
        failure_reason,
        rounds_used,
        finish_reason,
        total_prompt_tokens: prompt_tokens,
        total_completion_tokens: completion_tokens,
        tool_call_count,
        jsonl_truncated,
    };

    WakeTracePersistInput {
        owner: input.owner.clone(),
        authoring_personality_instance_id: input.personality_instance_id.into_inner(),
        root_perspective_memory_id: MemoryId::new(wake_context.root_perspective.memory_id),
        triggering_memory_id: MemoryId::new(wake_context.triggering_memory.memory_id),
        active_goal_ids,
        jsonl_bytes,
        jsonl_content_hash,
        jsonl_line_count,
        jsonl_truncated,
        citation_byte_range: None,
        wake_trace,
        source_id: SourceId::new("core/wake-trace".to_string()),
        source_batch_id: SourceBatchId::new(uuid::Uuid::now_v7()),
        observed_at: time::OffsetDateTime::now_utc(),
        occurred_at: timing.finished_at,
    }
}

fn outcome_kind_typed(kind: HarnessOutcomeKind) -> WakeTraceOutcomeKind {
    match kind {
        HarnessOutcomeKind::Succeeded => WakeTraceOutcomeKind::Succeeded,
        HarnessOutcomeKind::Truncated => WakeTraceOutcomeKind::Truncated,
        HarnessOutcomeKind::Failed => WakeTraceOutcomeKind::Failed,
    }
}

fn finish_reason_str(reason: FinishReason) -> &'static str {
    match reason {
        FinishReason::Stop => "stop",
        FinishReason::ToolCalls => "tool_calls",
        FinishReason::Length => "length",
        FinishReason::MaxRounds => "max_rounds",
        FinishReason::Cancelled => "cancelled",
        FinishReason::Unknown(value) => value,
    }
}

fn model_id(config: &crate::InferenceTargetConfig) -> String {
    match config {
        crate::InferenceTargetConfig::MistralChat(cfg) => cfg.model_id.clone(),
        crate::InferenceTargetConfig::OpenAIChat(cfg) => cfg.model_id.clone(),
        crate::InferenceTargetConfig::OpenAIResponses(cfg) => cfg.model_id.clone(),
        crate::InferenceTargetConfig::ChatGPTCodex(cfg) => cfg.model_id.clone(),
    }
}

pub fn provider_target_from_config(
    config: &crate::InferenceTargetConfig,
) -> Result<ProviderTarget, ProviderTargetBuildError> {
    match config {
        crate::InferenceTargetConfig::MistralChat(cfg) => Ok(ProviderTarget::MistralChat {
            base_url: cfg.base_url.clone(),
            model_id: cfg.model_id.clone(),
            api_key: read_key(&cfg.api_key_env)?,
            temperature: cfg.temperature,
            max_completion_tokens: cfg.max_completion_tokens,
            reasoning_effort: cfg.reasoning_effort.clone(),
        }),
        crate::InferenceTargetConfig::OpenAIChat(cfg) => Ok(ProviderTarget::OpenAIChat {
            base_url: cfg.base_url.clone(),
            model_id: cfg.model_id.clone(),
            api_key: read_key(&cfg.api_key_env)?,
            temperature: cfg.temperature,
            max_completion_tokens: cfg.max_completion_tokens,
        }),
        crate::InferenceTargetConfig::OpenAIResponses(cfg) => Ok(ProviderTarget::OpenAIResponses {
            base_url: cfg.base_url.clone(),
            model_id: cfg.model_id.clone(),
            api_key: read_key(&cfg.api_key_env)?,
            reasoning_effort: cfg.reasoning_effort.clone(),
        }),
        crate::InferenceTargetConfig::ChatGPTCodex(cfg) => {
            let home = std::env::var_os("HOME").ok_or_else(|| {
                ProviderTargetBuildError::MissingCredentials {
                    env: "HOME".to_string(),
                }
            })?;
            let auth_json = std::path::PathBuf::from(home).join(".codex/auth.json");
            Ok(ProviderTarget::ChatGPTCodex {
                base_url: cfg.base_url.clone(),
                model_id: cfg.model_id.clone(),
                reasoning_effort: cfg.reasoning_effort.clone(),
                auth_json,
            })
        }
    }
}

fn read_key(env: &str) -> Result<String, ProviderTargetBuildError> {
    std::env::var(env).map_err(|_| ProviderTargetBuildError::MissingCredentials {
        env: env.to_string(),
    })
}

#[derive(Debug, thiserror::Error)]
pub enum ProviderTargetBuildError {
    #[error("missing credentials in env var {env}")]
    MissingCredentials { env: String },
    #[error("provider variant {variant} not yet supported in harness dispatch")]
    NotYetSupported { variant: &'static str },
}
