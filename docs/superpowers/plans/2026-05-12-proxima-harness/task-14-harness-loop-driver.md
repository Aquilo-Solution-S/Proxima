# Task 4.3 — `HarnessLoop` driver

> Part of [Proxima Harness Implementation Plan](README.md). Subagent execution: implement steps in order, commit at the end of the task.

**Files:**
- Modify: `crates/harness/src/loop_driver.rs`
- Modify: `crates/harness/src/lib.rs`

- [ ] **Step 1: Implement the loop**

Replace `crates/harness/src/loop_driver.rs`:

```rust
//! HarnessLoop — the concrete `HarnessAdapter` impl.

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use proxima_core::Engine;
use proxima_core::harness::{
    ErrorClass, FinishReason, HarnessAdapter, HarnessContext, HarnessError, HarnessOutcome,
    HarnessProgram, ProviderTarget, classify_outcome, duration_ms,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::conversation::{AssistantTurn, ToolResultStatus, ToolResultTurn, Turn};
use crate::program::{ResolvedProgram, resolve};
use crate::providers::{ProviderClient, ProviderError, RoundResult};
use crate::providers::mistral_chat::MistralChatClient;
use crate::tools::workspace::dispatch as workspace_dispatch;
use crate::tools::{ToolBinding, WorkspaceCtx};
use crate::trace::jsonl::JsonlBuffer;

/// Concrete `HarnessAdapter` impl. Holds a clone of the Engine for
/// general use and a `HarnessSubstrateBridge` for wake-visible
/// substrate dispatch. In practice `DevMcpServer` implements the
/// bridge (Task 4.2), preserving registry MCP tools plus the
/// personality substrate pack.
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
        f.debug_struct("HarnessLoop").finish_non_exhaustive()
    }
}

#[async_trait]
impl HarnessAdapter for HarnessLoop {
    async fn run(
        &self,
        program: HarnessProgram,
        ctx: HarnessContext,
    ) -> Result<HarnessOutcome, HarnessError> {
        // Clone the provider target BEFORE `resolve(program)` consumes
        // `program`. The driver needs the typed config to build the
        // provider client; `ResolvedProgram` deliberately drops
        // `program.provider` after the conversation is materialized.
        let max_rounds = program.max_rounds;
        let workspace_root = program.workspace_root.clone();
        let provider_target = program.provider.clone();
        let model_id = model_id_for_log(&provider_target);

        let provider: Box<dyn ProviderClient> = build_provider(&self.engine, &provider_target)
            .ok_or_else(|| {
                HarnessError::InvalidProvider("unsupported provider".into())
            })?;

        let substrate_tools = resolve_substrate_tools(
            &*self.substrate_bridge,
            &program.substrate_tool_palette,
        )
        .map_err(HarnessError::Internal)?;
        let resolved = resolve(program, substrate_tools);

        run_loop(
            self,
            &*provider,
            resolved,
            workspace_root,
            ctx,
            max_rounds,
            &model_id,
        )
        .await
    }
}

fn model_id_for_log(p: &ProviderTarget) -> String {
    match p {
        ProviderTarget::MistralChat { model_id, .. }
        | ProviderTarget::OpenAIChat { model_id, .. }
        | ProviderTarget::OpenAIResponses { model_id, .. } => model_id.clone(),
    }
}

fn build_provider(_engine: &Engine, target: &ProviderTarget) -> Option<Box<dyn ProviderClient>> {
    match target {
        ProviderTarget::MistralChat {
            base_url,
            model_id,
            api_key,
            temperature,
            max_completion_tokens,
        } => {
            let mut c = MistralChatClient::new(base_url.clone(), model_id.clone(), api_key.clone());
            c.temperature = *temperature;
            c.max_completion_tokens = *max_completion_tokens;
            Some(Box::new(c))
        }
        ProviderTarget::OpenAIChat { .. } | ProviderTarget::OpenAIResponses { .. } => {
            // Phase 5 lands these.
            None
        }
    }
}

fn resolve_substrate_tools(
    bridge: &dyn proxima_core::mcp::HarnessSubstrateBridge,
    palette: &[String],
) -> Result<Vec<proxima_core::harness::SubstrateToolBinding>, String> {
    let specs = bridge.list_harness_tools(palette);
    let mut by_name: std::collections::HashMap<_, _> = specs
        .into_iter()
        .map(|s| (s.canonical_name.clone(), s))
        .collect();
    let mut out = Vec::with_capacity(palette.len());
    for name in palette {
        let Some(spec) = by_name.remove(name) else {
            return Err(format!("unknown_substrate_tool:{name}"));
        };
        out.push(proxima_core::harness::SubstrateToolBinding {
            canonical_name: spec.canonical_name,
            description: spec.description,
            args_schema: spec.args_schema,
        });
    }
    Ok(out)
}

async fn run_loop(
    loop_: &HarnessLoop,
    provider: &dyn ProviderClient,
    mut resolved: ResolvedProgram,
    workspace_root: Option<std::path::PathBuf>,
    ctx: HarnessContext,
    max_rounds: u32,
    model_id: &str,
) -> Result<HarnessOutcome, HarnessError> {
    let started = Instant::now();
    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();
    let timeout = ctx.invocation_timeout;
    tokio::spawn(async move {
        tokio::time::sleep(timeout).await;
        cancel_clone.cancel();
    });

    let mut jsonl = JsonlBuffer::with_capacity(loop_.jsonl_cap_bytes);
    jsonl.append(&json!({
        "record": "start",
        "invocation_id": ctx.invocation_id,
        "model_id": model_id,
        "max_rounds": max_rounds,
    }));

    let mut rounds_used: u32 = 0;
    let mut total_prompt: u64 = 0;
    let mut total_completion: u64 = 0;
    let mut tool_call_count: u32 = 0;

    let (finish_reason, error_class, failure_reason) = loop {
        if max_rounds > 0 && rounds_used >= max_rounds {
            break (FinishReason::MaxRounds, ErrorClass::None, None);
        }
        rounds_used += 1;
        jsonl.append(&json!({"record":"round_start","round_idx": rounds_used}));

        let r = provider
            .tool_round(&resolved.conversation, &resolved.tools, cancel.clone())
            .await;
        match r {
            Ok(RoundResult::Final { text, assistant, prompt_tokens, completion_tokens }) => {
                total_prompt += prompt_tokens.unwrap_or(0);
                total_completion += completion_tokens.unwrap_or(0);
                jsonl.append(&json!({
                    "record":"assistant_message",
                    "round_idx": rounds_used,
                    "text_excerpt": excerpt(&text, 2000),
                    "tool_call_count": 0,
                }));
                resolved.conversation.turns.push(Turn::Assistant(assistant));
                break (FinishReason::Stop, ErrorClass::None, None);
            }
            Ok(RoundResult::LengthCap { partial_text, assistant, prompt_tokens, completion_tokens }) => {
                total_prompt += prompt_tokens.unwrap_or(0);
                total_completion += completion_tokens.unwrap_or(0);
                jsonl.append(&json!({
                    "record":"assistant_message",
                    "round_idx": rounds_used,
                    "text_excerpt": excerpt(partial_text.as_deref().unwrap_or(""), 2000),
                    "length_cap": true,
                }));
                resolved.conversation.turns.push(Turn::Assistant(assistant));
                break (FinishReason::Length, ErrorClass::None, None);
            }
            Ok(RoundResult::ToolCalls { calls, assistant, prompt_tokens, completion_tokens }) => {
                total_prompt += prompt_tokens.unwrap_or(0);
                total_completion += completion_tokens.unwrap_or(0);
                resolved.conversation.turns.push(Turn::Assistant(assistant.clone()));
                let mut fatal: Option<String> = None;
                for call in calls {
                    tool_call_count += 1;
                    let canonical = resolved
                        .reverse_map
                        .get(&call.tool_name)
                        .cloned()
                        .unwrap_or_else(|| call.tool_name.clone());
                    jsonl.append(&json!({
                        "record":"tool_call",
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
                        workspace_root.as_deref(),
                        &ctx,
                        model_id,
                    )
                    .await;
                    let dur = duration_ms(dispatch_started.elapsed());
                    let (status, content): (ToolResultStatus, serde_json::Value) = match result {
                        DispatchOne::Ok(v) => (ToolResultStatus::Ok, v),
                        DispatchOne::Recoverable(msg) => {
                            (ToolResultStatus::Error, json!({"error": msg}))
                        }
                        DispatchOne::Fatal(msg) => {
                            fatal = Some(msg.clone());
                            (ToolResultStatus::Error, json!({"error": msg}))
                        }
                        DispatchOne::Unknown => (
                            ToolResultStatus::Error,
                            json!({"error":"unknown_tool", "tool_name": canonical}),
                        ),
                    };
                    jsonl.append(&json!({
                        "record":"tool_result",
                        "round_idx": rounds_used,
                        "call_id": call.call_id,
                        "status": status,
                        "duration_ms": dur,
                    }));
                    resolved.conversation.turns.push(Turn::ToolResult(ToolResultTurn {
                        call_id: call.call_id.clone(),
                        status,
                        content,
                    }));
                    if let Some(f) = fatal.clone() {
                        return Ok(HarnessOutcome {
                            kind: classify_outcome(
                                FinishReason::ToolCalls,
                                ErrorClass::ToolDispatchFatal,
                                rounds_used,
                                max_rounds,
                            ),
                            finish_reason: FinishReason::ToolCalls,
                            error_class: ErrorClass::ToolDispatchFatal,
                            failure_reason: Some(f),
                            rounds_used,
                            duration_ms: duration_ms(started.elapsed()),
                            total_prompt_tokens: Some(total_prompt),
                            total_completion_tokens: Some(total_completion),
                            tool_call_count,
                            jsonl_bytes: jsonl.snapshot().bytes,
                            jsonl_truncated: jsonl.truncated(),
                        });
                    }
                }
            }
            Err(e) => {
                let (class, msg) = error_class_for(&e);
                jsonl.append(&json!({
                    "record":"provider_error",
                    "round_idx": rounds_used,
                    "class": format!("{class:?}"),
                    "message": msg,
                }));
                break (FinishReason::Stop, class, Some(msg));
            }
        }
    };

    let dur = duration_ms(started.elapsed());
    let kind = classify_outcome(finish_reason, error_class, rounds_used, max_rounds);
    jsonl.append(&json!({
        "record":"finish",
        "outcome_kind": format!("{kind:?}"),
        "failure_reason": failure_reason,
        "rounds_used": rounds_used,
        "total_prompt_tokens": total_prompt,
        "total_completion_tokens": total_completion,
        "total_duration_ms": dur,
    }));

    let snap = jsonl.snapshot();
    Ok(HarnessOutcome {
        kind,
        finish_reason,
        error_class,
        failure_reason,
        rounds_used,
        duration_ms: dur,
        total_prompt_tokens: Some(total_prompt),
        total_completion_tokens: Some(total_completion),
        tool_call_count,
        jsonl_bytes: snap.bytes,
        jsonl_truncated: snap.truncated,
    })
}

enum DispatchOne {
    Ok(serde_json::Value),
    Recoverable(String),
    Fatal(String),
    Unknown,
}

async fn dispatch_one(
    loop_: &HarnessLoop,
    resolved: &ResolvedProgram,
    canonical: &str,
    args: serde_json::Value,
    workspace_root: Option<&std::path::Path>,
    ctx: &HarnessContext,
    model_id: &str,
) -> DispatchOne {
    match resolved.bindings.get(canonical) {
        Some(ToolBinding::Substrate(b)) => {
            use crate::tools::substrate_dispatch::{SubstrateDispatchResult, dispatch};
            match dispatch(&loop_.substrate_bridge, b, args, ctx, model_id).await {
                SubstrateDispatchResult::Ok(v) => DispatchOne::Ok(v),
                SubstrateDispatchResult::Recoverable(m) => DispatchOne::Recoverable(m),
                SubstrateDispatchResult::Fatal(m) => DispatchOne::Fatal(m),
            }
        }
        Some(ToolBinding::Workspace(name)) => {
            let root = match workspace_root {
                Some(r) => r.to_path_buf(),
                None => {
                    return DispatchOne::Recoverable(
                        "workspace tool called in non-workspace wake".into(),
                    );
                }
            };
            match workspace_dispatch(*name, args, &WorkspaceCtx { workspace_root: root }).await {
                Ok(v) => DispatchOne::Ok(v),
                Err(e) => DispatchOne::Recoverable(e.to_string()),
            }
        }
        None => DispatchOne::Unknown,
    }
}

fn error_class_for(e: &ProviderError) -> (ErrorClass, String) {
    match e {
        ProviderError::Auth => (ErrorClass::Auth, "auth".into()),
        ProviderError::RateLimited { .. } => (ErrorClass::RateLimited, "rate_limited".into()),
        ProviderError::ContextLength => (ErrorClass::ContextLength, "context_length".into()),
        ProviderError::InvalidRequest(s) => (ErrorClass::InvalidRequest, s.clone()),
        ProviderError::ServerError(s) => (ErrorClass::ServerError, s.clone()),
        ProviderError::Network(s) => (ErrorClass::Network, s.clone()),
        ProviderError::Timeout => (ErrorClass::Timeout, "timeout".into()),
        ProviderError::Deserialize(s) => (ErrorClass::Deserialize, s.clone()),
    }
}

fn excerpt(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max])
    }
}
```

