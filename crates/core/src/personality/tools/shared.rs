//! Shared helpers used by `emit_*` substrate tools.

use crate::error::ProtocolError;
use crate::personality::{
    PersonalityMemoryDraft, PersonalityRef, PersonalityToolContext, PersonalityWriteRequest,
    WakeChainDepth,
};
use crate::{
    CORE_AUTHORED_RELATION, CORE_DERIVED_FROM_RELATION, CORE_SUPERSEDES_RELATION, MemoryId,
};

pub(super) const PROMPT_VERSION: &str = "v1";

/// Best-effort text rendering for typed payloads when the personality
/// did not supply one. Personalities that embed-on-meaning should pass
/// `text` explicitly; this is a fallback so the embedding step never
/// crashes on a missing string.
pub(super) fn derive_text(payload: &serde_json::Value) -> String {
    payload
        .get("summary")
        .or_else(|| payload.get("text"))
        .or_else(|| payload.get("title"))
        .and_then(|v| v.as_str())
        .map_or_else(|| payload.to_string(), str::to_string)
}

pub(super) fn normalize_handle_refs_in_payload(
    ctx: &PersonalityToolContext<'_>,
    payload: &mut serde_json::Value,
) {
    normalize_handle_refs_value(ctx, None, payload);
}

fn normalize_handle_refs_value(
    ctx: &PersonalityToolContext<'_>,
    key: Option<&str>,
    value: &mut serde_json::Value,
) {
    match value {
        serde_json::Value::String(raw) => {
            if let Some(resolved) = resolve_handle_ref(ctx, key, raw) {
                *raw = resolved;
            }
        }
        serde_json::Value::Array(values) => {
            for item in values {
                normalize_handle_refs_value(ctx, key, item);
            }
        }
        serde_json::Value::Object(map) => {
            for (field, item) in map {
                normalize_handle_refs_value(ctx, Some(field.as_str()), item);
            }
        }
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {}
    }
}

fn resolve_handle_ref(
    ctx: &PersonalityToolContext<'_>,
    key: Option<&str>,
    raw: &str,
) -> Option<String> {
    let key = key?;
    let normalized = key.strip_suffix('s').unwrap_or(key);
    if normalized == "goal_id" || normalized.ends_with("_goal_id") {
        return ctx
            .handles
            .resolve_goal(raw)
            .ok()
            .map(|id| id.into_inner().to_string());
    }
    if normalized == "memory_id" || normalized.ends_with("_memory_id") {
        return ctx
            .handles
            .resolve_memory(raw)
            .ok()
            .map(|id| id.into_inner().to_string());
    }
    if normalized == "personality_instance_id" || normalized.ends_with("_personality_instance_id") {
        return ctx
            .handles
            .resolve_personality(raw)
            .ok()
            .map(|id| id.into_inner().to_string());
    }
    if normalized == "wake_entry_id" || normalized.ends_with("_wake_entry_id") {
        return ctx
            .handles
            .resolve_wake_entry(raw)
            .ok()
            .map(|id| id.to_string());
    }
    if normalized == "edge_id" || normalized.ends_with("_edge_id") {
        return ctx
            .handles
            .resolve_edge(raw)
            .ok()
            .map(|id| id.into_inner().to_string());
    }
    None
}

/// Persist a single personality-authored memory through the existing
/// `append_personality_memories` storage path. Used by `emit_*` tools
/// after the typed-payload validation + provenance snapshot.
pub(super) async fn emit_personality_memory(
    ctx: &PersonalityToolContext<'_>,
    sidecar_table: &str,
    wake_chain_depth: WakeChainDepth,
    prompt_version: &str,
    draft: &PersonalityMemoryDraft,
) -> Result<MemoryId, ProtocolError> {
    let provenance_relation = ctx
        .engine
        .registry()
        .resolve_relation(CORE_DERIVED_FROM_RELATION)
        .ok_or_else(|| ProtocolError::internal("missing core provenance relation"))?;
    let supersedes_relation = ctx
        .engine
        .registry()
        .resolve_relation(CORE_SUPERSEDES_RELATION)
        .ok_or_else(|| ProtocolError::internal("missing core supersedes relation"))?;
    let authored_relation = ctx
        .engine
        .registry()
        .resolve_relation(CORE_AUTHORED_RELATION)
        .ok_or_else(|| ProtocolError::internal("missing core authored relation"))?;
    let anthropic = ctx
        .engine
        .anthropic()
        .ok_or_else(|| ProtocolError::internal("anthropic client not wired into engine"))?;
    let model_id = model_id_from_personality_context(anthropic.as_ref());
    let instance = PersonalityRef::new(ctx.instance_id);
    let req = PersonalityWriteRequest {
        owner: ctx.owner.clone(),
        instance,
        model_id: &model_id,
        prompt_version,
        provenance_relation,
        supersedes_relation,
        authored_relation,
        current_root_perspective_memory_id: ctx.current_root_perspective_memory_id,
        wake_chain_depth,
        memories: std::slice::from_ref(draft),
        sidecar_table,
    };
    let outcome = ctx
        .engine
        .storage()
        .append_personality_memories(&req)
        .await
        .map_err(|e| ProtocolError::internal(format!("append_personality_memories: {e}")))?;
    outcome
        .memory_ids
        .into_iter()
        .next()
        .ok_or_else(|| ProtocolError::internal("append_personality_memories returned no id"))
}

/// Resolve the `model_id` for stamping provenance on memories emitted
/// from a substrate tool. The in-process wake runtime is gone, so this
/// uses the engine's Standard-tier Anthropic model.
pub fn model_id_from_personality_context(anthropic: &dyn crate::llm::AnthropicClient) -> String {
    anthropic
        .model_id_for(crate::ModelTier::Standard)
        .to_string()
}
