use proxima_core::models::EmbedCaps;
use proxima_core::{Owner, OwnerPrincipalKind, Principal};
use thiserror::Error;
use uuid::Uuid;

/// Embedding model row as stored in `proxima_core.embedding_models`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingModel {
    pub vendor: String,
    pub model_id: String,
    pub base_url: String,
    pub caps: EmbedCaps,
    pub secret_ref: Option<String>,
}

/// Helper: decompose Owner into the triple used in WHERE clauses.
pub(super) fn owner_triple(owner: &Owner) -> (OwnerPrincipalKind, Uuid, Uuid) {
    let (kind, principal_id) = match &owner.principal {
        Principal::User(u) => (OwnerPrincipalKind::User, u.into_inner()),
        Principal::Group(g) => (OwnerPrincipalKind::Group, g.into_inner()),
    };
    (kind, principal_id, owner.org_id.into_inner())
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

/// Helper: map `sqlx::Error` to `SettingsError` for inference-target ops.
pub(super) fn map_sqlx_err_inference_target(e: sqlx::Error) -> SettingsError {
    use sqlx::Error;
    match &e {
        Error::Database(db) if db.is_foreign_key_violation() => {
            SettingsError::Invariant(db.message().to_string())
        }
        Error::Database(db) if db.is_unique_violation() => {
            SettingsError::Conflict(db.message().to_string())
        }
        Error::Database(db) if db.is_check_violation() => {
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

    #[error("settings json error: {0}")]
    Json(#[source] serde_json::Error),

    #[error("settings conflict: {0}")]
    Conflict(String),

    #[error("settings target in use: {0}")]
    InUse(String),

    /// CHECK constraint violation (invalid dialect/tier text).
    /// Indicates a code bug since dialect/tier are typed enums on
    /// the Rust side; surface explicitly so it isn't silently
    /// folded into Database.
    #[error("settings invariant violation: {0}")]
    Invariant(String),
}
