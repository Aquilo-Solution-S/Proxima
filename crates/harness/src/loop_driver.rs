//! `HarnessLoop` — the concrete `HarnessAdapter` impl.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use proxima_core::Engine;
use proxima_core::harness::{
    ErrorClass, FinishReason, HarnessAdapter, HarnessContext, HarnessError, HarnessOutcome,
    HarnessProgram, ProviderTarget, classify_outcome, duration_ms,
};
use proxima_core::personality::parse_scoped_emit_tool_id;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::conversation::{ToolResultStatus, ToolResultTurn, Turn};
use crate::program::{ResolvedProgram, resolve};
use crate::progress::FulfillmentProgress;
#[cfg(feature = "chatgpt-codex")]
use crate::providers::chatgpt_codex::ChatGPTCodexClient;
use crate::providers::mistral_chat::MistralChatClient;
use crate::providers::openai_chat::OpenAIChatClient;
use crate::providers::openai_responses::OpenAIResponsesClient;
use crate::providers::{ProviderClient, ProviderError, RoundResult};
use crate::tools::ToolBinding;
use crate::trace::jsonl::JsonlBuffer;

const PROMPT_RECORD: &str = "prompt";
const TOOLS_SENT_RECORD: &str = "tools_sent";
const PROVIDER_ROUND_MAX_ATTEMPTS: u32 = 4;
const PROVIDER_RETRY_BASE_DELAY: Duration = Duration::from_secs(2);
const PROVIDER_RETRY_MAX_DELAY: Duration = Duration::from_secs(30);
const PROVIDER_RETRY_AFTER_MAX_DELAY: Duration = Duration::from_secs(90);
const PROVIDER_REQUEST_TIMEOUT: Duration = Duration::from_mins(10);
const FINISH_SOON_REMAINING_THRESHOLD: u32 = 3;

#[derive(Clone)]
pub struct HarnessLoop {
    pub engine: Arc<Engine>,
    pub substrate_bridge: Arc<dyn proxima_core::mcp::HarnessSubstrateBridge>,
    pub jsonl_cap_bytes: usize,
}

impl HarnessLoop {
    #[must_use]
    pub fn new(
        engine: Arc<Engine>,
        substrate_bridge: Arc<dyn proxima_core::mcp::HarnessSubstrateBridge>,
    ) -> Self {
        Self {
            engine,
            substrate_bridge,
            jsonl_cap_bytes: 5 * 1024 * 1024,
        }
    }
}

impl std::fmt::Debug for HarnessLoop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HarnessLoop")
            .field("jsonl_cap_bytes", &self.jsonl_cap_bytes)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl HarnessAdapter for HarnessLoop {
    async fn run(
        &self,
        program: HarnessProgram,
        ctx: HarnessContext,
    ) -> Result<HarnessOutcome, HarnessError> {
        let max_rounds = program.max_rounds;
        let provider_target = program.provider.clone();
        let model_id = model_id_for_log(&provider_target);
        let provider = build_provider(&provider_target);

        let substrate_tools =
            resolve_substrate_tools(&*self.substrate_bridge, &program.substrate_tool_palette)
                .map_err(HarnessError::Internal)?;
        let resolved = resolve(program, &substrate_tools)
            .map_err(|err| HarnessError::Internal(format!("program_resolve:{err}")))?;

        

        run_loop(
            self,
            &*provider,
            resolved,
            ctx,
            max_rounds,
            &model_id,
        )
        .await
    }
}

fn model_id_for_log(target: &ProviderTarget) -> String {
    match target {
        ProviderTarget::MistralChat { model_id, .. }
        | ProviderTarget::OpenAIChat { model_id, .. }
        | ProviderTarget::OpenAIResponses { model_id, .. }
        | ProviderTarget::ChatGPTCodex { model_id, .. } => model_id.clone(),
    }
}

