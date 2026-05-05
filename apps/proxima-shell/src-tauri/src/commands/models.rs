use std::sync::Arc;

use proxima_core::Engine;
use proxima_core::models::{LlmCaps, ModelTier};
use proxima_storage_pg::PgStorage;
use tauri::State;

use crate::boot::sentinel_owner;
use crate::command_error::CommandError;
use crate::config::{EmbeddingModelRecord, LlmModelRecord, ModelRef, TierBindings};

// ---------------------------------------------------------------
// Settings commands (m6.23) — DB-backed AppConfig surface for the
// Models settings panel (S1.g). Each handler pulls Arc<PgStorage>
// from Tauri state, calls the storage method, and maps errors via
// CommandError::from impls. tier_requires reads the engine's
// operator-union directly (no PG).
//
// Owner is a sentinel today (Uuid::nil) — multi-tenant deployments
// in v1.1+ wire owner from the auth context without changing the
// command shape.
// ---------------------------------------------------------------

/// # Errors
/// Returns `CommandError::Storage` on database failures.
#[tauri::command]
#[specta::specta]
pub async fn models_list_llm(
    pg: State<'_, Arc<PgStorage>>,
) -> Result<Vec<LlmModelRecord>, CommandError> {
    let owner = sentinel_owner();
    let rows = pg.list_llm_models(&owner).await?;
    Ok(rows.into_iter().map(LlmModelRecord::from).collect())
}

/// # Errors
/// Returns `CommandError::Storage` on database failures.
#[tauri::command]
#[specta::specta]
pub async fn models_list_embedding(
    pg: State<'_, Arc<PgStorage>>,
) -> Result<Vec<EmbeddingModelRecord>, CommandError> {
    let owner = sentinel_owner();
    let rows = pg.list_embedding_models(&owner).await?;
    Ok(rows.into_iter().map(EmbeddingModelRecord::from).collect())
}

/// # Errors
/// Returns `CommandError::Storage` on database failures.
#[tauri::command]
#[specta::specta]
pub async fn tier_bindings_get(
    pg: State<'_, Arc<PgStorage>>,
) -> Result<TierBindings, CommandError> {
    let owner = sentinel_owner();
    let raw = pg.list_tier_bindings(&owner).await?;
    let mut tb = TierBindings::default();
    for (tier, vendor, model_id) in raw {
        let r = ModelRef { vendor, model_id };
        match tier {
            ModelTier::Fast => tb.fast = Some(r),
            ModelTier::Standard => tb.standard = Some(r),
            ModelTier::Deep => tb.deep = Some(r),
        }
    }
    Ok(tb)
}

/// # Errors
/// Returns `CommandError::Storage` on database failures.
#[tauri::command]
#[specta::specta]
pub async fn embedding_active_get(
    pg: State<'_, Arc<PgStorage>>,
) -> Result<Option<ModelRef>, CommandError> {
    let owner = sentinel_owner();
    let pair = pg.get_embedding_active(&owner).await?;
    Ok(pair.map(|(vendor, model_id)| ModelRef { vendor, model_id }))
}

/// # Errors
/// Returns `CommandError::Storage` on database failures.
#[tauri::command]
#[specta::specta]
pub async fn tier_requires(
    engine: State<'_, Arc<Engine>>,
    tier: ModelTier,
) -> Result<LlmCaps, CommandError> {
    Ok(engine.tier_requires_union(tier))
}

/// # Errors
/// Returns `CommandError::DuplicateLlmModel` if the model already exists,
/// `CommandError::Storage` on database failures.
#[tauri::command]
#[specta::specta]
pub async fn models_register_llm(
    pg: State<'_, Arc<PgStorage>>,
    record: LlmModelRecord,
) -> Result<(), CommandError> {
    let owner = sentinel_owner();
    pg.register_llm_model(&owner, record.into())
        .await
        .map_err(CommandError::from)
}

