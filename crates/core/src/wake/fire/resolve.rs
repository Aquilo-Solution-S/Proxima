//! Target resolution for wake fire.

use crate::engine::Engine;
use crate::error::ProtocolError;
use crate::verbs::schema::PayloadKind;
use crate::{InferenceTargetConfig, InferenceTargetRow};

use super::input::FireWakeEntryInput;

/// Resolved-target snapshot used to populate the invocation row + env.
#[derive(Debug)]
pub struct ResolvedTarget {
    pub target_ref: String,
    pub config_model_id: Option<String>,
    pub config: InferenceTargetConfig,
}

/// Resolve the inference target for a wake entry.
///
/// # Errors
///
/// Returns `ProtocolError::TierUnbound` when no tier binding covers the
/// entry's model tier, `ProtocolError::InferenceTargetMissing` when the
/// chosen ref has no target row, and `ProtocolError::Internal` for
/// storage failures.
pub async fn resolve_target(
    engine: &Engine,
    input: &FireWakeEntryInput,
) -> Result<ResolvedTarget, ProtocolError> {
    let chosen_ref = if let Some(r) = &input.wake_entry.inference_target_ref {
        r.clone()
    } else {
        let bindings = engine
            .storage()
            .list_inference_tier_bindings(&input.owner)
            .await
            .map_err(|e| ProtocolError::internal(format!("list_inference_tier_bindings: {e}")))?;
        bindings
            .into_iter()
            .find(|b| b.tier == input.wake_entry.model_tier)
            .map(|b| b.target_ref)
            .ok_or_else(|| {
                ProtocolError::tier_unbound(format!("{:?}", input.wake_entry.model_tier))
            })?
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

/// Decode an inference target row into a resolved target.
#[must_use]
pub fn decode_target(target_ref: String, row: InferenceTargetRow) -> ResolvedTarget {
    let config_model_id: Option<String> = match &row.config {
        InferenceTargetConfig::MistralChat(cfg) => Some(cfg.model_id.clone()),
        InferenceTargetConfig::OpenAIChat(cfg) => Some(cfg.model_id.clone()),
        InferenceTargetConfig::OpenAIResponses(cfg) => Some(cfg.model_id.clone()),
        InferenceTargetConfig::ChatGPTCodex(cfg) => Some(cfg.model_id.clone()),
    };
    ResolvedTarget {
        target_ref,
        config_model_id,
        config: row.config,
    }
}

/// Collect sidecar specs from the engine's registry.
#[must_use]
pub fn collect_sidecars(engine: &Engine) -> Vec<crate::personality::SidecarSpec> {
    engine
        .registry()
        .list()
        .into_iter()
        .filter(|s| {
            matches!(
                s.kind,
                PayloadKind::Fact | PayloadKind::Abstraction | PayloadKind::Perspective
            )
        })
        .filter_map(|s| {
            s.sidecar_table
                .map(|table| crate::personality::SidecarSpec {
                    schema_id: s.schema_id,
                    sidecar_table: table,
                })
        })
        .collect()
}