fn build_provider(target: &ProviderTarget) -> Box<dyn ProviderClient> {
    match target {
        ProviderTarget::MistralChat {
            base_url,
            model_id,
            api_key,
            temperature,
            max_completion_tokens,
            reasoning_effort,
            ..
        } => Box::new(MistralChatClient {
            http: reqwest::Client::new(),
            base_url: base_url.clone(),
            model_id: model_id.clone(),
            api_key: api_key.clone(),
            temperature: *temperature,
            max_completion_tokens: *max_completion_tokens,
            reasoning_effort: reasoning_effort.clone(),
        }),
        ProviderTarget::OpenAIChat {
            base_url,
            model_id,
            api_key,
            temperature,
            max_completion_tokens,
            ..
        } => Box::new(OpenAIChatClient {
            http: reqwest::Client::new(),
            base_url: base_url.clone(),
            model_id: model_id.clone(),
            api_key: api_key.clone(),
            temperature: *temperature,
            max_completion_tokens: *max_completion_tokens,
        }),
        ProviderTarget::OpenAIResponses {
            base_url,
            model_id,
            api_key,
            reasoning_effort,
            ..
        } => {
            let mut client =
                OpenAIResponsesClient::new(base_url.clone(), model_id.clone(), api_key.clone());
            client.reasoning_effort.clone_from(reasoning_effort);
            client.request_timeout = PROVIDER_REQUEST_TIMEOUT;
            Box::new(client)
        }
        #[cfg(feature = "chatgpt-codex")]
        ProviderTarget::ChatGPTCodex {
            base_url,
            model_id,
            reasoning_effort,
            auth_json,
            ..
        } => {
            let mut client = ChatGPTCodexClient::new(
                base_url.clone(),
                model_id.clone(),
                proxima_codex_auth::AuthDotJsonPath::from_explicit(auth_json.clone()),
            );
            client.reasoning_effort.clone_from(reasoning_effort);
            client.request_timeout = PROVIDER_REQUEST_TIMEOUT;
            Box::new(client)
        }
        #[cfg(not(feature = "chatgpt-codex"))]
        ProviderTarget::ChatGPTCodex { .. } => Box::new(crate::providers::UnavailableProvider {
            feature: "chatgpt-codex",
        }),
    }
}

fn resolve_substrate_tools(
    bridge: &dyn proxima_core::mcp::HarnessSubstrateBridge,
    palette: &[String],
) -> Result<Vec<proxima_core::harness::SubstrateToolBinding>, String> {
    let mut by_name: HashMap<_, _> = bridge
        .list_harness_tools(palette)
        .into_iter()
        .map(|spec| (spec.canonical_name.clone(), spec))
        .collect();
    let mut out = Vec::with_capacity(palette.len());
    let mut seen = std::collections::HashSet::new();

    for name in palette {
        let internal_name = parse_scoped_emit_tool_id(name)
            .map_err(|err| format!("invalid_scoped_emit_tool:{}:{}", err.tool_id, err.reason))?
            .map_or(name.as_str(), |scoped| scoped.base_tool_id);
        if !seen.insert(internal_name.to_string()) {
            continue;
        }
        let Some(spec) = by_name.remove(internal_name) else {
            return Err(format!("unknown_substrate_tool:{internal_name}"));
        };
        out.push(proxima_core::harness::SubstrateToolBinding {
            canonical_name: spec.canonical_name,
            description: spec.description,
            args_schema: spec.args_schema,
        });
    }

    Ok(out)
}