The `run` method clones `program.provider` before calling `resolve(program)` because `ResolvedProgram` drops the provider config after the conversation is materialized — the driver needs the typed `ProviderTarget` to construct the `ProviderClient` (`MistralChatClient`, `OpenAIChatClient`, `OpenAIResponsesClient`, etc.).

- [ ] **Step 2: Re-export from lib.rs**

Update `crates/harness/src/lib.rs`:

```rust
#![forbid(unsafe_code)]

pub mod conversation;
pub mod loop_driver;
pub mod program;
pub mod providers;
pub mod tools;
pub mod trace;

pub use loop_driver::HarnessLoop;

// Re-export trait + program types so callers depend only on
// proxima-harness for typing (they still need proxima-core types
// for HarnessAdapter, HarnessProgram, etc.).
pub use proxima_core::harness::{
    HarnessAdapter, HarnessContext, HarnessError, HarnessOutcome, HarnessOutcomeKind,
    HarnessProgram, ProviderTarget, SubstrateToolBinding,
};
```

- [ ] **Step 3: Build**

Run: `cargo build -p proxima-harness`
Expected: builds clean.

- [ ] **Step 4: Write a stub-provider integration test**

Create `crates/harness/tests/loop_driver.rs`. The test uses an in-test `ProviderClient` impl that returns scripted `RoundResult`s.

