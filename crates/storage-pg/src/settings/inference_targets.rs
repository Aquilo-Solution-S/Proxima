//! `InferenceTarget` + `InferenceTierBinding` storage verbs.
//!
//! Register is idempotent only when `(owner, target_ref)` already has an
//! identical kind + JSON config. A differing body is a conflict.

use proxima_core::{
    BindInferenceTierRequest, BindInferenceTierResponse, InferenceTargetConfig,
    InferenceTargetKind, InferenceTargetRow, InferenceTierBindingRow, ModelTier, Owner,
    OwnerPrincipalKind, RegisterInferenceTargetRequest, RegisterInferenceTargetResponse,
    RemoveInferenceTargetRequest, RemoveInferenceTargetResponse,
};
use sqlx::PgPool;

use super::types::{SettingsError, map_sqlx_err_inference_target, owner_triple};

/// Register an inference target for an owner.
///
/// # Errors
/// `SettingsError::Conflict` if the same `target_ref` exists with a
/// different body. `SettingsError::Database` for connectivity failures.
pub async fn register_inference_target(
    pool: &PgPool,
    req: &RegisterInferenceTargetRequest,
) -> Result<RegisterInferenceTargetResponse, SettingsError> {
    let owner = req.owner();
    let (owner_kind, owner_principal_id, owner_org_id) = owner_triple(&owner);
    let kind = req.config.kind();
    let config_json = serde_json::to_value(&req.config).map_err(SettingsError::Json)?;

    let existing = sqlx::query!(
        r#"SELECT kind AS "kind: InferenceTargetKind", config
             FROM proxima_core.inference_targets
             WHERE owner_principal_kind = $1
               AND owner_principal_id = $2
               AND owner_org_id = $3
               AND target_ref = $4"#,
        owner_kind as OwnerPrincipalKind,
        owner_principal_id,
        owner_org_id,
        &req.target_ref,
    )
    .fetch_optional(pool)
    .await
    .map_err(SettingsError::Database)?;

    if let Some(row) = existing {
        if row.kind == kind && row.config == config_json {
            return Ok(RegisterInferenceTargetResponse {
                target_ref: req.target_ref.clone(),
                idempotent_replay: true,
            });
        }
        return Err(SettingsError::Conflict(format!(
            "target_ref `{}` already exists with a different body",
            req.target_ref
        )));
    }

    sqlx::query!(
        r#"INSERT INTO proxima_core.inference_targets
            (owner_principal_kind, owner_principal_id, owner_org_id,
             target_ref, kind, config)
         VALUES ($1, $2, $3, $4, $5, $6)"#,
        owner_kind as OwnerPrincipalKind,
        owner_principal_id,
        owner_org_id,
        &req.target_ref,
        kind as InferenceTargetKind,
        config_json,
    )
    .execute(pool)
    .await
    .map_err(map_sqlx_err_inference_target)?;

    Ok(RegisterInferenceTargetResponse {
        target_ref: req.target_ref.clone(),
        idempotent_replay: false,
    })
}

/// List all inference targets for an owner.
///
/// # Errors
/// `SettingsError::Database` for connectivity failures.
pub async fn list_inference_targets(
    pool: &PgPool,
    owner: &Owner,
) -> Result<Vec<InferenceTargetRow>, SettingsError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_triple(owner);
    let rows = sqlx::query!(
        r#"SELECT target_ref, config, created_at, updated_at
             FROM proxima_core.inference_targets
             WHERE owner_principal_kind = $1
               AND owner_principal_id = $2
               AND owner_org_id = $3
             ORDER BY target_ref"#,
        owner_kind as OwnerPrincipalKind,
        owner_principal_id,
        owner_org_id,
    )
    .fetch_all(pool)
    .await
    .map_err(SettingsError::Database)?;

    rows.into_iter()
        .map(|row| {
            let config: InferenceTargetConfig =
                serde_json::from_value(row.config).map_err(SettingsError::Json)?;
            Ok(InferenceTargetRow {
                owner: owner.clone(),
                target_ref: row.target_ref,
                config,
                created_at: row.created_at,
                updated_at: row.updated_at,
            })
        })
        .collect()
}

