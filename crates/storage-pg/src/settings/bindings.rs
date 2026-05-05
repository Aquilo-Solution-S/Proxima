use proxima_core::Owner;
use proxima_core::models::ModelTier;
use sqlx::{PgPool, Row};

use super::types::{SettingsError, map_sqlx_err, owner_triple, str_to_tier, tier_to_str};

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