#[expect(
    clippy::too_many_lines,
    clippy::too_many_arguments,
    reason = "wake loop state is clearer in one place"
)]
async fn run_loop(
    loop_: &HarnessLoop,
    provider: &dyn ProviderClient,
    mut resolved: ResolvedProgram,
    ctx: HarnessContext,
    max_rounds: u32,
    model_id: &str,
) -> Result<HarnessOutcome, HarnessError> {
    let started = Instant::now();
    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();
    let invocation_timeout = ctx.invocation_timeout;
    tokio::spawn(async move {
        tokio::time::sleep(invocation_timeout).await;
        cancel_clone.cancel();
    });

    let mut jsonl = JsonlBuffer::with_capacity(loop_.jsonl_cap_bytes);
    jsonl.append(&json!({
        "record": "start",
        "invocation_id": ctx.invocation_id,
        "model_id": model_id,
        "max_rounds": max_rounds,
        "system_prompt": &resolved.conversation.system_prompt,
        "user_seed": &resolved.conversation.user_seed,
    }));
    append_prompt_and_tools_records(&mut jsonl, &resolved, &ctx, model_id, max_rounds);

    let user_seed_without_budget_notice = resolved.conversation.user_seed.clone();
    let mut fulfillment = FulfillmentProgress::new(
        &resolved.tool_productions,
        &resolved.required_fulfillment_schema_ids,
    );
    let mut rounds_used = 0;
    let mut tool_call_count = 0;

    let (finish_reason, error_class, failure_reason) = loop {
        if fulfillment.is_stalled_after(rounds_used) {
            let reason = fulfillment.stall_reason();
            jsonl.append(&json!({
                "record": "fulfillment_stalled",
                "rounds_used": rounds_used,
                "required_schema_ids": fulfillment.required_schema_ids(),
                "reason": reason,
            }));
            break (
                FinishReason::ToolCalls,
                ErrorClass::FulfillmentStalled,
                Some(reason),
            );
        }
        if max_rounds > 0 && rounds_used >= max_rounds {
            break (FinishReason::MaxRounds, ErrorClass::None, None);
        }

        rounds_used += 1;
        apply_runtime_notices(
            &mut resolved.conversation.user_seed,
            &user_seed_without_budget_notice,
            rounds_used,
            max_rounds,
            fulfillment
                .should_remind(rounds_used)
                .then(|| fulfillment.reminder(rounds_used)),
            &mut jsonl,
        );
        jsonl.append(&json!({
            "record": "round_start",
            "round_idx": rounds_used,
        }));

        let round = provider_round_with_retries(
            provider,
            &resolved.conversation,
            &resolved.tools,
            cancel.clone(),
            &mut jsonl,
            rounds_used,
        )
        .await;

        match round {
            Ok(RoundResult::Final {
                text,
                raw_assistant,
            }) => {
                jsonl.append(&json!({
                    "record": "assistant_message",
                    "round_idx": rounds_used,
                    "text_excerpt": excerpt(&text, 2000),
                    "tool_call_count": 0,
                }));
                resolved
                    .conversation
                    .turns
                    .push(Turn::Assistant(raw_assistant));
                if fulfillment.durable_required() {
                    let reason = "fulfillment_required_but_model_stopped".to_string();
                    jsonl.append(&json!({
                        "record": "fulfillment_stalled",
                        "round_idx": rounds_used,
                        "required_schema_ids": fulfillment.required_schema_ids(),
                        "reason": reason,
                    }));
                    break (
                        FinishReason::Stop,
                        ErrorClass::FulfillmentStalled,
                        Some(reason),
                    );
                }
                break (FinishReason::Stop, ErrorClass::None, None);
            }
            Ok(RoundResult::LengthCap {
                partial_text,
                raw_assistant,
            }) => {
                jsonl.append(&json!({
                    "record": "assistant_message",
                    "round_idx": rounds_used,
                    "text_excerpt": excerpt(partial_text.as_deref().unwrap_or(""), 2000),
                    "length_cap": true,
                }));
                resolved
                    .conversation
                    .turns
                    .push(Turn::Assistant(raw_assistant));
                break (FinishReason::Length, ErrorClass::None, None);
            }
            Ok(RoundResult::ToolCalls {
                calls,
                raw_assistant,
            }) => {
                resolved
                    .conversation
                    .turns
                    .push(Turn::Assistant(raw_assistant));

                for call in calls {
                    tool_call_count += 1;
                    let canonical = resolved
                        .reverse_map
                        .get(&call.tool_name)
                        .cloned()
                        .unwrap_or_else(|| call.tool_name.clone());
                    jsonl.append(&json!({
                        "record": "tool_call",
                        "round_idx": rounds_used,
                        "call_id": call.call_id,
                        "tool_name": canonical,
                        "args": call.arguments,
                    }));

                    let dispatch_started = Instant::now();
                    let result = dispatch_one(
                        loop_,
                        &resolved,
                        &canonical,
                        call.arguments.clone(),
                        &ctx,
                        model_id,
                    )
                    .await;
                    let duration = duration_ms(dispatch_started.elapsed());
                    let (status, content, fatal) = match result {
                        DispatchOne::Ok(value) => (ToolResultStatus::Ok, value, None),
                        DispatchOne::Recoverable(message) => {
                            (ToolResultStatus::Error, json!({ "error": message }), None)
                        }
                        DispatchOne::Fatal(message) => (
                            ToolResultStatus::Error,
                            json!({ "error": message }),
                            Some(message),
                        ),
                        DispatchOne::Unknown => (
                            ToolResultStatus::Error,
                            json!({ "error": "unknown_tool", "tool_name": canonical }),
                            None,
                        ),
                    };
                    jsonl.append(&json!({
                        "record": "tool_result",
                        "round_idx": rounds_used,
                        "call_id": call.call_id,
                        "status": status,
                        "duration_ms": duration,
                        "content": content,
                    }));
                    resolved
                        .conversation
                        .turns
                        .push(Turn::ToolResult(ToolResultTurn {
                            call_id: call.call_id,
                            status,
                            content: content.clone(),
                        }));

                    if let Some(message) = fatal {
                        return Ok(finish_outcome(
                            &mut jsonl,
                            started,
                            FinishReason::ToolCalls,
                            ErrorClass::ToolDispatchFatal,
                            Some(message),
                            rounds_used,
                            max_rounds,
                            tool_call_count,
                        ));
                    }
                    match status {
                        ToolResultStatus::Ok => {
                            fulfillment.note_success();
                            if let Some(matched) = fulfillment.successful_tool_fulfills(&canonical)
                            {
                                jsonl.append(&json!({
                                    "record": "fulfillment_satisfied",
                                    "round_idx": rounds_used,
                                    "tool_name": matched.tool_name,
                                    "produced_schema_ids": matched.produced_schema_ids,
                                }));
                                return Ok(finish_outcome(
                                    &mut jsonl,
                                    started,
                                    FinishReason::Fulfilled,
                                    ErrorClass::None,
                                    None,
                                    rounds_used,
                                    max_rounds,
                                    tool_call_count,
                                ));
                            }
                        }
                        ToolResultStatus::Error => {
                            let message = content
                                .get("error")
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or("tool_error");
                            if let Some(streak) = fulfillment.note_tool_error(&canonical, message) {
                                let reason = format!(
                                    "tool_error_streak:{}:{}:{}",
                                    streak.tool_name, streak.count, streak.message
                                );
                                jsonl.append(&json!({
                                    "record": "tool_error_streak",
                                    "round_idx": rounds_used,
                                    "tool_name": streak.tool_name,
                                    "count": streak.count,
                                    "message": streak.message,
                                    "reason": reason,
                                }));
                                return Ok(finish_outcome(
                                    &mut jsonl,
                                    started,
                                    FinishReason::ToolCalls,
                                    ErrorClass::ToolErrorStreak,
                                    Some(reason),
                                    rounds_used,
                                    max_rounds,
                                    tool_call_count,
                                ));
                            }
                        }
                    }
                }
            }
            Err(error) => {
                let (class, message) = error_class_for(&error);
                jsonl.append(&json!({
                    "record": "provider_error",
                    "round_idx": rounds_used,
                    "class": format!("{class:?}"),
                    "message": message,
                }));
                break (FinishReason::Stop, class, Some(message));
            }
        }
    };

    Ok(finish_outcome(
        &mut jsonl,
        started,
        finish_reason,
        error_class,
        failure_reason,
        rounds_used,
        max_rounds,
        tool_call_count,
    ))
}

