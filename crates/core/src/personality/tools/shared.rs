//! Shared helpers used by `emit_*` substrate tools.

use crate::error::ProtocolError;
use crate::personality::{
    PersonalityMemoryDraft, PersonalityRef, PersonalityToolContext, PersonalityWriteRequest,
    WakeChainDepth,
};
use crate::{CORE_DERIVED_FROM_RELATION, CORE_SUPERSEDES_RELATION, MemoryId};

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
    let anthropic = ctx
        .engine
        .anthropic()
        .ok_or_else(|| ProtocolError::internal("anthropic client not wired into engine"))?;
    let model_id = anthropic
        .model_id_for(model_tier_from_palette(ctx))
        .to_string();
    let instance = PersonalityRef::new(ctx.type_id.to_string(), ctx.instance_id);
    let req = PersonalityWriteRequest {
        owner: ctx.owner.clone(),
        instance,
        model_id: &model_id,
        prompt_version,
        provenance_relation,
        supersedes_relation,
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

/// Resolve the model tier for stamping provenance on memories emitted
/// from a substrate tool. v1 returns `Standard` unconditionally because
/// no wakes fire yet. Phase 1d sources this from the live invocation
/// context so the stamp matches the model that executed the recipe.
fn model_tier_from_palette(_ctx: &PersonalityToolContext<'_>) -> crate::ModelTier {
    // TODO(phase-1d): read tier from the active WakeInvocation context.
    crate::ModelTier::Standard
}