```rust
//! Loop driver integration test: stub provider drives the loop
//! through one tool call and a final stop.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use proxima_harness::conversation::{AssistantTurn, Conversation, ToolCall, ToolSpec};
use proxima_harness::providers::{ProviderClient, ProviderError, RoundResult};
use serde_json::json;
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct StubProvider {
    round: AtomicUsize,
}

#[async_trait]
impl ProviderClient for StubProvider {
    async fn tool_round(
        &self,
        _conversation: &Conversation,
        _tools: &[ToolSpec],
        _cancel: CancellationToken,
    ) -> Result<RoundResult, ProviderError> {
        let r = self.round.fetch_add(1, Ordering::SeqCst);
        Ok(match r {
            0 => RoundResult::ToolCalls {
                calls: vec![ToolCall {
                    call_id: "call_0".into(),
                    tool_name: "workspace_list_files".into(),
                    arguments: json!({"path":".","recursive":false}),
                }],
                assistant: AssistantTurn::default(),
                prompt_tokens: Some(10),
                completion_tokens: Some(5),
            },
            _ => RoundResult::Final {
                text: "Done.".into(),
                assistant: AssistantTurn {
                    text: "Done.".into(),
                    ..Default::default()
                },
                prompt_tokens: Some(8),
                completion_tokens: Some(3),
            },
        })
    }
}

// Note: full driver wiring (HarnessLoop::new requires Engine) is
// exercised in the end-to-end test in Phase 8. This test pokes the
// provider+conversation surface alone.

#[tokio::test]
async fn stub_provider_returns_two_rounds() {
    let p = StubProvider::default();
    let conv = Conversation {
        system_prompt: "test".into(),
        user_seed: "go".into(),
        turns: vec![],
    };
    let tools: Vec<ToolSpec> = vec![];
    let r1 = p
        .tool_round(&conv, &tools, CancellationToken::new())
        .await
        .unwrap();
    assert!(matches!(r1, RoundResult::ToolCalls { .. }));
    let r2 = p
        .tool_round(&conv, &tools, CancellationToken::new())
        .await
        .unwrap();
    assert!(matches!(r2, RoundResult::Final { .. }));
}
```

Run: `cargo test -p proxima-harness --test loop_driver`
Expected: passes.

- [ ] **Step 5: Commit**

```bash
git add crates/harness/src/lib.rs crates/harness/src/loop_driver.rs crates/harness/tests/loop_driver.rs
git commit -m "harness: HarnessLoop driver with multi-round dispatch + reverse-map"
```
