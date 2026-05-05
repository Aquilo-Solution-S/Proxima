//! Per-Owner settings registration — runtime model/tier/embedding
//! state. Backs the desktop-shell Tauri commands at S1.e.2.
//!
//! Tables: `proxima_core.llm_models`, `embedding_models`,
//! `tier_bindings`, `embedding_active` (migration m6.20).
//!
//! Not on the `Storage` wire trait — settings are a desktop/admin
//! concern, not a verb in docs/14. Methods are free functions taking
//! `&PgPool`; `PgStorage` exposes thin wrapper methods in lib.rs.

use proxima_core::models::{Dialect, EmbedCaps, LlmCaps, ModelTier};
use proxima_core::{Owner, Principal};
use sqlx::{PgPool, Row};
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
fn tier_to_str(t: ModelTier) -> &'static str {
    match t {
        ModelTier::Fast => "fast",
        ModelTier::Standard => "standard",
        ModelTier::Deep => "deep",
    }
}

/// Helper: lowercase string → `ModelTier`.
fn str_to_tier(s: &str) -> Result<ModelTier, SettingsError> {
    match s {
        "fast" => Ok(ModelTier::Fast),
        "standard" => Ok(ModelTier::Standard),
        "deep" => Ok(ModelTier::Deep),
        _ => Err(SettingsError::Invariant(format!(
            "unknown tier '{s}'"
        ))),
    }
}

/// Helper: `Dialect` → lowercase string for DB.
#[must_use]
fn dialect_to_str(d: Dialect) -> &'static str {
    match d {
        Dialect::Anthropic => "anthropic",
        Dialect::OpenAI => "openai",
    }
}

/// Helper: lowercase string → `Dialect`.
fn str_to_dialect(s: &str) -> Result<Dialect, SettingsError> {
    match s {
        "anthropic" => Ok(Dialect::Anthropic),
        "openai" => Ok(Dialect::OpenAI),
        _ => Err(SettingsError::Invariant(format!(
            "unknown dialect '{s}'"
        ))),
    }
}

/// Helper: decompose Owner into the triple used in WHERE clauses.
fn owner_triple(owner: &Owner) -> (&'static str, Uuid, Uuid) {
    let (kind, principal_id) = match &owner.principal {
        Principal::User(u) => ("User", u.into_inner()),
        Principal::Group(g) => ("Group", g.into_inner()),
    };
    (kind, principal_id, owner.org_id.into_inner())
}

