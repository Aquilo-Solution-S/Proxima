//! Phase 1d Task 8: per-entry wake fire path.
//!
//! `fire_wake_entry` mints a wake token, snapshots the resolved
//! inference target + recipe SHA-256 onto the invocation row, drives
//! the [`TargetAdapter`], and finalizes status. Workspace mode
//! dispatches to the flavor's registered `WorkspaceRunner`; the
//! Code-flavor Phase-1 stub returns `WorkspaceRunnerError::Unimplemented`
//! so the legacy `failure_reason = workspace_mode_not_yet_implemented`
//! string is preserved. Self-wake is a defense-in-depth `Ok(false)`
//! (the dispatcher's `authored_by` filter is the primary guard).
//!
//! The four wake-context envelopes flow through unchanged: every
//! WakeEntry on every Personality gets the same four fixed params,
//! per spec docs/superpowers/specs/2026-05-07 lines 285–306.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::MemoryId;
use crate::engine::Engine;
use crate::error::ProtocolError;
use crate::personality::workspace::{
    WorkspaceFinalizeInput, WorkspaceOutcome, WorkspacePrepareInput, WorkspaceRunnerError,
};
use crate::personality::{
    PersonalityInstanceId, WakeChainDepth, WakeEntryExecutionMode, WakeInvocationStart,
    WakeInvocationStatus,
};
use crate::wake::context::assemble_wake_context;
use crate::wake::target_adapter::{TargetAdapter, TargetInvocation};
use crate::wake::token_store::WakeTokenContext;

use super::finalize::{
    append_session_artifact_log, append_session_log_error_if_present, finalize,
    wake_session_log_path,
};
use super::input::{FireWakeEntryInput, per_invocation_timeout};
use super::outcome::{wake_outcome_from_target_result, WakeInvocationFinalizeOutcome};
use super::recipe::write_effective_recipe;
use super::resolve::{collect_sidecars, resolve_recipe_path, resolve_target};