fn apply_runtime_notices(
    user_seed: &mut String,
    base_user_seed: &str,
    round_idx: u32,
    max_rounds: u32,
    fulfillment_reminder: Option<String>,
    jsonl: &mut JsonlBuffer,
) {
    let mut notices = Vec::new();
    if let Some(notice) = round_budget_notice(round_idx, max_rounds) {
        jsonl.append(&json!({
            "record": "round_budget_notice",
            "round_idx": round_idx,
            "max_rounds": max_rounds,
            "notice": notice,
        }));
        notices.push(notice);
    }
    if let Some(notice) = fulfillment_reminder {
        jsonl.append(&json!({
            "record": "fulfillment_reminder",
            "round_idx": round_idx,
            "notice": notice,
        }));
        notices.push(notice);
    }
    if notices.is_empty() {
        user_seed.clear();
        user_seed.push_str(base_user_seed);
        return;
    }
    *user_seed = format!("{base_user_seed}\n\n{}", notices.join("\n\n"));
}

fn round_budget_notice(round_idx: u32, max_rounds: u32) -> Option<String> {
    if max_rounds == 0 || round_idx > max_rounds {
        return None;
    }
    let remaining_after_this_round = max_rounds - round_idx;
    if remaining_after_this_round >= FINISH_SOON_REMAINING_THRESHOLD {
        return None;
    }
    Some(format!(
        "Runtime budget notice: this is round {round_idx} of {max_rounds}; \
         {remaining_after_this_round} rounds remain after this response. \
         You must finish soon. Prefer emitting the required final tool call or reply over further exploration."
    ))
}

