# Task 8.5 — Rewire `fire_wake_entry`

> Part of [Proxima Harness Implementation Plan](README.md). Subagent execution: implement steps in order, commit at the end of the task.

**Scope.** Replace the recipe/effective-recipe path with a `HarnessProgram` build, dispatch via `adapter.run(program, ctx)`, **preserve the workspace prepare → run → finalize semantics** (current shape at `crates/core/src/wake/fire/fire.rs:303-463` — `runner.prepare` produces `WorkspacePreparedRun { work_dir, workspace_context, effective_recipe_path, runner_state }`; the dispatcher runs the adapter with `cwd = prepared.work_dir`, then calls `runner.finalize(WorkspaceFinalizeInput { prepared, outcome, .. })`), then emit the wake-trace Fact via the dedicated `engine.persist_wake_trace_internal(...)` verb.

Do **not** drop the `runner.finalize` call — it materializes the workspace-mode primary Fact (e.g. the execution-request result memory) and creates the `core/authored` / `core/derived-from` edges for it. That work is independent of the wake-trace Fact this task adds.

**Files:**
- Modify: `crates/core/src/wake/fire/fire.rs`
- Create: `crates/core/src/wake/trace/emit.rs` (and `wake/trace/mod.rs` if not present)

- [ ] **Step 1: Substrate-mode dispatch rewrite**

The current `fire_wake_entry` (`crates/core/src/wake/fire/fire.rs:67-299`) does, in order: assemble context → resolve target + recipe path → read recipe bytes + sha256 → mint wake token → `start_wake_invocation` → build params + env → `write_effective_recipe` → workspace-mode branch OR `adapter.run(TargetInvocation { recipe_path, ... })` → finalize.

Substrate-mode rewrite (around the existing step 9). Drop the recipe-path / recipe-bytes / effective-recipe steps; replace with the harness call:

```rust
// `wake_context` from existing step 1, `wake_token` from step 4,
// `invocation_id_for_dispatch` from step 4, `inserted` from step 5.
//
// After wake_token + start_wake_invocation have succeeded, do not use
// `?` for preflight work. Every error on this side of the boundary must
// revoke the token and finalize the invocation.

let started_at = time::OffsetDateTime::now_utc();
let provider_target = match build_provider_target(&resolved, engine).await {
    Ok(target) => target,
    Err(err) => {
        let finished_at = time::OffsetDateTime::now_utc();
        let failure_reason = provider_target_failure_reason(&err);
        finalize_failed_started_wake(
            engine,
            &input,
            &wake_context,
            &resolved,
            invocation_id_for_dispatch,
            wake_token,
            started_at,
            finished_at,
            failure_reason,
        )
        .await?;
        return Ok(true);
    }
};
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

let program = crate::harness::HarnessProgram {
    system_prompt: wake_context.root_perspective.system_prompt.clone(),
    instructions: input.wake_entry.instructions.clone(),
    context_params,
    substrate_tool_palette: input.wake_entry.substrate_tool_palette.clone(),
    workspace_root: None,                    // workspace-mode branch sets this
    max_rounds: u32::from(input.wake_entry.max_rounds),
    provider: provider_target,
};
let hctx = crate::harness::HarnessContext {
    owner: input.owner.clone(),
    invocation_id: invocation_id_for_dispatch,
    wake_entry_id: input.wake_entry.wake_entry_id,
    personality_instance_id: input.personality_instance_id,
    change_event_seq: input.change_event_seq,
    root_perspective_memory_id: MemoryId::new(wake_context.root_perspective.memory_id),
    wake_token,
    invocation_timeout,
};

let outcome_result = adapter.run(program, hctx).await;
let finished_at = time::OffsetDateTime::now_utc();

engine.wake_token_store().revoke(wake_token).await;
append_session_log_error_if_present(engine, &input, &outcome_result).await;

// Emit wake-trace Fact BEFORE wake-invocation finalize. The trace
// captures the run's outcome regardless of finalize outcome.
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
.ok(); // non-blocking; failure is logged inside emit_trace_from_outcome

let outcome = match outcome_result {
    Ok(outcome) => wake_outcome_from_harness_outcome(&input, outcome),
    Err(e) => WakeInvocationFinalizeOutcome::failed(format!("harness_error: {e}")),
};
finalize(engine, &input, outcome).await?;
Ok(true)
```

