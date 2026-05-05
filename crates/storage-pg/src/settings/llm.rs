use proxima_core::Owner;
use proxima_core::models::LlmCaps;
use sqlx::{PgPool, Row};

use super::types::{
    LlmModel, SettingsError, dialect_to_str, map_sqlx_err, owner_triple, str_to_dialect,
};

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