/// Helper: map `sqlx::Error` to `SettingsError` with context.
fn map_sqlx_err(
    e: sqlx::Error,
    vendor: Option<String>,
    model_id: Option<String>,
) -> SettingsError {
    use sqlx::Error;
    match &e {
        Error::Database(db) if db.is_unique_violation() => {
            if let (Some(v), Some(m)) = (vendor, model_id) {
                SettingsError::DuplicateLlmModel { vendor: v, model_id: m }
            } else {
                SettingsError::Database(e)
            }
        }
        Error::Database(db) if db.is_foreign_key_violation() => {
            if let (Some(v), Some(m)) = (vendor, model_id) {
                SettingsError::UnknownLlmModel { vendor: v, model_id: m }
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
fn map_sqlx_err_embedding(
    e: sqlx::Error,
    vendor: Option<String>,
    model_id: Option<String>,
) -> SettingsError {
    use sqlx::Error;
    match &e {
        Error::Database(db) if db.is_unique_violation() => {
            if let (Some(v), Some(m)) = (vendor, model_id) {
                SettingsError::DuplicateEmbeddingModel { vendor: v, model_id: m }
            } else {
                SettingsError::Database(e)
            }
        }
        Error::Database(db) if db.is_foreign_key_violation() => {
            if let (Some(v), Some(m)) = (vendor, model_id) {
                SettingsError::UnknownEmbeddingModel { vendor: v, model_id: m }
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

/// List all LLM models for an owner.
///
/// # Errors
/// `SettingsError::Database` for connectivity failures.
/// `SettingsError::Invariant` if a row has an unrecognized dialect.
pub async fn list_llm_models(pool: &PgPool, owner: &Owner) -> Result<Vec<LlmModel>, SettingsError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_triple(owner);

    let rows = sqlx::query(
        "SELECT vendor, model_id, dialect, base_url, \
               caps_tool_use, caps_json_mode, caps_long_context, caps_vision, secret_ref \
        FROM proxima_core.llm_models \
        WHERE owner_principal_kind = $1 AND owner_principal_id = $2 AND owner_org_id = $3 \
        ORDER BY vendor, model_id",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .fetch_all(pool)
    .await
    .map_err(SettingsError::Database)?;

    rows.into_iter()
        .map(|row| {
            let dialect = str_to_dialect(&row.get::<String, _>("dialect"))?;
            Ok(LlmModel {
                vendor: row.get("vendor"),
                model_id: row.get("model_id"),
                dialect,
                base_url: row.get("base_url"),
                caps: LlmCaps {
                    tool_use: row.get("caps_tool_use"),
                    json_mode: row.get("caps_json_mode"),
                    long_context: row.get("caps_long_context"),
                    vision: row.get("caps_vision"),
                },
                secret_ref: row.get("secret_ref"),
            })
        })
        .collect()
}

/// List all embedding models for an owner.
///
/// # Errors
/// `SettingsError::Database` for connectivity failures.
pub async fn list_embedding_models(
    pool: &PgPool,
    owner: &Owner,
) -> Result<Vec<EmbeddingModel>, SettingsError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_triple(owner);

    let rows = sqlx::query(
        "SELECT vendor, model_id, base_url, caps_dim, caps_matryoshka, secret_ref \
        FROM proxima_core.embedding_models \
        WHERE owner_principal_kind = $1 AND owner_principal_id = $2 AND owner_org_id = $3 \
        ORDER BY vendor, model_id",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .fetch_all(pool)
    .await
    .map_err(SettingsError::Database)?;

    rows.into_iter()
        .map(|row| {
            // CHECK ensures > 0
            let caps_dim: i32 = row.get("caps_dim");
            Ok(EmbeddingModel {
                vendor: row.get("vendor"),
                model_id: row.get("model_id"),
                base_url: row.get("base_url"),
                caps: EmbedCaps {
                    dim: caps_dim.cast_unsigned(),
                    matryoshka: row.get("caps_matryoshka"),
                },
                secret_ref: row.get("secret_ref"),
            })
        })
        .collect()
}

/// List all tier bindings for an owner.
/// Returns (tier, vendor, `model_id`) triples.
///
/// # Errors
/// `SettingsError::Database` for connectivity failures.
/// `SettingsError::Invariant` if a row has an unrecognized tier.
pub async fn list_tier_bindings(
    pool: &PgPool,
    owner: &Owner,
) -> Result<Vec<(ModelTier, String, String)>, SettingsError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_triple(owner);

    let rows = sqlx::query(
        "SELECT tier, vendor, model_id \
        FROM proxima_core.tier_bindings \
        WHERE owner_principal_kind = $1 AND owner_principal_id = $2 AND owner_org_id = $3 \
        ORDER BY tier",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .fetch_all(pool)
    .await
    .map_err(SettingsError::Database)?;

    rows.into_iter()
        .map(|row| {
            let tier = str_to_tier(&row.get::<String, _>("tier"))?;
            Ok((tier, row.get("vendor"), row.get("model_id")))
        })
        .collect()
}

/// Get the active embedding model for an owner.
/// Returns (vendor, `model_id`) or None.
///
/// # Errors
/// `SettingsError::Database` for connectivity failures.
pub async fn get_embedding_active(
    pool: &PgPool,
    owner: &Owner,
) -> Result<Option<(String, String)>, SettingsError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_triple(owner);

    let row: Option<(String, String)> = sqlx::query_as(
        "SELECT vendor, model_id \
        FROM proxima_core.embedding_active \
        WHERE owner_principal_kind = $1 AND owner_principal_id = $2 AND owner_org_id = $3",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .fetch_optional(pool)
    .await
    .map_err(SettingsError::Database)?;

    Ok(row)
}

/// Register an LLM model for an owner.
///
/// # Errors
/// `SettingsError::Database` for connectivity failures.
/// `SettingsError::DuplicateLlmModel` if (vendor, `model_id`) already exists.
/// `SettingsError::Invariant` for CHECK violations (should not happen).
pub async fn register_llm_model(
    pool: &PgPool,
    owner: &Owner,
    m: LlmModel,
) -> Result<(), SettingsError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_triple(owner);

    sqlx::query(
        "INSERT INTO proxima_core.llm_models \
            (owner_principal_kind, owner_principal_id, owner_org_id, \
             vendor, model_id, dialect, base_url, \
             caps_tool_use, caps_json_mode, caps_long_context, caps_vision, secret_ref) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(&m.vendor)
    .bind(&m.model_id)
    .bind(dialect_to_str(m.dialect))
    .bind(&m.base_url)
    .bind(m.caps.tool_use)
    .bind(m.caps.json_mode)
    .bind(m.caps.long_context)
    .bind(m.caps.vision)
    .bind(m.secret_ref)
    .execute(pool)
    .await
    .map_err(|e| map_sqlx_err(e, Some(m.vendor.clone()), Some(m.model_id.clone())))?;

    Ok(())
}

/// Register an embedding model for an owner.
///
/// # Errors
/// `SettingsError::Database` for connectivity failures.
/// `SettingsError::DuplicateEmbeddingModel` if (vendor, `model_id`) already exists.
pub async fn register_embedding_model(
    pool: &PgPool,
    owner: &Owner,
    m: EmbeddingModel,
) -> Result<(), SettingsError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_triple(owner);

    sqlx::query(
        "INSERT INTO proxima_core.embedding_models \
            (owner_principal_kind, owner_principal_id, owner_org_id, \
             vendor, model_id, base_url, caps_dim, caps_matryoshka, secret_ref) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(&m.vendor)
    .bind(&m.model_id)
    .bind(&m.base_url)
    .bind(m.caps.dim.cast_signed())
    .bind(m.caps.matryoshka)
    .bind(m.secret_ref)
    .execute(pool)
    .await
    .map_err(|e| {
        map_sqlx_err_embedding(e, Some(m.vendor.clone()), Some(m.model_id.clone()))
    })?;

    Ok(())
}

/// Delete an LLM model for an owner.
/// Returns whether a row existed.
///
/// # Errors
/// `SettingsError::Database` for connectivity failures.
pub async fn delete_llm_model(
    pool: &PgPool,
    owner: &Owner,
    vendor: &str,
    model_id: &str,
) -> Result<bool, SettingsError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_triple(owner);

    let result = sqlx::query(
        "DELETE FROM proxima_core.llm_models \
         WHERE owner_principal_kind = $1 AND owner_principal_id = $2 AND owner_org_id = $3 \
               AND vendor = $4 AND model_id = $5",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(vendor)
    .bind(model_id)
    .execute(pool)
    .await
    .map_err(SettingsError::Database)?;

    Ok(result.rows_affected() > 0)
}

/// Delete an embedding model for an owner.
/// Returns whether a row existed.
///
/// # Errors
/// `SettingsError::Database` for connectivity failures.
pub async fn delete_embedding_model(
    pool: &PgPool,
    owner: &Owner,
    vendor: &str,
    model_id: &str,
) -> Result<bool, SettingsError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_triple(owner);

    let result = sqlx::query(
        "DELETE FROM proxima_core.embedding_models \
         WHERE owner_principal_kind = $1 AND owner_principal_id = $2 AND owner_org_id = $3 \
               AND vendor = $4 AND model_id = $5",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(vendor)
    .bind(model_id)
    .execute(pool)
    .await
    .map_err(SettingsError::Database)?;

    Ok(result.rows_affected() > 0)
}

/// Bind a tier to an (vendor, `model_id`) for an owner.
/// Upserts: if a binding for this tier already exists, it is updated.
///
/// # Errors
/// `SettingsError::Database` for connectivity failures.
/// `SettingsError::UnknownLlmModel` if (vendor, `model_id`) is not registered.
/// `SettingsError::Invariant` for CHECK violations (should not happen).
pub async fn bind_tier(
    pool: &PgPool,
    owner: &Owner,
    tier: ModelTier,
    vendor: &str,
    model_id: &str,
) -> Result<(), SettingsError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_triple(owner);

    sqlx::query(
        "INSERT INTO proxima_core.tier_bindings \
            (owner_principal_kind, owner_principal_id, owner_org_id, \
             tier, vendor, model_id) \
         VALUES ($1, $2, $3, $4, $5, $6) \
         ON CONFLICT (owner_principal_kind, owner_principal_id, owner_org_id, tier) \
         DO UPDATE SET vendor = EXCLUDED.vendor, model_id = EXCLUDED.model_id, bound_at = now()",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(tier_to_str(tier))
    .bind(vendor)
    .bind(model_id)
    .execute(pool)
    .await
    .map_err(|e| map_sqlx_err(e, Some(vendor.to_string()), Some(model_id.to_string())))?;

    Ok(())
}

/// Unbind a tier for an owner.
/// Returns whether a row existed.
///
/// # Errors
/// `SettingsError::Database` for connectivity failures.
pub async fn unbind_tier(
    pool: &PgPool,
    owner: &Owner,
    tier: ModelTier,
) -> Result<bool, SettingsError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_triple(owner);

    let result = sqlx::query(
        "DELETE FROM proxima_core.tier_bindings \
         WHERE owner_principal_kind = $1 AND owner_principal_id = $2 AND owner_org_id = $3 \
               AND tier = $4",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(tier_to_str(tier))
    .execute(pool)
    .await
    .map_err(SettingsError::Database)?;

    Ok(result.rows_affected() > 0)
}

/// Set the active embedding model for an owner.
/// Upserts: if an active model already exists, it is replaced.
///
/// # Errors
/// `SettingsError::Database` for connectivity failures.
/// `SettingsError::UnknownEmbeddingModel` if (vendor, `model_id`) is not registered.
pub async fn set_embedding_active(
    pool: &PgPool,
    owner: &Owner,
    vendor: &str,
    model_id: &str,
) -> Result<(), SettingsError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_triple(owner);

    sqlx::query(
        "INSERT INTO proxima_core.embedding_active \
            (owner_principal_kind, owner_principal_id, owner_org_id, \
             vendor, model_id) \
         VALUES ($1, $2, $3, $4, $5) \
         ON CONFLICT (owner_principal_kind, owner_principal_id, owner_org_id) \
         DO UPDATE SET vendor = EXCLUDED.vendor, model_id = EXCLUDED.model_id, set_at = now()",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(vendor)
    .bind(model_id)
    .execute(pool)
    .await
    .map_err(|e| {
        map_sqlx_err_embedding(e, Some(vendor.to_string()), Some(model_id.to_string()))
    })?;

    Ok(())
}

/// Clear the active embedding model for an owner.
/// Returns whether a row existed.
///
/// # Errors
/// `SettingsError::Database` for connectivity failures.
pub async fn clear_embedding_active(pool: &PgPool, owner: &Owner) -> Result<bool, SettingsError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_triple(owner);

    let result = sqlx::query(
        "DELETE FROM proxima_core.embedding_active \
         WHERE owner_principal_kind = $1 AND owner_principal_id = $2 AND owner_org_id = $3",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .execute(pool)
    .await
    .map_err(SettingsError::Database)?;

    Ok(result.rows_affected() > 0)
}