/// Drive one matched wake entry end-to-end.
///
/// Returns `Ok(true)` when the invocation row was written (whether it
/// succeeded, was truncated, or failed), and `Ok(false)` when the fire
/// was skipped (self-wake guard). Returns `Err` only for plumbing
/// failures that prevent us from writing an invocation row at all
/// (target missing, recipe unresolvable, MCP URL absent).
///
/// # Errors
///
/// - `ProtocolError::not_found` when the triggering memory or change
///   event has been pruned out from under us.
/// - `ProtocolError::tier_unbound` when the wake entry's `model_tier`
///   has no binding and no explicit `inference_target_ref`.
/// - `ProtocolError::inference_target_missing` when the chosen target
///   is not registered.
/// - `ProtocolError::recipe_not_found` (via `recipe_resolve`) when the
///   recipe ref does not point at a real file.
/// - `ProtocolError::internal` when the storage trait, recipe-read, or
///   MCP-URL slot fails.
pub async fn fire_wake_entry(
    engine: &Engine,
    adapter: &dyn TargetAdapter,
    input: FireWakeEntryInput,
) -> Result<bool, ProtocolError> {
    // 0. Self-wake guard. The dispatcher's `authored_by` filter is the
    // primary defense; this is belt-and-braces so a misconfigured
    // entry can't fan out into a self-wake loop. Read the change event
    // we'd be acting on and bail if its author is the personality we
    // would otherwise wake.
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

    // 1. Assemble four-param context. Sidecars list comes from the
    // engine's frozen registry — recipes match by `schema_id` to
    // populate the triggering memory's typed_payload.
    let sidecars = collect_sidecars(engine);
    let wake_context = assemble_wake_context(
        engine.storage().as_ref(),
        &input.owner,
        input.personality_instance_id,
        input.change_event_seq,
        &sidecars,
    )
    .await?;

    // 2. Resolve target and recipe path. Both fail loud — without
    // either we can't write a meaningful invocation row.
    let resolved = resolve_target(engine, &input).await?;
    let recipe_path = resolve_recipe_path(engine, &input)?;

    // 3. Compute recipe sha256 from the resolved bytes (Phase 1d
    // §change_event_seq column 7).
    let recipe_bytes = tokio::fs::read(&recipe_path)
        .await
        .map_err(|e| ProtocolError::internal(format!("read recipe: {e}")))?;
    let mut hasher = Sha256::new();
    hasher.update(&recipe_bytes);
    let recipe_sha256 = hex::encode(hasher.finalize());

    // 4. Mint wake token. Held in WakeTokenStore; the MCP listener's
    // auth layer resolves it back to the WakeTokenContext on each
    // tool call.
    let invocation_id_for_dispatch = Uuid::new_v4();
    let token_ctx = WakeTokenContext {
        invocation_id: invocation_id_for_dispatch,
        personality_instance_id: input.personality_instance_id.into_inner(),
        wake_entry_id: input.wake_entry.wake_entry_id,
        change_event_seq: input.change_event_seq,
        owner: input.owner.clone(),
        palette: input.wake_entry.substrate_tool_palette.clone(),
        model_id: resolved.config_model_id.clone().unwrap_or_default(),
        max_rounds: u32::from(input.wake_entry.max_rounds),
        current_root_perspective_memory_id: MemoryId::new(
            wake_context.root_perspective.memory_id,
        ),
        triggering_event_memory_id: MemoryId::new(wake_context.triggering_memory.memory_id),
        triggering_event_depth: WakeChainDepth::new(
            u16::try_from(wake_context.trigger_event.wake_chain_depth).unwrap_or(0),
        ),
        read_log: Arc::new(tokio::sync::Mutex::new(Vec::new())),
    };
    let root_perspective_memory_id_for_dispatch =
        MemoryId::new(wake_context.root_perspective.memory_id);
    let triggering_memory_id_for_dispatch = MemoryId::new(wake_context.triggering_memory.memory_id);
    let max_rounds = u32::from(input.wake_entry.max_rounds);
    let invocation_timeout = per_invocation_timeout(max_rounds);
    let wake_token = engine
        .wake_token_store()
        .mint_with_max_lifetime(token_ctx, invocation_timeout)
        .await;

    // 5. INSERT invocation row (status = running).
    let inserted = engine
        .storage()
        .start_wake_invocation(&WakeInvocationStart {
            owner: input.owner.clone(),
            personality_instance_id: input.personality_instance_id,
            wake_entry_id: input.wake_entry.wake_entry_id,
            change_event_seq: input.change_event_seq,
            wake_token,
            recipe_sha256: recipe_sha256.clone(),
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

    // 6. Build the four params as JSON values for goose --params.
    let mut params: HashMap<String, serde_json::Value> = HashMap::new();
    params.insert(
        "root_perspective".to_string(),
        serde_json::to_value(&wake_context.root_perspective)
            .map_err(|e| ProtocolError::internal(format!("serialize root_perspective: {e}")))?,
    );
    params.insert(
        "active_goals".to_string(),
        serde_json::to_value(&wake_context.active_goals)
            .map_err(|e| ProtocolError::internal(format!("serialize active_goals: {e}")))?,
    );
    params.insert(
        "trigger_event".to_string(),
        serde_json::to_value(&wake_context.trigger_event)
            .map_err(|e| ProtocolError::internal(format!("serialize trigger_event: {e}")))?,
    );
    params.insert(
        "triggering_memory".to_string(),
        serde_json::to_value(&wake_context.triggering_memory)
            .map_err(|e| ProtocolError::internal(format!("serialize triggering_memory: {e}")))?,
    );

    // 7. Build env. PROXIMA_WAKE_TOKEN + PROXIMA_MCP_URL are the
    // always-injected pair the substrate authorization layer relies on.
    // Target-resolved overrides (e.g. GOOSE_PROFILE) layer on top.
    let mcp_url = engine.mcp_url().ok_or_else(|| {
        ProtocolError::internal(
            "engine.mcp_url() is None — call Engine::start (or set_mcp_url) before firing wakes",
        )
    })?;
    let mut env: HashMap<String, String> = HashMap::new();
    env.insert("PROXIMA_WAKE_TOKEN".to_string(), wake_token.to_string());
    env.insert("PROXIMA_MCP_URL".to_string(), mcp_url.clone());
    for (k, v) in &resolved.env_overrides {
        env.insert(k.clone(), v.clone());
    }

    let effective_recipe_path = write_effective_recipe(
        &recipe_bytes,
        &mcp_url,
        wake_token,
        &input.wake_entry.substrate_tool_palette,
        &input.wake_entry.workspace_tool_palette,
    )
    .await?;

    // 8. Workspace mode dispatch. Core resolves only flavor-generic
    // eligibility; the runner interprets the triggering payload.
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
            recipe_sha256,
            recipe_bytes,
            mcp_url,
            params,
            env,
            effective_recipe_path,
            invocation_id_for_dispatch,
            root_perspective_memory_id_for_dispatch,
            triggering_memory_id_for_dispatch,
            max_rounds,
            invocation_timeout,
            session_log_path,
        )
        .await;
    }

    // 9. Run adapter (non-workspace mode).
    let outcome_result = adapter
        .run(TargetInvocation {
            recipe_path: effective_recipe_path.clone(),
            params,
            max_rounds,
            env,
            timeout: invocation_timeout,
            enable_developer_builtin: false,
            cwd: None,
            session_log_path: Some(session_log_path),
            invocation_id: Some(invocation_id_for_dispatch),
            personality_instance_id: Some(input.personality_instance_id.into_inner()),
            wake_entry_id: Some(input.wake_entry.wake_entry_id),
            change_event_seq: Some(input.change_event_seq),
        })
        .await;
    let _ = tokio::fs::remove_file(&effective_recipe_path).await;

    // 10. Finalize: revoke token, write outcome.
    engine.wake_token_store().revoke(wake_token).await;
    append_session_log_error_if_present(engine, &input, &outcome_result).await;
    let outcome = match outcome_result {
        Ok(outcome) => wake_outcome_from_target_result(&input, Ok(outcome)),
        Err(e) => WakeInvocationFinalizeOutcome::failed(format!("adapter_error: {e}")),
    };
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
    finalize(engine, &input, outcome).await?;
    Ok(true)
}

