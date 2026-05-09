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
        .map(str::to_string)
        .unwrap_or_else(|| payload.to_string())
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
    let model_id = if let Some(wake) = ctx.wake_invocation {
        wake.model_id.clone()
    } else {
        let anthropic = ctx
            .engine
            .anthropic()
            .ok_or_else(|| ProtocolError::internal("anthropic client not wired into engine"))?;
        model_id_from_wake_invocation(ctx, anthropic.as_ref())
    };
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
/// from a substrate tool. When a wake invocation is bound to the tool
/// context, we use the resolved `InferenceTarget.model_id` that drove
/// the wake — that is the canonical record of which model authored the
/// memory. The legacy admin-tool path (no wake context bound) falls
/// back to the engine's Standard-tier Anthropic model so the row's
/// `model_id` column is never null.
pub fn model_id_from_wake_invocation(
    ctx: &PersonalityToolContext<'_>,
    anthropic: &dyn crate::llm::AnthropicClient,
) -> String {
    if let Some(w) = ctx.wake_invocation {
        return w.model_id.clone();
    }
    anthropic
        .model_id_for(crate::ModelTier::Standard)
        .to_string()
}
