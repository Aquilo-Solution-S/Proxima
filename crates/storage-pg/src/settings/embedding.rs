use proxima_core::models::EmbedCaps;
use sqlx::{PgPool, Row};

use super::types::{EmbeddingModel, SettingsError, map_sqlx_err_embedding};

/// List all binary-wide embedding models.
///
/// # Errors
/// `SettingsError::Database` for connectivity failures.
pub async fn list_embedding_models(pool: &PgPool) -> Result<Vec<EmbeddingModel>, SettingsError> {
    let rows = sqlx::query(
        "SELECT vendor, model_id, base_url, caps_dim, caps_matryoshka, secret_ref \
        FROM proxima_core.embedding_models \
        ORDER BY vendor, model_id",
    )
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

/// Get the active binary-wide embedding model.
/// Returns (vendor, `model_id`) or None.
///
/// # Errors
/// `SettingsError::Database` for connectivity failures.
pub async fn get_embedding_active(
    pool: &PgPool,
) -> Result<Option<(String, String)>, SettingsError> {
    let row: Option<(String, String)> = sqlx::query_as(
        "SELECT vendor, model_id \
        FROM proxima_core.embedding_active \
        WHERE singleton",
    )
    .fetch_optional(pool)
    .await
    .map_err(SettingsError::Database)?;

    Ok(row)
}

/// Register a binary-wide embedding model.
///
/// # Errors
/// `SettingsError::Database` for connectivity failures.
/// `SettingsError::DuplicateEmbeddingModel` if (vendor, `model_id`) already exists.
pub async fn register_embedding_model(
    pool: &PgPool,
    m: EmbeddingModel,
) -> Result<(), SettingsError> {
    sqlx::query(
        "INSERT INTO proxima_core.embedding_models \
            (vendor, model_id, base_url, caps_dim, caps_matryoshka, secret_ref) \
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(&m.vendor)
    .bind(&m.model_id)
    .bind(&m.base_url)
    .bind(m.caps.dim.cast_signed())
    .bind(m.caps.matryoshka)
    .bind(m.secret_ref)
    .execute(pool)
    .await
    .map_err(|e| map_sqlx_err_embedding(e, Some(m.vendor.clone()), Some(m.model_id.clone())))?;

    Ok(())
}

/// Delete a binary-wide embedding model.
/// Returns whether a row existed.
///
/// # Errors
/// `SettingsError::Database` for connectivity failures.
pub async fn delete_embedding_model(
    pool: &PgPool,
    vendor: &str,
    model_id: &str,
) -> Result<bool, SettingsError> {
    let result = sqlx::query(
        "DELETE FROM proxima_core.embedding_models \
         WHERE vendor = $1 AND model_id = $2",
    )
    .bind(vendor)
    .bind(model_id)
    .execute(pool)
    .await
    .map_err(SettingsError::Database)?;

    Ok(result.rows_affected() > 0)
}

/// Set the active binary-wide embedding model.
/// Upserts: if an active model already exists, it is replaced.
///
/// # Errors
/// `SettingsError::Database` for connectivity failures.
/// `SettingsError::UnknownEmbeddingModel` if (vendor, `model_id`) is not registered.
pub async fn set_embedding_active(
    pool: &PgPool,
    vendor: &str,
    model_id: &str,
) -> Result<(), SettingsError> {
    sqlx::query(
        "INSERT INTO proxima_core.embedding_active \
            (singleton, vendor, model_id) \
         VALUES (true, $1, $2) \
         ON CONFLICT (singleton) \
         DO UPDATE SET vendor = EXCLUDED.vendor, model_id = EXCLUDED.model_id, set_at = now()",
    )
    .bind(vendor)
    .bind(model_id)
    .execute(pool)
    .await
    .map_err(|e| map_sqlx_err_embedding(e, Some(vendor.to_string()), Some(model_id.to_string())))?;

    Ok(())
}

/// Clear the active binary-wide embedding model.
/// Returns whether a row existed.
///
/// # Errors
/// `SettingsError::Database` for connectivity failures.
pub async fn clear_embedding_active(pool: &PgPool) -> Result<bool, SettingsError> {
    let result = sqlx::query("DELETE FROM proxima_core.embedding_active WHERE singleton")
        .execute(pool)
        .await
        .map_err(SettingsError::Database)?;

    Ok(result.rows_affected() > 0)
}