async fn provider_round_with_retries(
    provider: &dyn ProviderClient,
    conversation: &crate::conversation::Conversation,
    tools: &[crate::conversation::ToolSpec],
    cancel: CancellationToken,
    jsonl: &mut JsonlBuffer,
    round_idx: u32,
) -> Result<RoundResult, ProviderError> {
    let mut attempt = 1;
    loop {
        let result = provider
            .tool_round(conversation, tools, cancel.clone())
            .await;

        match result {
            Err(error)
                if provider_error_is_retryable(&error)
                    && attempt < PROVIDER_ROUND_MAX_ATTEMPTS
                    && !cancel.is_cancelled() =>
            {
                let (class, message) = error_class_for(&error);
                let delay = provider_retry_delay(&error, attempt);
                jsonl.append(&json!({
                    "record": "provider_retry",
                    "round_idx": round_idx,
                    "attempt": attempt,
                    "next_attempt": attempt + 1,
                    "class": format!("{class:?}"),
                    "message": excerpt(&message, 1000),
                    "delay_ms": duration_ms(delay),
                }));
                sleep_or_cancel(delay, cancel.clone()).await?;
                attempt += 1;
            }
            other => return other,
        }
    }
}

async fn sleep_or_cancel(delay: Duration, cancel: CancellationToken) -> Result<(), ProviderError> {
    tokio::select! {
        () = tokio::time::sleep(delay) => Ok(()),
        () = cancel.cancelled() => Err(ProviderError::Timeout),
    }
}

fn provider_error_is_retryable(error: &ProviderError) -> bool {
    matches!(
        error,
        ProviderError::RateLimited { .. }
            | ProviderError::ServerError(_)
            | ProviderError::Network(_)
            | ProviderError::Timeout
    )
}