`build_provider_target(&resolved, engine)` reads `resolved.config_kind` and the credential env var (`std::env::var(&cfg.api_key_env)`). If the env var is absent, it returns an error that `provider_target_failure_reason` maps to exactly `credentials_missing:{ENV_NAME}`. This error is a failed wake outcome, not a dispatch error: the wake token has already been minted and the invocation row has already been started, so the caller must revoke the token, emit the failed wake trace, and finalize with `WakeInvocationFinalizeOutcome::failed("credentials_missing:...")`.

Do not resolve substrate tools in `fire_wake_entry`. Pass the palette ids through `HarnessProgram.substrate_tool_palette`; `HarnessLoop` resolves them through the injected `HarnessSubstrateBridge`, which sees both registry MCP tools and the personality substrate pack.

`wake_outcome_from_harness_outcome` is the renamed `wake_outcome_from_target_result` from `crates/core/src/wake/fire/outcome.rs`, adapted to consume `HarnessOutcome` (rounds_used, total_*_tokens, jsonl_truncated, finish_reason → status, failure_reason).

- [ ] **Step 2: Workspace-mode dispatch rewrite**

Workspace-mode replaces the current `handle_workspace_mode` body (`crates/core/src/wake/fire/fire.rs:303-463`). Keep the structure: prepare → run → finalize → finalize-wake-invocation. The middle "run" step swaps from `adapter.run(TargetInvocation { recipe_path, cwd, ... })` to `adapter.run(HarnessProgram { workspace_root: Some(prepared.work_dir), ... })`.