/// Remove an inference target if no active references remain.
///
/// # Errors
/// `SettingsError::InUse` when a tier binding or active wake entry uses
/// this target. `SettingsError::Database` for connectivity failures.
pub async fn remove_inference_target(
    pool: &PgPool,
    req: &RemoveInferenceTargetRequest,
) -> Result<RemoveInferenceTargetResponse, SettingsError> {
    let owner = req.owner();
    let (owner_kind, owner_principal_id, owner_org_id) = owner_triple(&owner);

    let tiers = sqlx::query_scalar!(
        r#"SELECT tier AS "tier: ModelTier"
             FROM proxima_core.inference_tier_bindings
             WHERE owner_principal_kind = $1
               AND owner_principal_id = $2
               AND owner_org_id = $3
               AND target_ref = $4"#,
        owner_kind as OwnerPrincipalKind,
        owner_principal_id,
        owner_org_id,
        &req.target_ref,
    )
    .fetch_all(pool)
    .await
    .map_err(SettingsError::Database)?;

    if !tiers.is_empty() {
        let tier_strs: Vec<String> = tiers.iter().map(|t| format!("{t:?}")).collect();
        return Err(SettingsError::InUse(format!(
            "target `{}` still bound to tiers: {}",
            req.target_ref,
            tier_strs.join(", ")
        )));
    }

    let result = sqlx::query!(
        r#"DELETE FROM proxima_core.inference_targets
             WHERE owner_principal_kind = $1
               AND owner_principal_id = $2
               AND owner_org_id = $3
               AND target_ref = $4"#,
        owner_kind as OwnerPrincipalKind,
        owner_principal_id,
        owner_org_id,
        &req.target_ref,
    )
    .execute(pool)
    .await
    .map_err(SettingsError::Database)?;

    Ok(RemoveInferenceTargetResponse {
        idempotent_replay: result.rows_affected() == 0,
    })
}

/// Bind an inference tier to a registered target.
///
/// # Errors
/// `SettingsError::Database` for connectivity failures or FK failures.
pub async fn bind_inference_tier(
    pool: &PgPool,
    req: &BindInferenceTierRequest,
) -> Result<BindInferenceTierResponse, SettingsError> {
    let owner = req.owner();
    let (owner_kind, owner_principal_id, owner_org_id) = owner_triple(&owner);
    sqlx::query!(
        r#"INSERT INTO proxima_core.inference_tier_bindings
            (owner_principal_kind, owner_principal_id, owner_org_id, tier, target_ref)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (owner_principal_kind, owner_principal_id, owner_org_id, tier)
         DO UPDATE SET target_ref = EXCLUDED.target_ref, bound_at = now()"#,
        owner_kind as OwnerPrincipalKind,
        owner_principal_id,
        owner_org_id,
        req.tier as ModelTier,
        &req.target_ref,
    )
    .execute(pool)
    .await
    .map_err(map_sqlx_err_inference_target)?;

    Ok(BindInferenceTierResponse {})
}

/// Unbind a model tier for an owner.
///
/// # Errors
/// `SettingsError::Database` for connectivity failures.
pub async fn unbind_inference_tier(
    pool: &PgPool,
    owner: &Owner,
    tier: ModelTier,
) -> Result<(), SettingsError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_triple(owner);
    sqlx::query!(
        r#"DELETE FROM proxima_core.inference_tier_bindings
             WHERE owner_principal_kind = $1
               AND owner_principal_id = $2
               AND owner_org_id = $3
               AND tier = $4"#,
        owner_kind as OwnerPrincipalKind,
        owner_principal_id,
        owner_org_id,
        tier as ModelTier,
    )
    .execute(pool)
    .await
    .map_err(SettingsError::Database)?;
    Ok(())
}

/// List inference tier bindings for an owner.
///
/// # Errors
/// `SettingsError::Database` for connectivity failures.
pub async fn list_inference_tier_bindings(
    pool: &PgPool,
    owner: &Owner,
) -> Result<Vec<InferenceTierBindingRow>, SettingsError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_triple(owner);
    let rows = sqlx::query!(
        r#"SELECT tier AS "tier: ModelTier", target_ref
             FROM proxima_core.inference_tier_bindings
             WHERE owner_principal_kind = $1
               AND owner_principal_id = $2
               AND owner_org_id = $3
             ORDER BY tier"#,
        owner_kind as OwnerPrincipalKind,
        owner_principal_id,
        owner_org_id,
    )
    .fetch_all(pool)
    .await
    .map_err(SettingsError::Database)?;

    rows.into_iter()
        .map(|row| {
            Ok(InferenceTierBindingRow {
                owner: owner.clone(),
                tier: row.tier,
                target_ref: row.target_ref,
            })
        })
        .collect()
}
