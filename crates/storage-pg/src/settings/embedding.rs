use proxima_core::Owner;
use proxima_core::models::EmbedCaps;
use sqlx::{PgPool, Row};

use super::types::{EmbeddingModel, SettingsError, map_sqlx_err_embedding, owner_triple};

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
    .map_err(|e| map_sqlx_err_embedding(e, Some(m.vendor.clone()), Some(m.model_id.clone())))?;

    Ok(())
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
    .map_err(|e| map_sqlx_err_embedding(e, Some(vendor.to_string()), Some(model_id.to_string())))?;

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