```rust
async fn handle_workspace_mode(
    engine: &Engine,
    adapter: &dyn HarnessAdapter,
    input: FireWakeEntryInput,
    wake_token: Uuid,
    wake_context: crate::wake::context::WakeContext,
    resolved: ResolvedInferenceTarget,
    mut context_params: HashMap<String, serde_json::Value>,
    invocation_id_for_dispatch: Uuid,
    invocation_timeout: Duration,
) -> Result<bool, ProtocolError> {
    let flavor_id = input.wake_entry.trigger_id.split('/').next().unwrap_or("");
    let runner_opt = engine.registry().workspace_runner(flavor_id);
    let Some(runner) = runner_opt else {
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

    if !engine.registry().is_workspace_trigger(&input.wake_entry.trigger_id) {
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

    // The harness no longer needs a recipe file — pass an empty path
    // through `WorkspacePrepareInput`. Runners that currently consume
    // `recipe_bytes` / `recipe_sha256` for fingerprinting should switch
    // to `harness_program_fingerprint` (out of scope here; runners can
    // accept empty `recipe_bytes` and a fixed sha256 until they're
    // updated separately).
    let empty_path = std::path::PathBuf::new();
    let mcp_url = engine.mcp_url().ok_or_else(|| {
        ProtocolError::internal("engine.mcp_url() must be set before firing wakes")
    })?;
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
        effective_recipe_path: &empty_path,
        recipe_bytes: &[],
        recipe_sha256: "",
    };

    let prepared = match runner.prepare(prepare_input).await {
        Ok(p) => p,
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

    // The runner-owned context payload merges into the harness params
    // under the spec-fixed key. Field is `workspace_context` (Option<Value>),
    // see crates/core/src/personality/workspace.rs:80.
    if let Some(ws_ctx) = prepared.workspace_context.clone() {
        context_params.insert("workspace_context".into(), ws_ctx);
    }

    let started_at = time::OffsetDateTime::now_utc();
    let provider_target = match build_provider_target(&resolved, engine).await {
        Ok(target) => target,
        Err(err) => {
            let finished_at = time::OffsetDateTime::now_utc();
            let failure_reason = provider_target_failure_reason(&err);
            let finalize_outcome = WakeInvocationFinalizeOutcome::failed(failure_reason.clone());
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
    let program = crate::harness::HarnessProgram {
        system_prompt: wake_context.root_perspective.system_prompt.clone(),
        instructions: input.wake_entry.instructions.clone(),
        context_params,
        substrate_tool_palette: input.wake_entry.substrate_tool_palette.clone(),
        workspace_root: Some(prepared.work_dir.clone()),
        max_rounds: u32::from(input.wake_entry.max_rounds),
        provider: provider_target,
    };
    let hctx = crate::harness::HarnessContext {
        owner: input.owner.clone(),
        invocation_id: invocation_id_for_dispatch,
        wake_entry_id: input.wake_entry.wake_entry_id,
        personality_instance_id: input.personality_instance_id,
        change_event_seq: input.change_event_seq,
        root_perspective_memory_id: MemoryId::new(wake_context.root_perspective.memory_id),
        wake_token,
        invocation_timeout,
    };

    let outcome_result = adapter.run(program, hctx).await;
    let finished_at = time::OffsetDateTime::now_utc();

    append_session_log_error_if_present(engine, &input, &outcome_result).await;

    // Emit wake-trace Fact for the workspace-mode run (same path as
    // substrate mode). Done before runner.finalize so the trace exists
    // even if finalize fails.
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

    // Build WorkspaceOutcome from the harness result for runner.finalize.
    let finalize_outcome = match &outcome_result {
        Ok(o) => wake_outcome_from_harness_outcome(&input, o.clone()),
        Err(e) => WakeInvocationFinalizeOutcome::failed(format!("harness_error: {e}")),
    };
    let workspace_outcome = WorkspaceOutcome {
        exit_code: finalize_outcome.exit_code,
        stdout_tail: finalize_outcome.stdout_tail.clone(),
        stderr_tail: finalize_outcome.stderr_tail.clone(),
        duration_ms: finalize_outcome.duration_ms,
    };

    let authored_relation = engine
        .registry()
        .resolve_relation(crate::CORE_AUTHORED_RELATION)
        .ok_or_else(|| ProtocolError::internal("missing core/authored relation"))?;
    let derived_from_relation = engine
        .registry()
        .resolve_relation(crate::CORE_DERIVED_FROM_RELATION)
        .ok_or_else(|| ProtocolError::internal("missing core/derived-from relation"))?;

    // The runner materializes the workspace-mode primary Fact + its
    // own core/authored + core/derived-from edges via WorkspaceFinalizeInput.
    // This is independent of the wake-trace Fact above — do not drop it.
    let finalized = runner
        .finalize(WorkspaceFinalizeInput {
            owner: &input.owner,
            invocation_id: invocation_id_for_dispatch,
            root_perspective_memory_id: MemoryId::new(wake_context.root_perspective.memory_id),
            triggering_memory_id: MemoryId::new(wake_context.triggering_memory.memory_id),
            authored_relation,
            derived_from_relation,
            prepared,
            outcome: workspace_outcome,
        })
        .await;

    engine.wake_token_store().revoke(wake_token).await;

    let outcome = match finalized {
        Ok(_) => finalize_outcome,
        Err(err) => {
            WakeInvocationFinalizeOutcome::failed(format!("workspace_runner_finalize:{err}"))
        }
    };
    finalize(engine, &input, outcome).await?;
    Ok(true)
}
```