fn provider_retry_delay(error: &ProviderError, failed_attempt: u32) -> Duration {
    if let ProviderError::RateLimited {
        retry_after: Some(retry_after),
    } = error
    {
        return (*retry_after).min(PROVIDER_RETRY_AFTER_MAX_DELAY);
    }

    let exponent = failed_attempt.saturating_sub(1).min(8);
    let multiplier = 1_u32 << exponent;
    PROVIDER_RETRY_BASE_DELAY
        .saturating_mul(multiplier)
        .min(PROVIDER_RETRY_MAX_DELAY)
}

fn append_prompt_and_tools_records(
    jsonl: &mut JsonlBuffer,
    resolved: &ResolvedProgram,
    ctx: &HarnessContext,
    model_id: &str,
    max_rounds: u32,
) {
    jsonl.append(&json!({
        "record": PROMPT_RECORD,
        "invocation_id": ctx.invocation_id,
        "wake_entry_id": ctx.wake_entry_id,
        "personality_instance_id": ctx.personality_instance_id.into_inner(),
        "change_event_seq": ctx.change_event_seq,
        "model_id": model_id,
        "max_rounds": max_rounds,
        "system_prompt": &resolved.conversation.system_prompt,
        "user_seed": &resolved.conversation.user_seed,
    }));
    jsonl.append(&json!({
        "record": TOOLS_SENT_RECORD,
        "invocation_id": ctx.invocation_id,
        "model_id": model_id,
        "tools": resolved
            .tools
            .iter()
            .map(|tool| {
                json!({
                    "canonical_name": &tool.canonical,
                    "provider_name": &tool.provider_safe,
                    "description": &tool.description,
                    "produces_schema_ids": resolved
                        .tool_productions
                        .get(&tool.canonical)
                        .cloned()
                        .unwrap_or_default(),
                    "input_schema": &tool.input_schema,
                })
            })
            .collect::<Vec<_>>(),
    }));
}

#[expect(
    clippy::too_many_arguments,
    reason = "outcome fields are intentionally explicit"
)]
fn finish_outcome(
    jsonl: &mut JsonlBuffer,
    started: Instant,
    finish_reason: FinishReason,
    error_class: ErrorClass,
    failure_reason: Option<String>,
    rounds_used: u32,
    max_rounds: u32,
    tool_call_count: u32,
) -> HarnessOutcome {
    let duration = duration_ms(started.elapsed());
    let kind = classify_outcome(finish_reason, error_class, rounds_used, max_rounds);
    jsonl.append(&json!({
        "record": "finish",
        "outcome_kind": format!("{kind:?}"),
        "finish_reason": format!("{finish_reason:?}"),
        "error_class": format!("{error_class:?}"),
        "failure_reason": failure_reason,
        "rounds_used": rounds_used,
        "total_duration_ms": duration,
    }));
    let snapshot = jsonl.snapshot();
    HarnessOutcome {
        kind,
        finish_reason,
        error_class,
        failure_reason,
        rounds_used,
        duration_ms: duration,
        total_prompt_tokens: None,
        total_completion_tokens: None,
        tool_call_count,
        jsonl_bytes: snapshot.bytes,
        jsonl_truncated: snapshot.truncated,
        // Attached by `HarnessLoop::run` after the sandbox is torn down.
        network_log: None,
    }
}

enum DispatchOne {
    Ok(serde_json::Value),
    Recoverable(String),
    Fatal(String),
    Unknown,
}