/// Handle workspace mode dispatch.
async fn handle_workspace_mode(
    engine: &Engine,
    adapter: &dyn TargetAdapter,
    input: FireWakeEntryInput,
    wake_token: Uuid,
    wake_context: crate::wake::context::WakeContext,
    recipe_sha256: String,
    recipe_bytes: Vec<u8>,
    mcp_url: String,
    mut params: HashMap<String, serde_json::Value>,
    env: HashMap<String, String>,
    effective_recipe_path: PathBuf,
    invocation_id_for_dispatch: Uuid,
    root_perspective_memory_id_for_dispatch: MemoryId,
    triggering_memory_id_for_dispatch: MemoryId,
    max_rounds: u32,
    invocation_timeout: Duration,
    session_log_path: PathBuf,
) -> Result<bool, ProtocolError> {
    let flavor_id_for_dispatch = input.wake_entry.trigger_id.split('/').next().unwrap_or("");
    let runner_opt = engine.registry().workspace_runner(flavor_id_for_dispatch);
    let Some(runner) = runner_opt else {
        engine.wake_token_store().revoke(wake_token).await;
        let _ = tokio::fs::remove_file(&effective_recipe_path).await;
        finalize(
            engine,
            &input,
            WakeInvocationFinalizeOutcome::failed(format!(
                "workspace_no_runner_for_flavor:{flavor_id_for_dispatch}"
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
        let _ = tokio::fs::remove_file(&effective_recipe_path).await;
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

    let prepare_input = WorkspacePrepareInput {
        invocation_id: invocation_id_for_dispatch,
        owner: &input.owner,
        wake_token,
        mcp_url: &mcp_url,
        root_perspective_memory_id: root_perspective_memory_id_for_dispatch,
        triggering_memory_id: triggering_memory_id_for_dispatch,
        triggering_memory_schema_id: input.wake_entry.trigger_id.as_str(),
        triggering_memory_payload: &wake_context.triggering_memory.typed_payload,
        workspace_tool_palette: &input.wake_entry.workspace_tool_palette,
        effective_recipe_path: &effective_recipe_path,
        recipe_bytes: &recipe_bytes,
        recipe_sha256: &recipe_sha256,
    };

    let prepared = match runner.prepare(prepare_input).await {
        Ok(prepared) => prepared,
        Err(WorkspaceRunnerError::Unimplemented) => {
            engine.wake_token_store().revoke(wake_token).await;
            let _ = tokio::fs::remove_file(&effective_recipe_path).await;
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
            let _ = tokio::fs::remove_file(&effective_recipe_path).await;
            finalize(
                engine,
                &input,
                WakeInvocationFinalizeOutcome::failed(format!("workspace_runner_prepare:{err}")),
            )
            .await?;
            return Ok(true);
        }
    };

    if let Some(workspace_context) = prepared.workspace_context.clone() {
        params.insert("workspace_context".to_string(), workspace_context);
    }

    let outcome_result = adapter
        .run(TargetInvocation {
            recipe_path: prepared.effective_recipe_path.clone(),
            params,
            max_rounds,
            env,
            timeout: invocation_timeout,
            enable_developer_builtin: !input.wake_entry.workspace_tool_palette.is_empty(),
            cwd: Some(prepared.work_dir.clone()),
            session_log_path: Some(session_log_path.clone()),
            invocation_id: Some(invocation_id_for_dispatch),
            personality_instance_id: Some(input.personality_instance_id.into_inner()),
            wake_entry_id: Some(input.wake_entry.wake_entry_id),
            change_event_seq: Some(input.change_event_seq),
        })
        .await;

    append_session_log_error_if_present(engine, &input, &outcome_result).await;
    let finalize_outcome = wake_outcome_from_target_result(&input, outcome_result);

    let workspace_outcome = WorkspaceOutcome {
        exit_code: finalize_outcome.exit_code,
        stdout_tail: finalize_outcome.stdout_tail.clone(),
        stderr_tail: finalize_outcome.stderr_tail.clone(),
        duration_ms: finalize_outcome.duration_ms,
    };

    let authored_relation = engine
        .registry()
        .resolve_relation(crate::CORE_AUTHORED_RELATION)
        .ok_or_else(|| ProtocolError::internal("missing core authored relation"))?;
    let derived_from_relation = engine
        .registry()
        .resolve_relation(crate::CORE_DERIVED_FROM_RELATION)
        .ok_or_else(|| ProtocolError::internal("missing core derived-from relation"))?;

    let finalized = runner
        .finalize(WorkspaceFinalizeInput {
            owner: &input.owner,
            invocation_id: invocation_id_for_dispatch,
            root_perspective_memory_id: root_perspective_memory_id_for_dispatch,
            triggering_memory_id: triggering_memory_id_for_dispatch,
            authored_relation,
            derived_from_relation,
            prepared,
            outcome: workspace_outcome,
        })
        .await;

    engine.wake_token_store().revoke(wake_token).await;
    let _ = tokio::fs::remove_file(&effective_recipe_path).await;

    let outcome = match finalized {
        Ok(_) => finalize_outcome,
        Err(err) => {
            WakeInvocationFinalizeOutcome::failed(format!("workspace_runner_finalize:{err}"))
        }
    };
    finalize(engine, &input, outcome).await?;
    Ok(true)
}
