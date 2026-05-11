//! Target and recipe resolution for wake fire.

use std::path::PathBuf;

use crate::InferenceTargetRow;
use crate::engine::Engine;
use crate::error::ProtocolError;

use super::input::FireWakeEntryInput;

/// Resolved-target snapshot used to populate the invocation row + env.
pub struct ResolvedTarget {
    pub target_ref: String,
    pub config_model_id: Option<String>,
    pub env_overrides: Vec<(String, String)>,
}

/// Resolve the inference target for a wake entry.
pub async fn resolve_target(
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

/// Decode an inference target row into a resolved target.
pub fn decode_target(target_ref: String, row: InferenceTargetRow) -> ResolvedTarget {
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

/// Resolve the recipe path for a wake entry.
pub fn resolve_recipe_path(
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

/// Collect sidecar specs from the engine's registry.
pub fn collect_sidecars(engine: &Engine) -> Vec<crate::personality::SidecarSpec> {
    engine
        .registry()
        .list()
        .into_iter()
        .filter_map(|s| {
            s.sidecar_table.map(|table| crate::personality::SidecarSpec {
                schema_id: s.schema_id,
                sidecar_table: table,
            })
        })
        .collect()
}