The `runner.finalize` call is load-bearing — it writes the workspace-mode primary memory and its provenance edges. Removing it would silently break execution-worker wakes (the Code flavor's only workspace-mode personality).

- [ ] **Step 3: Pre-run failure helpers**

Add helpers near the fire path. These are used only after `wake_token` minting and `start_wake_invocation` have succeeded. They must never propagate a pre-run failure with `?` before revoking the token and finalizing the invocation.

```rust
async fn finalize_failed_started_wake(
    engine: &Engine,
    input: &FireWakeEntryInput,
    wake_context: &WakeContext,
    resolved: &ResolvedInferenceTarget,
    invocation_id: uuid::Uuid,
    wake_token: uuid::Uuid,
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

async fn finalize_pre_run_workspace(
    engine: &Engine,
    input: &FireWakeEntryInput,
    wake_context: &WakeContext,
    resolved: &ResolvedInferenceTarget,
    invocation_id: uuid::Uuid,
    wake_token: uuid::Uuid,
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

    // prepare succeeded, so give the runner one finalize call with a
    // failed WorkspaceOutcome. This preserves workspace cleanup and the
    // workspace-mode primary failure Fact when the runner supports it.
    let workspace_outcome = WorkspaceOutcome {
        exit_code: finalize_outcome.exit_code,
        stdout_tail: finalize_outcome.stdout_tail.clone(),
        stderr_tail: finalize_outcome.stderr_tail.clone(),
        duration_ms: finalize_outcome.duration_ms,
    };
    let authored_relation = engine.registry().resolve_relation(crate::CORE_AUTHORED_RELATION);
    let derived_from_relation = engine.registry().resolve_relation(crate::CORE_DERIVED_FROM_RELATION);
    if let (Some(authored_relation), Some(derived_from_relation)) =
        (authored_relation, derived_from_relation)
    {
        let _ = engine
            .registry()
            .workspace_runner(input.wake_entry.trigger_id.split('/').next().unwrap_or(""))
            .expect("runner existed before prepare")
            .finalize(WorkspaceFinalizeInput {
                owner: &input.owner,
                invocation_id,
                root_perspective_memory_id: MemoryId::new(wake_context.root_perspective.memory_id),
                triggering_memory_id: MemoryId::new(wake_context.triggering_memory.memory_id),
                authored_relation,
                derived_from_relation,
                prepared,
                outcome: workspace_outcome,
            })
            .await;
    }

    engine.wake_token_store().revoke(wake_token).await;
    finalize(engine, input, finalize_outcome).await
}

fn provider_target_failure_reason(err: &ProviderTargetBuildError) -> String {
    match err {
        ProviderTargetBuildError::MissingCredentials { env } => {
            format!("credentials_missing:{env}")
        }
        other => format!("provider_target:{other}"),
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
```

Load-bearing rule: no `?` between `start_wake_invocation` and token revocation/finalization for errors that should be represented as a wake outcome. Missing credentials are the main regression guard: they must finalize as `Failed("credentials_missing:{ENV}")`, not leave the invocation started.

- [ ] **Step 4: `emit_trace_from_outcome` helper**

Place `emit_trace_from_outcome` in `crates/core/src/wake/trace/emit.rs` (new file; add `pub mod emit;` to `crates/core/src/wake/trace/mod.rs` if not present). It's the pure mapping from `HarnessOutcome` → `WakeTracePersistInput`, with explicit handling of the `Err` branch so failed adapter calls still record a trace.

```rust
//! Build a `WakeTracePersistInput` from a finished harness invocation
//! and call `engine.persist_wake_trace_internal` (the dispatcher's
//! crate-private bypass of the auth_resolver round-trip — the wake
//! token store already authorised this dispatcher).

// This file lives at `crates/core/src/wake/trace/emit.rs` — i.e.
// inside the `proxima-core` crate. Reach core's own modules via
// `crate::...`; `proxima_core::...` would only resolve if the crate
// root declared `extern crate self as proxima_core;`, which it does
// not (verified: zero `use proxima_core::` lines anywhere under
// `crates/core/src`).
use crate::harness::{HarnessError, HarnessOutcome};
use crate::verbs::persist_wake_trace::{
    WakeTracePersistInput, WakeTracePersistOutcome,
};
use crate::wake::trace::WakeTracePayload;
use crate::{Engine, GoalId, MemoryId, SourceBatchId, SourceId, StorageError};

use crate::wake::fire::input::FireWakeEntryInput;
use crate::wake::fire::resolve::ResolvedInferenceTarget;
use crate::wake::context::WakeContext;

pub async fn emit_trace_from_outcome(
    engine: &Engine,
    input: &FireWakeEntryInput,
    wake_context: &WakeContext,
    resolved: &ResolvedInferenceTarget,
    invocation_id: uuid::Uuid,
    outcome_result: &Result<HarnessOutcome, HarnessError>,
    started_at: time::OffsetDateTime,
    finished_at: time::OffsetDateTime,
) -> Result<WakeTracePersistOutcome, StorageError> {
    let (jsonl_bytes, jsonl_truncated, outcome_kind, failure_reason,
         rounds_used, finish_reason_str, prompt_toks, completion_toks,
         tool_call_count) = match outcome_result {
        Ok(o) => (
            o.jsonl_bytes.clone(),
            o.jsonl_truncated,
            harness_outcome_kind_str(o.kind).to_string(),
            o.failure_reason.clone(),
            o.rounds_used,
            Some(harness_finish_reason_str(o.finish_reason).to_string()),
            o.total_prompt_tokens,
            o.total_completion_tokens,
            o.tool_call_count,
        ),
        Err(err) => (
            Vec::new(),
            false,
            "failed".to_string(),
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

    // Active goals at wake time — IDs only. WakeContext.active_goals
    // is Vec<ActiveGoalEnvelope>; .goal_id is a Uuid (the Goal entity id).
    let active_goal_ids: Vec<GoalId> = wake_context
        .active_goals
        .iter()
        .map(|g| GoalId::new(g.goal_id))
        .collect();

    let wake_trace = WakeTracePayload {
        invocation_id,
        wake_entry_id: input.wake_entry.wake_entry_id,
        personality_instance_id: input.personality_instance_id.into_inner(),
        model_target_ref: resolved.target_ref.clone(),
        model_id: resolved.config_model_id.clone().unwrap_or_default(),
        started_at,
        finished_at,
        outcome_kind,
        failure_reason,
        rounds_used,
        finish_reason: finish_reason_str,
        total_prompt_tokens: prompt_toks,
        total_completion_tokens: completion_toks,
        tool_call_count,
        jsonl_truncated,
    };

    let persist = WakeTracePersistInput {
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
        source_id: SourceId::new("core/wake-trace".into()),
        source_batch_id: SourceBatchId::new(uuid::Uuid::now_v7()),
        observed_at: time::OffsetDateTime::now_utc(),
        occurred_at: finished_at,
    };

    let r = engine.persist_wake_trace_internal(persist).await;
    if let Err(ref e) = r {
        tracing::warn!(
            invocation_id = %invocation_id,
            error = %e,
            "persist_wake_trace failed — wake-trace Fact not written",
        );
    }
    r
}

fn harness_outcome_kind_str(k: crate::harness::HarnessOutcomeKind) -> &'static str {
    use crate::harness::HarnessOutcomeKind::*;
    match k { Succeeded => "succeeded", Truncated => "truncated", Failed => "failed" }
}

fn harness_finish_reason_str(f: crate::harness::FinishReason) -> &'static str {
    use crate::harness::FinishReason::*;
    match f {
        Stop => "stop",
        ToolCalls => "tool_calls",
        Length => "length",
        MaxRounds => "max_rounds",
        Unknown(s) => s,
    }
}
```

`Engine::persist_wake_trace_internal` is the crate-private path added in Task 7.4 step 2 — it bypasses `auth_resolver` because the dispatcher has already authorised the wake.

Also add `emit_trace_from_failed_preflight` in the same module. It uses the same `WakeTracePersistInput` construction as `emit_trace_from_outcome`, but with empty JSONL, `outcome_kind = "failed"`, `rounds_used = 0`, no finish reason, no token counts, and `failure_reason = Some(failure_reason)`. This is the trace path for failures that happen after invocation start but before `adapter.run`, including `credentials_missing:{ENV}`.

- [ ] **Step 5: Build**

Run: `cargo build -p proxima-core`
Expected: clean. Other crates that pin the old `TargetAdapter` trait name will be unblocked by Task 8.4 (the adapter trait alias).

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/wake/fire/fire.rs crates/core/src/wake/trace/emit.rs \
        crates/core/src/wake/trace/mod.rs
git commit -m "$(cat <<'EOF'
core(wake/fire): swap goose recipe path for HarnessAdapter; emit wake-trace Fact

Substrate and workspace branches both build a HarnessProgram and call
adapter.run(program, ctx). Workspace branch preserves runner.prepare
→ harness run with cwd = prepared.work_dir → runner.finalize so the
execution-request primary Fact and its provenance edges still land.
The wake-trace Fact is emitted via engine.persist_wake_trace_internal
in both branches, before wake-invocation finalize, so failed runs
still produce an audit Fact.
EOF
)"
```