#[expect(
    clippy::too_many_arguments,
    reason = "dispatch context is intentionally explicit"
)]
async fn dispatch_one(
    loop_: &HarnessLoop,
    resolved: &ResolvedProgram,
    canonical: &str,
    args: serde_json::Value,
    ctx: &HarnessContext,
    model_id: &str,
) -> DispatchOne {
    match resolved.bindings.get(canonical) {
        Some(ToolBinding::Substrate(binding)) => {
            use crate::tools::substrate_dispatch::{SubstrateDispatchResult, dispatch};
            match dispatch(&loop_.substrate_bridge, binding, args, ctx, model_id).await {
                SubstrateDispatchResult::Ok(value) => DispatchOne::Ok(value),
                SubstrateDispatchResult::Recoverable(message) => DispatchOne::Recoverable(message),
                SubstrateDispatchResult::Fatal(message) => DispatchOne::Fatal(message),
            }
        }
        Some(ToolBinding::TypedEmit {
            internal,
            schema_id,
            schema_version,
            kind: _,
        }) => {
            use crate::tools::substrate_dispatch::{
                SubstrateDispatchResult, dispatch, typed_emit_args,
            };
            let args = match typed_emit_args(schema_id, *schema_version, args) {
                Ok(args) => args,
                Err(message) => return DispatchOne::Recoverable(message),
            };
            match dispatch(&loop_.substrate_bridge, internal, args, ctx, model_id).await {
                SubstrateDispatchResult::Ok(value) => DispatchOne::Ok(value),
                SubstrateDispatchResult::Recoverable(message) => DispatchOne::Recoverable(message),
                SubstrateDispatchResult::Fatal(message) => DispatchOne::Fatal(message),
            }
        }
        None => DispatchOne::Unknown,
    }
}

fn error_class_for(error: &ProviderError) -> (ErrorClass, String) {
    match error {
        ProviderError::Auth => (ErrorClass::Auth, "auth".to_string()),
        ProviderError::RateLimited { .. } => (ErrorClass::RateLimited, "rate_limited".to_string()),
        ProviderError::ContextLength => (ErrorClass::ContextLength, "context_length".to_string()),
        ProviderError::InvalidRequest(message) => (ErrorClass::InvalidRequest, message.clone()),
        ProviderError::ServerError(message) => (ErrorClass::ServerError, message.clone()),
        ProviderError::Network(message) => (ErrorClass::Network, message.clone()),
        ProviderError::Timeout => (ErrorClass::Timeout, "timeout".to_string()),
        ProviderError::Deserialize(message) => (ErrorClass::Deserialize, message.clone()),
    }
}

