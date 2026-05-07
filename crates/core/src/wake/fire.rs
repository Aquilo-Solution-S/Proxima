//! Phase 1d Task 8: per-entry wake fire path.
//!
//! `fire_wake_entry` mints a wake token, snapshots the resolved
//! inference target + recipe SHA-256 onto the invocation row, drives
//! the [`TargetAdapter`], and finalizes status. Workspace mode
//! short-circuits with `failure_reason = workspace_mode_not_yet_implemented`
//! until Phase 1e. Self-wake is a defense-in-depth `Ok(false)` (the
//! dispatcher's `authored_by` filter is the primary guard).
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
use crate::personality::{
    PersonalityInstanceId, SidecarSpec, WakeEntryExecutionMode, WakeEntryRow,
    WakeInvocationFinalize, WakeInvocationStart, WakeInvocationStatus,
};
use crate::wake::context::assemble_wake_context;
use crate::wake::target_adapter::{
    TargetAdapter, TargetInvocation, TargetOutcome, TargetOutcomeKind,
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
        owner: input.owner.clone(),
        palette: input.wake_entry.substrate_tool_palette.clone(),
        model_id: resolved.config_model_id.clone().unwrap_or_default(),
        max_rounds: u32::from(input.wake_entry.max_rounds),
    };
    let wake_token = engine.wake_token_store().mint(token_ctx).await;

    // 5. INSERT invocation row (status = running).
    engine
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

    // 6. Workspace mode stub: fail with reason, revoke token, exit.
    // Phase 1e flips this into the real workspace fork; until then we
    // want the row + reason on disk so dispatcher posture observability
    // sees the attempt.
    if matches!(
        input.wake_entry.execution_mode,
        WakeEntryExecutionMode::Workspace
    ) {
        engine.wake_token_store().revoke(wake_token).await;
        finalize(engine, &input, WakeInvocationFinalizeOutcome {
            status: WakeInvocationStatus::Failed,
            turn_count: None,
            cost_usd: None,
            failure_reason: Some("workspace_mode_not_yet_implemented".to_string()),
        })
        .await?;
        return Ok(true);
    }

    // 7. Build the four params as JSON values for goose --params.
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

    // 8. Build env. PROXIMA_WAKE_TOKEN + PROXIMA_MCP_URL are the
    // always-injected pair the substrate authorization layer relies on.
    // Target-resolved overrides (e.g. GOOSE_PROFILE) layer on top.
    let mcp_url = engine.mcp_url().ok_or_else(|| {
        ProtocolError::internal(
            "engine.mcp_url() is None — call Engine::start (or set_mcp_url) before firing wakes",
        )
    })?;
    let mut env: HashMap<String, String> = HashMap::new();
    env.insert("PROXIMA_WAKE_TOKEN".to_string(), wake_token.to_string());
    env.insert("PROXIMA_MCP_URL".to_string(), mcp_url);
    for (k, v) in &resolved.env_overrides {
        env.insert(k.clone(), v.clone());
    }

    // 9. Run adapter.
    let max_rounds = u32::from(input.wake_entry.max_rounds);
    let outcome_result = adapter
        .run(TargetInvocation {
            recipe_path,
            params,
            max_rounds,
            env,
            timeout: per_invocation_timeout(max_rounds),
        })
        .await;

    // 10. Finalize: revoke token, write outcome.
    engine.wake_token_store().revoke(wake_token).await;
    let outcome = match outcome_result {
        Ok(TargetOutcome {
            kind,
            turn_count,
            stderr_tail,
        }) => match kind {
            TargetOutcomeKind::Succeeded => WakeInvocationFinalizeOutcome {
                status: WakeInvocationStatus::Succeeded,
                turn_count: turn_count.and_then(|c| u16::try_from(c.max(0)).ok()),
                cost_usd: None,
                failure_reason: None,
            },
            TargetOutcomeKind::Truncated => WakeInvocationFinalizeOutcome {
                status: WakeInvocationStatus::Truncated,
                turn_count: turn_count
                    .and_then(|c| u16::try_from(c.max(0)).ok())
                    .or(Some(input.wake_entry.max_rounds)),
                cost_usd: None,
                failure_reason: Some("max_rounds_reached".to_string()),
            },
            TargetOutcomeKind::Failed => WakeInvocationFinalizeOutcome {
                status: WakeInvocationStatus::Failed,
                turn_count: turn_count.and_then(|c| u16::try_from(c.max(0)).ok()),
                cost_usd: None,
                failure_reason: Some(stderr_tail),
            },
        },
        Err(e) => WakeInvocationFinalizeOutcome {
            status: WakeInvocationStatus::Failed,
            turn_count: None,
            cost_usd: None,
            failure_reason: Some(format!("adapter_error: {e}")),
        },
    };
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
        })
        .await
        .map_err(|e| ProtocolError::internal(format!("finalize_wake_invocation: {e}")))
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