/// # Errors
/// Returns `CommandError::DuplicateEmbeddingModel` if the model already exists,
/// `CommandError::Storage` on database failures.
#[tauri::command]
#[specta::specta]
pub async fn models_register_embedding(
    pg: State<'_, Arc<PgStorage>>,
    record: EmbeddingModelRecord,
) -> Result<(), CommandError> {
    let owner = sentinel_owner();
    pg.register_embedding_model(&owner, record.into())
        .await
        .map_err(CommandError::from)
}

/// # Errors
/// Returns `CommandError::Storage` on database failures.
#[tauri::command]
#[specta::specta]
pub async fn models_delete_llm(
    pg: State<'_, Arc<PgStorage>>,
    vendor: String,
    model_id: String,
) -> Result<bool, CommandError> {
    let owner = sentinel_owner();
    pg.delete_llm_model(&owner, &vendor, &model_id)
        .await
        .map_err(CommandError::from)
}

/// # Errors
/// Returns `CommandError::Storage` on database failures.
#[tauri::command]
#[specta::specta]
pub async fn models_delete_embedding(
    pg: State<'_, Arc<PgStorage>>,
    vendor: String,
    model_id: String,
) -> Result<bool, CommandError> {
    let owner = sentinel_owner();
    pg.delete_embedding_model(&owner, &vendor, &model_id)
        .await
        .map_err(CommandError::from)
}

/// # Errors
/// Returns `CommandError::InsufficientTierCaps` if the model's caps don't satisfy
/// the tier's operator-union requirements, `CommandError::UnknownLlmModel` if the
/// model is not registered, or `CommandError::Storage` on database failures.
#[tauri::command]
#[specta::specta]
pub async fn tier_bind(
    engine: State<'_, Arc<Engine>>,
    pg: State<'_, Arc<PgStorage>>,
    tier: ModelTier,
    vendor: String,
    model_id: String,
) -> Result<(), CommandError> {
    let owner = sentinel_owner();
    // Caps pre-check — refuse to bind if the model can't satisfy
    // the engine's operator-union for this tier.
    let llm_models = pg.list_llm_models(&owner).await?;
    let bound = llm_models
        .iter()
        .find(|m| m.vendor == vendor && m.model_id == model_id)
        .ok_or(CommandError::UnknownLlmModel {
            model_ref: ModelRef {
                vendor: vendor.clone(),
                model_id: model_id.clone(),
            },
        })?;
    let required = engine.tier_requires_union(tier);
    if !bound.caps.satisfies(&required) {
        return Err(CommandError::InsufficientTierCaps {
            tier,
            model_ref: ModelRef {
                vendor: vendor.clone(),
                model_id: model_id.clone(),
            },
        });
    }
    pg.bind_tier(&owner, tier, &vendor, &model_id)
        .await
        .map_err(CommandError::from)
}

/// # Errors
/// Returns `CommandError::Storage` on database failures.
#[tauri::command]
#[specta::specta]
pub async fn tier_unbind(
    pg: State<'_, Arc<PgStorage>>,
    tier: ModelTier,
) -> Result<bool, CommandError> {
    let owner = sentinel_owner();
    pg.unbind_tier(&owner, tier)
        .await
        .map_err(CommandError::from)
}

/// # Errors
/// Returns `CommandError::UnknownEmbeddingModel` if the model is not registered,
/// `CommandError::Storage` on database failures.
#[tauri::command]
#[specta::specta]
pub async fn embedding_active_set(
    pg: State<'_, Arc<PgStorage>>,
    vendor: String,
    model_id: String,
) -> Result<(), CommandError> {
    let owner = sentinel_owner();
    pg.set_embedding_active(&owner, &vendor, &model_id)
        .await
        .map_err(CommandError::from)
}

/// # Errors
/// Returns `CommandError::Storage` on database failures.
#[tauri::command]
#[specta::specta]
pub async fn embedding_active_clear(pg: State<'_, Arc<PgStorage>>) -> Result<bool, CommandError> {
    let owner = sentinel_owner();
    pg.clear_embedding_active(&owner)
        .await
        .map_err(CommandError::from)
}
