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
use std::time::Duration;

use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::InferenceTargetRow;
use crate::Owner;
use crate::engine::Engine;
use crate::error::ProtocolError;
use crate::mcp::provider_safe_tool_name;
use crate::personality::workspace::{
    WorkspaceFinalizeInput, WorkspaceOutcome, WorkspacePrepareInput, WorkspaceRunnerError,
};
use crate::personality::{
    PersonalityInstanceId, SidecarSpec, WakeChainDepth, WakeEntryExecutionMode, WakeEntryRow,
    WakeInvocationFinalize, WakeInvocationLogDraft, WakeInvocationStart, WakeInvocationStatus,
};
use crate::wake::context::assemble_wake_context;
use crate::wake::target_adapter::{
    TargetAdapter, TargetAdapterError, TargetInvocation, TargetOutcome, TargetOutcomeKind,
};
use crate::wake::token_store::WakeTokenContext;

/// Inputs to one wake fire — assembled by the dispatcher tick from the
/// `WakeDispatchEntryRow` it just matched.
#[derive(Debug, Clone)]
pub struct FireWakeEntryInput {
    pub owner: Owner,
    pub personality_instance_id: PersonalityInstanceId,
    pub wake_entry: WakeEntryRow,
    pub change_event_seq: Uuid,
    pub triggering_memory_id: Uuid,
}

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
    let token_ctx = WakeTokenContext {
        invocation_id: Uuid::new_v4(),
        personality_instance_id: input.personality_instance_id.into_inner(),
        wake_entry_id: input.wake_entry.wake_entry_id,
        change_event_seq: input.change_event_seq,
        owner: input.owner.clone(),
        palette: input.wake_entry.substrate_tool_palette.clone(),
        model_id: resolved.config_model_id.clone().unwrap_or_default(),
        max_rounds: u32::from(input.wake_entry.max_rounds),
        current_root_perspective_memory_id: crate::MemoryId::new(
            wake_context.root_perspective.memory_id,
        ),
        triggering_event_memory_id: crate::MemoryId::new(wake_context.triggering_memory.memory_id),
        triggering_event_depth: WakeChainDepth::new(
            u16::try_from(wake_context.trigger_event.wake_chain_depth).unwrap_or(0),
        ),
        read_log: std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new())),
    };
    let invocation_id_for_dispatch = token_ctx.invocation_id;
    let root_perspective_memory_id_for_dispatch = token_ctx.current_root_perspective_memory_id;
    let triggering_memory_id_for_dispatch = token_ctx.triggering_event_memory_id;
    let wake_token = engine.wake_token_store().mint(token_ctx).await;

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
        "available",
        session_log_path.display().to_string(),
    )
    .await;

    // 6. Build the four params as JSON values for goose --params.
    // The recipe drops anything it doesn't bind, so passing all four
    // unconditionally is the cheapest contract.
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
                    WakeInvocationFinalizeOutcome::failed(format!(
                        "workspace_runner_prepare:{err}"
                    )),
                )
                .await?;
                return Ok(true);
            }
        };

        let max_rounds = u32::from(input.wake_entry.max_rounds);
        let outcome_result = adapter
            .run(TargetInvocation {
                recipe_path: prepared.effective_recipe_path.clone(),
                params,
                max_rounds,
                env,
                timeout: per_invocation_timeout(max_rounds),
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
        return Ok(true);
    }

    // 9. Run adapter.
    let max_rounds = u32::from(input.wake_entry.max_rounds);
    let outcome_result = adapter
        .run(TargetInvocation {
            recipe_path: effective_recipe_path.clone(),
            params,
            max_rounds,
            env,
            timeout: per_invocation_timeout(max_rounds),
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
        Ok(TargetOutcome {
            kind,
            turn_count,
            exit_code,
            duration_ms,
            stdout_tail,
            stderr_tail,
            stdout_truncated,
            stderr_truncated,
            session_log_error: _,
        }) => match kind {
            TargetOutcomeKind::Succeeded => WakeInvocationFinalizeOutcome {
                status: WakeInvocationStatus::Succeeded,
                turn_count: turn_count.and_then(|c| u16::try_from(c.max(0)).ok()),
                cost_usd: None,
                failure_reason: None,
                exit_code,
                duration_ms: Some(duration_ms),
                stdout_tail: Some(stdout_tail),
                stderr_tail: Some(stderr_tail),
                stdout_truncated,
                stderr_truncated,
            },
            TargetOutcomeKind::Truncated => WakeInvocationFinalizeOutcome {
                status: WakeInvocationStatus::Truncated,
                turn_count: turn_count
                    .and_then(|c| u16::try_from(c.max(0)).ok())
                    .or(Some(input.wake_entry.max_rounds)),
                cost_usd: None,
                failure_reason: Some("max_rounds_reached".to_string()),
                exit_code,
                duration_ms: Some(duration_ms),
                stdout_tail: Some(stdout_tail),
                stderr_tail: Some(stderr_tail),
                stdout_truncated,
                stderr_truncated,
            },
            TargetOutcomeKind::Failed => WakeInvocationFinalizeOutcome {
                status: WakeInvocationStatus::Failed,
                turn_count: turn_count.and_then(|c| u16::try_from(c.max(0)).ok()),
                cost_usd: None,
                failure_reason: Some(stderr_tail.clone()),
                exit_code,
                duration_ms: Some(duration_ms),
                stdout_tail: Some(stdout_tail),
                stderr_tail: Some(stderr_tail),
                stdout_truncated,
                stderr_truncated,
            },
        },
        Err(e) => WakeInvocationFinalizeOutcome {
            status: WakeInvocationStatus::Failed,
            turn_count: None,
            cost_usd: None,
            failure_reason: Some(format!("adapter_error: {e}")),
            exit_code: None,
            duration_ms: None,
            stdout_tail: None,
            stderr_tail: None,
            stdout_truncated: false,
            stderr_truncated: false,
        },
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

/// Internal mirror of `WakeInvocationFinalize` that elides the routing
/// keys (owner / instance / entry / seq) the call site already knows.
struct WakeInvocationFinalizeOutcome {
    status: WakeInvocationStatus,
    turn_count: Option<u16>,
    cost_usd: Option<f64>,
    failure_reason: Option<String>,
    exit_code: Option<i32>,
    duration_ms: Option<u64>,
    stdout_tail: Option<String>,
    stderr_tail: Option<String>,
    stdout_truncated: bool,
    stderr_truncated: bool,
}

impl WakeInvocationFinalizeOutcome {
    fn failed(failure_reason: String) -> Self {
        Self {
            status: WakeInvocationStatus::Failed,
            turn_count: None,
            cost_usd: None,
            failure_reason: Some(failure_reason),
            exit_code: None,
            duration_ms: None,
            stdout_tail: None,
            stderr_tail: None,
            stdout_truncated: false,
            stderr_truncated: false,
        }
    }
}

fn wake_outcome_from_target_result(
    input: &FireWakeEntryInput,
    outcome_result: Result<TargetOutcome, TargetAdapterError>,
) -> WakeInvocationFinalizeOutcome {
    match outcome_result {
        Ok(TargetOutcome {
            kind,
            turn_count,
            exit_code,
            duration_ms,
            stdout_tail,
            stderr_tail,
            stdout_truncated,
            stderr_truncated,
            session_log_error: _,
        }) => match kind {
            TargetOutcomeKind::Succeeded => WakeInvocationFinalizeOutcome {
                status: WakeInvocationStatus::Succeeded,
                turn_count: turn_count.and_then(|c| u16::try_from(c.max(0)).ok()),
                cost_usd: None,
                failure_reason: None,
                exit_code,
                duration_ms: Some(duration_ms),
                stdout_tail: Some(stdout_tail),
                stderr_tail: Some(stderr_tail),
                stdout_truncated,
                stderr_truncated,
            },
            TargetOutcomeKind::Truncated => WakeInvocationFinalizeOutcome {
                status: WakeInvocationStatus::Truncated,
                turn_count: turn_count
                    .and_then(|c| u16::try_from(c.max(0)).ok())
                    .or(Some(input.wake_entry.max_rounds)),
                cost_usd: None,
                failure_reason: Some("max_rounds_reached".to_string()),
                exit_code,
                duration_ms: Some(duration_ms),
                stdout_tail: Some(stdout_tail),
                stderr_tail: Some(stderr_tail),
                stdout_truncated,
                stderr_truncated,
            },
            TargetOutcomeKind::Failed => WakeInvocationFinalizeOutcome {
                status: WakeInvocationStatus::Failed,
                turn_count: turn_count.and_then(|c| u16::try_from(c.max(0)).ok()),
                cost_usd: None,
                failure_reason: Some(stderr_tail.clone()),
                exit_code,
                duration_ms: Some(duration_ms),
                stdout_tail: Some(stdout_tail),
                stderr_tail: Some(stderr_tail),
                stdout_truncated,
                stderr_truncated,
            },
        },
        Err(e) => WakeInvocationFinalizeOutcome::failed(format!("adapter_error: {e}")),
    }
}

async fn finalize(
    engine: &Engine,
    input: &FireWakeEntryInput,
    outcome: WakeInvocationFinalizeOutcome,
) -> Result<(), ProtocolError> {
    engine
        .storage()
        .finalize_wake_invocation(&WakeInvocationFinalize {
            owner: input.owner.clone(),
            personality_instance_id: input.personality_instance_id,
            wake_entry_id: input.wake_entry.wake_entry_id,
            change_event_seq: input.change_event_seq,
            status: outcome.status,
            turn_count: outcome.turn_count,
            cost_usd: outcome.cost_usd,
            failure_reason: outcome.failure_reason,
            exit_code: outcome.exit_code,
            duration_ms: outcome.duration_ms,
            stdout_tail: outcome.stdout_tail,
            stderr_tail: outcome.stderr_tail,
            stdout_truncated: outcome.stdout_truncated,
            stderr_truncated: outcome.stderr_truncated,
        })
        .await
        .map_err(|e| ProtocolError::internal(format!("finalize_wake_invocation: {e}")))
}

async fn append_session_artifact_log(
    engine: &Engine,
    input: &FireWakeEntryInput,
    status: &str,
    message_tail: String,
) {
    if let Err(err) = engine
        .storage()
        .append_wake_invocation_log(&WakeInvocationLogDraft {
            owner: input.owner.clone(),
            personality_instance_id: input.personality_instance_id,
            wake_entry_id: input.wake_entry.wake_entry_id,
            change_event_seq: input.change_event_seq,
            phase: "session_artifact".to_string(),
            tool_id: None,
            status: status.to_string(),
            duration_ms: None,
            message_tail: Some(message_tail),
        })
        .await
    {
        tracing::warn!(
            personality_instance_id = %input.personality_instance_id.into_inner(),
            wake_entry_id = %input.wake_entry.wake_entry_id,
            change_event_seq = %input.change_event_seq,
            error = %err,
            "failed to append wake session artifact log"
        );
    }
}

async fn append_session_log_error_if_present(
    engine: &Engine,
    input: &FireWakeEntryInput,
    outcome_result: &Result<TargetOutcome, TargetAdapterError>,
) {
    let Some(error) = outcome_result
        .as_ref()
        .ok()
        .and_then(|outcome| outcome.session_log_error.as_ref())
    else {
        return;
    };
    append_session_artifact_log(engine, input, "failed", error.clone()).await;
}

fn wake_session_log_path(owner: &Owner, invocation_id: Uuid) -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join(".proxima/wake-runs")
        .join(owner_principal_segment(owner))
        .join(invocation_id.to_string())
        .join("worker-session.jsonl")
}

fn owner_principal_segment(owner: &Owner) -> String {
    match &owner.principal {
        crate::Principal::User(user) => format!("user-{}", user.into_inner()),
        crate::Principal::Group(group) => format!("group-{}", group.into_inner()),
    }
}

fn per_invocation_timeout(max_rounds: u32) -> Duration {
    // Conservative: 60s per round + 30s startup. Adapter-side timeouts
    // are the floor; the dispatcher's outer cancel signal is the
    // ceiling. Phase 1e tunes this once Code-flavor recipes have a
    // measured p95.
    Duration::from_secs(30 + u64::from(max_rounds) * 60)
}

/// Resolved-target snapshot used to populate the invocation row + env.
struct ResolvedTarget {
    target_ref: String,
    config_model_id: Option<String>,
    env_overrides: Vec<(String, String)>,
}

async fn resolve_target(
    engine: &Engine,
    input: &FireWakeEntryInput,
) -> Result<ResolvedTarget, ProtocolError> {
    let chosen_ref = match &input.wake_entry.inference_target_ref {
        Some(r) => r.clone(),
        None => {
            let bindings = engine
                .storage()
                .list_inference_tier_bindings(&input.owner)
                .await
                .map_err(|e| {
                    ProtocolError::internal(format!("list_inference_tier_bindings: {e}"))
                })?;
            bindings
                .into_iter()
                .find(|b| b.tier == input.wake_entry.model_tier)
                .map(|b| b.target_ref)
                .ok_or_else(|| {
                    ProtocolError::tier_unbound(format!("{:?}", input.wake_entry.model_tier))
                })?
        }
    };
    let targets = engine
        .storage()
        .list_inference_targets(&input.owner)
        .await
        .map_err(|e| ProtocolError::internal(format!("list_inference_targets: {e}")))?;
    let row = targets
        .into_iter()
        .find(|t| t.target_ref == chosen_ref)
        .ok_or_else(|| ProtocolError::inference_target_missing(&chosen_ref))?;
    Ok(decode_target(chosen_ref, row))
}

fn decode_target(target_ref: String, row: InferenceTargetRow) -> ResolvedTarget {
    use crate::InferenceTargetConfig;
    let mut env_overrides: Vec<(String, String)> = Vec::new();
    let config_model_id: Option<String> = match row.config {
        InferenceTargetConfig::LocalCli(cfg) => {
            if let Some(profile) = cfg.profile {
                env_overrides.push(("GOOSE_PROFILE".to_string(), profile));
            }
            for (k, v) in cfg.env_overrides {
                env_overrides.push((k, v));
            }
            Some(cfg.command)
        }
        InferenceTargetConfig::RemoteModel(cfg) => {
            // Vendor-specific credential injection lands in Phase 2;
            // for v1 the LocalCli adapter is the only consumer.
            Some(cfg.model_id)
        }
    };
    ResolvedTarget {
        target_ref,
        config_model_id,
        env_overrides,
    }
}

fn resolve_recipe_path(
    engine: &Engine,
    input: &FireWakeEntryInput,
) -> Result<PathBuf, ProtocolError> {
    crate::inference::recipe_resolve::resolve_recipe_ref(
        &input.wake_entry.recipe_ref,
        &engine.owner_recipes_root(&input.owner),
        engine.registry(),
    )
    .map_err(|e| match e {
        crate::inference::recipe_resolve::RecipeResolveError::Malformed(_)
        | crate::inference::recipe_resolve::RecipeResolveError::BundledNotRegistered(_)
        | crate::inference::recipe_resolve::RecipeResolveError::UserMissing(_) => {
            ProtocolError::recipe_not_found(&input.wake_entry.recipe_ref)
        }
    })
}

async fn write_effective_recipe(
    source_bytes: &[u8],
    mcp_url: &str,
    wake_token: Uuid,
    substrate_tools: &[String],
    workspace_tools: &[String],
) -> Result<PathBuf, ProtocolError> {
    let source = std::str::from_utf8(source_bytes)
        .map_err(|e| ProtocolError::internal(format!("recipe is not utf8: {e}")))?;
    let mut rendered = strip_top_level_extensions(source);
    if !rendered.ends_with('\n') {
        rendered.push('\n');
    }
    rendered.push_str("extensions:\n");
    rendered.push_str("  - type: streamable_http\n");
    rendered.push_str("    name: proxima-engine-mcp\n");
    rendered.push_str(&format!("    uri: \"{}\"\n", yaml_quote(mcp_url)));
    rendered.push_str("    headers:\n");
    rendered.push_str(&format!(
        "      authorization: \"Bearer {}\"\n",
        yaml_quote(&wake_token.to_string())
    ));
    if substrate_tools.is_empty() {
        rendered.push_str("    available_tools: []\n");
    } else {
        rendered.push_str("    available_tools:\n");
        for tool in substrate_tools {
            rendered.push_str(&format!(
                "      - \"{}\"\n",
                yaml_quote(&provider_safe_tool_name(tool))
            ));
        }
    }
    if !workspace_tools.is_empty() {
        rendered.push_str("  - type: builtin\n");
        rendered.push_str("    name: developer\n");
        rendered.push_str("    available_tools:\n");
        for tool in workspace_tools {
            let provider_tool = workspace_tool_to_goose(tool).ok_or_else(|| {
                ProtocolError::tool_not_registered(format!(
                    "workspace tool mapping missing: {tool}"
                ))
            })?;
            rendered.push_str(&format!("      - \"{}\"\n", yaml_quote(provider_tool)));
        }
    }

    let path =
        std::env::temp_dir().join(format!("proxima-wake-{}-{wake_token}.yaml", Uuid::new_v4()));
    tokio::fs::write(&path, rendered)
        .await
        .map_err(|e| ProtocolError::internal(format!("write effective recipe: {e}")))?;
    Ok(path)
}

fn workspace_tool_to_goose(tool_id: &str) -> Option<&'static str> {
    match tool_id {
        "proxima-workspace/text_editor" => Some("developer__text_editor"),
        "proxima-workspace/shell" => Some("developer__shell"),
        "proxima-workspace/list_files" => Some("developer__list_files"),
        _ => None,
    }
}

fn strip_top_level_extensions(source: &str) -> String {
    let mut out = String::new();
    let mut skipping = false;
    for line in source.lines() {
        let is_top_level = !line.starts_with(' ') && !line.starts_with('\t');
        if is_top_level && line.trim_start().starts_with("extensions:") {
            skipping = true;
            continue;
        }
        if skipping
            && is_top_level
            && !line.trim().is_empty()
            && !line.trim_start().starts_with('#')
        {
            skipping = false;
        }
        if !skipping {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

fn yaml_quote(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn collect_sidecars(engine: &Engine) -> Vec<SidecarSpec> {
    engine
        .registry()
        .list()
        .into_iter()
        .filter_map(|s| {
            s.sidecar_table.map(|table| SidecarSpec {
                schema_id: s.schema_id,
                sidecar_table: table,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_existing_top_level_extensions() {
        let source =
            "version: 1.0.0\nextensions:\n  - type: builtin\n    name: developer\nprompt: hi\n";

        let stripped = strip_top_level_extensions(source);

        assert!(!stripped.contains("type: builtin"));
        assert!(stripped.contains("prompt: hi"));
    }

    #[tokio::test]
    async fn effective_recipe_injects_wake_mcp_extension() {
        let token = Uuid::new_v4();
        let path = write_effective_recipe(
            b"version: 1.0.0\ntitle: smoke\nprompt: hi\n",
            "http://127.0.0.1:31415/mcp",
            token,
            &[
                "core/fetch_memory".to_string(),
                "core/emit_abstraction".to_string(),
            ],
            &[],
        )
        .await
        .expect("write effective recipe");

        let rendered = tokio::fs::read_to_string(&path).await.expect("read recipe");
        let _ = tokio::fs::remove_file(&path).await;

        assert!(rendered.contains("type: streamable_http"));
        assert!(rendered.contains("uri: \"http://127.0.0.1:31415/mcp\""));
        assert!(rendered.contains(&format!("authorization: \"Bearer {token}\"")));
        assert!(rendered.contains("- \"core_fetch_memory\""));
        assert!(rendered.contains("- \"core_emit_abstraction\""));
    }

    #[tokio::test]
    async fn effective_recipe_injects_developer_extension_for_workspace_tools() {
        let token = Uuid::new_v4();
        let path = write_effective_recipe(
            b"version: 1.0.0\ntitle: smoke\nprompt: hi\n",
            "http://127.0.0.1:31415/mcp",
            token,
            &[],
            &[
                "proxima-workspace/text_editor".to_string(),
                "proxima-workspace/shell".to_string(),
                "proxima-workspace/list_files".to_string(),
            ],
        )
        .await
        .expect("write effective recipe");

        let rendered = tokio::fs::read_to_string(&path).await.expect("read recipe");
        let _ = tokio::fs::remove_file(&path).await;

        assert!(rendered.contains("name: developer"));
        assert!(rendered.contains("- \"developer__text_editor\""));
        assert!(rendered.contains("- \"developer__shell\""));
        assert!(rendered.contains("- \"developer__list_files\""));
    }
}
