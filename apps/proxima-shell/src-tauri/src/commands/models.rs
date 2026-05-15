use std::sync::Arc;

use proxima_storage_pg::PgStorage;
use tauri::State;

use crate::command_error::CommandError;
use crate::config::{EmbeddingModelRecord, EmbeddingModelRef};

// ---------------------------------------------------------------
// Embedding settings commands. Inference model/tier settings live in
// `commands::inference_targets`.
// ---------------------------------------------------------------

/// # Errors
/// Returns `CommandError::Storage` on database failures.
#[tauri::command]
#[specta::specta]
pub async fn models_list_embedding(
    pg: State<'_, Arc<PgStorage>>,
) -> Result<Vec<EmbeddingModelRecord>, CommandError> {
    let rows = pg.list_embedding_models().await?;
    Ok(rows.into_iter().map(EmbeddingModelRecord::from).collect())
}

/// # Errors
/// Returns `CommandError::Storage` on database failures.
#[tauri::command]
#[specta::specta]
pub async fn embedding_active_get(
    pg: State<'_, Arc<PgStorage>>,
) -> Result<Option<EmbeddingModelRef>, CommandError> {
    let pair = pg.get_embedding_active().await?;
    Ok(pair.map(|(vendor, model_id)| EmbeddingModelRef { vendor, model_id }))
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
    pg.register_embedding_model(record.into())
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
    pg.delete_embedding_model(&vendor, &model_id)
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
    pg.set_embedding_active(&vendor, &model_id)
        .await
        .map_err(CommandError::from)
}

/// # Errors
/// Returns `CommandError::Storage` on database failures.
#[tauri::command]
#[specta::specta]
pub async fn embedding_active_clear(pg: State<'_, Arc<PgStorage>>) -> Result<bool, CommandError> {
    pg.clear_embedding_active()
        .await
        .map_err(CommandError::from)
}