fn excerpt(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let excerpt: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{excerpt}...")
    } else {
        excerpt
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::time::Duration;

    use proxima_core::{MemoryId, OrgId, Owner, PersonalityInstanceId, Principal, UserId};
    use serde_json::json;
    use uuid::Uuid;

    use crate::conversation::{Conversation, ToolSpec};
    use crate::program::ResolvedProgram;
    use crate::providers::ProviderError;
    use crate::tools::ToolBinding;
    use crate::trace::jsonl::JsonlBuffer;

    use super::{
        PROVIDER_RETRY_AFTER_MAX_DELAY, PROVIDER_RETRY_BASE_DELAY, append_prompt_and_tools_records,
        provider_error_is_retryable, provider_retry_delay, round_budget_notice,
    };

    #[test]
    fn provider_retry_policy_is_transient_only() {
        assert!(provider_error_is_retryable(&ProviderError::RateLimited {
            retry_after: None
        }));
        assert!(provider_error_is_retryable(&ProviderError::ServerError(
            "temporary upstream failure".into()
        )));
        assert!(provider_error_is_retryable(&ProviderError::Network(
            "connection reset".into()
        )));
        assert!(provider_error_is_retryable(&ProviderError::Timeout));

        assert!(!provider_error_is_retryable(&ProviderError::Auth));
        assert!(!provider_error_is_retryable(&ProviderError::ContextLength));
        assert!(!provider_error_is_retryable(
            &ProviderError::InvalidRequest("bad request".into())
        ));
        assert!(!provider_error_is_retryable(&ProviderError::Deserialize(
            "bad json".into()
        )));
    }

    #[test]
    fn provider_retry_delay_uses_backoff_and_bounded_retry_after() {
        assert_eq!(
            provider_retry_delay(&ProviderError::ServerError("temporary".into()), 1),
            PROVIDER_RETRY_BASE_DELAY
        );
        assert_eq!(
            provider_retry_delay(&ProviderError::Network("temporary".into()), 3),
            Duration::from_secs(8)
        );
        assert_eq!(
            provider_retry_delay(
                &ProviderError::RateLimited {
                    retry_after: Some(Duration::from_mins(10))
                },
                1,
            ),
            PROVIDER_RETRY_AFTER_MAX_DELAY
        );
    }

    #[test]
    fn round_budget_notice_starts_below_three_remaining_rounds() {
        assert!(round_budget_notice(5, 8).is_none());

        let round_six = round_budget_notice(6, 8).expect("round six warning");
        assert!(round_six.contains("round 6 of 8"));
        assert!(round_six.contains("2 rounds remain"));
        assert!(round_six.contains("You must finish soon"));

        let round_eight = round_budget_notice(8, 8).expect("final round warning");
        assert!(round_eight.contains("0 rounds remain"));
    }

    #[test]
    fn prompt_and_tools_records_are_extractable_from_jsonl() {
        let invocation_id = Uuid::now_v7();
        let resolved = ResolvedProgram {
            conversation: Conversation {
                system_prompt: "system body".into(),
                user_seed: "Wake Contract:\n{}".into(),
                turns: Vec::new(),
            },
            tools: vec![ToolSpec {
                canonical: "proxima-code/code_emit_execution_request".into(),
                provider_safe: "proxima_code_code_emit_execution_request".into(),
                description: "Emit execution request".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "evidence": { "type": "array", "items": { "type": "string" } }
                    }
                }),
            }],
            tool_productions: HashMap::from([(
                "proxima-code/code_emit_execution_request".into(),
                vec!["proxima-code/execution-request-v1".into()],
            )]),
            required_fulfillment_schema_ids: Vec::new(),
            reverse_map: HashMap::new(),
            bindings: HashMap::<String, ToolBinding>::new(),
        };
        let ctx = proxima_core::harness::HarnessContext {
            owner: Owner {
                principal: Principal::User(UserId::new(Uuid::now_v7())),
                org_id: OrgId::new(Uuid::now_v7()),
            },
            invocation_id,
            wake_entry_id: Uuid::now_v7(),
            personality_instance_id: PersonalityInstanceId::new(Uuid::now_v7()),
            change_event_seq: Uuid::now_v7(),
            root_perspective_memory_id: MemoryId::new(Uuid::now_v7()),
            wake_token: Uuid::now_v7(),
            invocation_timeout: Duration::from_secs(30),
        };
        let mut jsonl = JsonlBuffer::with_capacity(64 * 1024);

        append_prompt_and_tools_records(&mut jsonl, &resolved, &ctx, "mistral-medium-3.5", 4);

        let snapshot = jsonl.snapshot();
        let lines = String::from_utf8(snapshot.bytes).expect("jsonl utf8");
        let records: Vec<serde_json::Value> = lines
            .lines()
            .map(|line| serde_json::from_str(line).expect("json record"))
            .collect();
        let prompt = records
            .iter()
            .find(|record| record["record"] == "prompt")
            .expect("prompt record");
        let tools = records
            .iter()
            .find(|record| record["record"] == "tools_sent")
            .expect("tools_sent record");

        assert_eq!(prompt["system_prompt"], "system body");
        assert_eq!(prompt["user_seed"], "Wake Contract:\n{}");
        assert_eq!(prompt["invocation_id"], invocation_id.to_string());
        assert_eq!(
            tools["tools"][0]["canonical_name"],
            "proxima-code/code_emit_execution_request"
        );
        assert_eq!(
            tools["tools"][0]["provider_name"],
            "proxima_code_code_emit_execution_request"
        );
        assert_eq!(tools["tools"][0]["input_schema"]["type"], "object");
    }
}
