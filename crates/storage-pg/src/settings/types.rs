use proxima_core::models::EmbedCaps;
use thiserror::Error;

/// Embedding model row as stored in `proxima_core.embedding_models`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingModel {
    pub vendor: String,
    pub model_id: String,
    pub base_url: String,
    pub caps: EmbedCaps,
    pub secret_ref: Option<String>,
}

/// Helper: map `sqlx::Error` to `SettingsError` for embedding model ops.
pub(super) fn map_sqlx_err_embedding(
    e: sqlx::Error,
    vendor: Option<String>,
    model_id: Option<String>,
) -> SettingsError {
    use sqlx::Error;
    match &e {
        Error::Database(db) if db.is_unique_violation() => {
            if let (Some(v), Some(m)) = (vendor, model_id) {
                SettingsError::DuplicateEmbeddingModel {
                    vendor: v,
                    model_id: m,
                }
            } else {
                SettingsError::Database(e)
            }
        }
        Error::Database(db) if db.is_foreign_key_violation() => {
            if let (Some(v), Some(m)) = (vendor, model_id) {
                SettingsError::UnknownEmbeddingModel {
                    vendor: v,
                    model_id: m,
                }
            } else {
                SettingsError::Database(e)
            }
        }
        Error::Database(db) if db.code().as_deref() == Some("23514") => {
            SettingsError::Invariant(db.message().to_string())
        }
        _ => SettingsError::Database(e),
    }
}

/// Settings error type.
#[derive(Debug, Error)]
pub enum SettingsError {
    #[error("settings db error: {0}")]
    Database(#[source] sqlx::Error),

    #[error("duplicate embedding model {vendor:?}/{model_id:?}")]
    DuplicateEmbeddingModel { vendor: String, model_id: String },

    /// FK violation when setting active embedding — the (vendor,
    /// `model_id`) is not in binary-wide `embedding_models`.
    #[error("unknown embedding model {vendor:?}/{model_id:?}")]
    UnknownEmbeddingModel { vendor: String, model_id: String },

    /// CHECK constraint violation. Indicates a code bug since closed
    /// vocabularies are typed enums on the Rust side; surface
    /// explicitly so it isn't silently folded into Database.
    #[error("settings invariant violation: {0}")]
    Invariant(String),
}
