use proxima_core::models::{Dialect, EmbedCaps, LlmCaps, ModelTier};
use proxima_core::{Owner, Principal};
use thiserror::Error;
use uuid::Uuid;

/// LLM model row as stored in `proxima_core.llm_models`.
/// Maps 1:1 to a row; use it to insert or to receive the result of
/// a list/get query. Caller-side DTO mapping (e.g. to
/// `LlmModelRecord` in proxima-shell) is the boundary's job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlmModel {
    pub vendor: String,
    pub model_id: String,
    pub dialect: Dialect,
    pub base_url: String,
    pub caps: LlmCaps,
    pub secret_ref: Option<String>,
}

/// Embedding model row as stored in `proxima_core.embedding_models`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingModel {
    pub vendor: String,
    pub model_id: String,
    pub base_url: String,
    pub caps: EmbedCaps,
    pub secret_ref: Option<String>,
}

/// Helper: `ModelTier` → lowercase string for DB.
#[must_use]
pub(super) fn tier_to_str(t: ModelTier) -> &'static str {
    match t {
        ModelTier::Fast => "fast",
        ModelTier::Standard => "standard",
        ModelTier::Deep => "deep",
    }
}

/// Helper: lowercase string → `ModelTier`.
pub(super) fn str_to_tier(s: &str) -> Result<ModelTier, SettingsError> {
    match s {
        "fast" => Ok(ModelTier::Fast),
        "standard" => Ok(ModelTier::Standard),
        "deep" => Ok(ModelTier::Deep),
        _ => Err(SettingsError::Invariant(format!("unknown tier '{s}'"))),
    }
}

/// Helper: `Dialect` → lowercase string for DB.
#[must_use]
pub(super) fn dialect_to_str(d: Dialect) -> &'static str {
    match d {
        Dialect::Anthropic => "anthropic",
        Dialect::OpenAI => "openai",
    }
}

/// Helper: lowercase string → `Dialect`.
pub(super) fn str_to_dialect(s: &str) -> Result<Dialect, SettingsError> {
    match s {
        "anthropic" => Ok(Dialect::Anthropic),
        "openai" => Ok(Dialect::OpenAI),
        _ => Err(SettingsError::Invariant(format!("unknown dialect '{s}'"))),
    }
}

/// Helper: decompose Owner into the triple used in WHERE clauses.
pub(super) fn owner_triple(owner: &Owner) -> (&'static str, Uuid, Uuid) {
    let (kind, principal_id) = match &owner.principal {
        Principal::User(u) => ("User", u.into_inner()),
        Principal::Group(g) => ("Group", g.into_inner()),
    };
    (kind, principal_id, owner.org_id.into_inner())
}

/// Helper: map `sqlx::Error` to `SettingsError` with context.
pub(super) fn map_sqlx_err(
    e: sqlx::Error,
    vendor: Option<String>,
    model_id: Option<String>,
) -> SettingsError {
    use sqlx::Error;
    match &e {
        Error::Database(db) if db.is_unique_violation() => {
            if let (Some(v), Some(m)) = (vendor, model_id) {
                SettingsError::DuplicateLlmModel {
                    vendor: v,
                    model_id: m,
                }
            } else {
                SettingsError::Database(e)
            }
        }
        Error::Database(db) if db.is_foreign_key_violation() => {
            if let (Some(v), Some(m)) = (vendor, model_id) {
                SettingsError::UnknownLlmModel {
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

    #[error("duplicate llm model {vendor:?}/{model_id:?}")]
    DuplicateLlmModel { vendor: String, model_id: String },

    #[error("duplicate embedding model {vendor:?}/{model_id:?}")]
    DuplicateEmbeddingModel { vendor: String, model_id: String },

    /// FK violation when binding a tier — the (vendor, `model_id`)
    /// is not registered in `llm_models` for this owner.
    #[error("unknown llm model {vendor:?}/{model_id:?}")]
    UnknownLlmModel { vendor: String, model_id: String },

    /// FK violation when setting active embedding — the (vendor,
    /// `model_id`) is not in `embedding_models` for this owner.
    #[error("unknown embedding model {vendor:?}/{model_id:?}")]
    UnknownEmbeddingModel { vendor: String, model_id: String },

    /// CHECK constraint violation (invalid dialect/tier text).
    /// Indicates a code bug since dialect/tier are typed enums on
    /// the Rust side; surface explicitly so it isn't silently
    /// folded into Database.
    #[error("settings invariant violation: {0}")]
    Invariant(String),
}
