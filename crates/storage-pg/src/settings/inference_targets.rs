//! `InferenceTarget` + `InferenceTierBinding` storage verbs.
//!
//! Register is idempotent only when `(owner, target_ref)` already has an
//! identical kind + JSON config. A differing body is a conflict.

use proxima_core::{
    BindInferenceTierRequest, BindInferenceTierResponse, InferenceTargetConfig, InferenceTargetRow,
    InferenceTierBindingRow, ModelTier, Owner, RegisterInferenceTargetRequest,
    RegisterInferenceTargetResponse, RemoveInferenceTargetRequest, RemoveInferenceTargetResponse,
};
use sqlx::{PgPool, Row};

use super::types::{
    SettingsError, map_sqlx_err_inference_target, owner_triple, str_to_tier, tier_to_str,
};

fn config_kind(config: &InferenceTargetConfig) -> &'static str {
    match config {
        InferenceTargetConfig::LocalCli(_) => "local_cli",
        InferenceTargetConfig::RemoteModel(_) => "remote_model",
    }
}

/// Register an inference target for an owner.
///
/// # Errors
/// `SettingsError::Conflict` if the same `target_ref` exists with a
/// different body. `SettingsError::Database` for connectivity failures.
pub async fn register_inference_target(
    pool: &PgPool,
    req: &RegisterInferenceTargetRequest,
) -> Result<RegisterInferenceTargetResponse, SettingsError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_triple(&req.owner);
    let kind = config_kind(&req.config);
    let config_json = serde_json::to_value(&req.config).map_err(SettingsError::Json)?;

    let existing = sqlx::query(
        "SELECT kind, config
         FROM proxima_core.inference_targets
         WHERE owner_principal_kind = $1
           AND owner_principal_id = $2
           AND owner_org_id = $3
           AND target_ref = $4",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(&req.target_ref)
    .fetch_optional(pool)
    .await
    .map_err(SettingsError::Database)?;

    if let Some(row) = existing {
        let existing_kind: String = row.get("kind");
        let existing_config: serde_json::Value = row.get("config");
        if existing_kind == kind && existing_config == config_json {
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

    sqlx::query(
        "INSERT INTO proxima_core.inference_targets
            (owner_principal_kind, owner_principal_id, owner_org_id,
             target_ref, kind, config)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(&req.target_ref)
    .bind(kind)
    .bind(config_json)
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
    let rows = sqlx::query(
        "SELECT target_ref, config, created_at, updated_at
         FROM proxima_core.inference_targets
         WHERE owner_principal_kind = $1
           AND owner_principal_id = $2
           AND owner_org_id = $3
         ORDER BY target_ref",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .fetch_all(pool)
    .await
    .map_err(SettingsError::Database)?;

    rows.into_iter()
        .map(|row| {
            let config: InferenceTargetConfig =
                serde_json::from_value(row.get("config")).map_err(SettingsError::Json)?;
            Ok(InferenceTargetRow {
                owner: owner.clone(),
                target_ref: row.get("target_ref"),
                config,
                created_at: row.get("created_at"),
                updated_at: row.get("updated_at"),
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
    let (owner_kind, owner_principal_id, owner_org_id) = owner_triple(&req.owner);

    let tiers: Vec<String> = sqlx::query_scalar(
        "SELECT tier
         FROM proxima_core.inference_tier_bindings
         WHERE owner_principal_kind = $1
           AND owner_principal_id = $2
           AND owner_org_id = $3
           AND target_ref = $4",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(&req.target_ref)
    .fetch_all(pool)
    .await
    .map_err(SettingsError::Database)?;

    if !tiers.is_empty() {
        return Err(SettingsError::InUse(format!(
            "target `{}` still bound to tiers: {}",
            req.target_ref,
            tiers.join(", ")
        )));
    }

    let wake_entries: Vec<String> = sqlx::query_scalar(
        "SELECT wake_entry_id::text
         FROM proxima_core.personality_wake_entries
         WHERE owner_principal_kind = $1
           AND owner_principal_id = $2
           AND owner_org_id = $3
           AND inference_target_ref = $4
           AND tombstoned_at IS NULL",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(&req.target_ref)
    .fetch_all(pool)
    .await
    .map_err(SettingsError::Database)?;

    if !wake_entries.is_empty() {
        return Err(SettingsError::InUse(format!(
            "target `{}` still pinned by wake entries: {}",
            req.target_ref,
            wake_entries.join(", ")
        )));
    }

    let result = sqlx::query(
        "DELETE FROM proxima_core.inference_targets
         WHERE owner_principal_kind = $1
           AND owner_principal_id = $2
           AND owner_org_id = $3
           AND target_ref = $4",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(&req.target_ref)
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
    let (owner_kind, owner_principal_id, owner_org_id) = owner_triple(&req.owner);
    sqlx::query(
        "INSERT INTO proxima_core.inference_tier_bindings
            (owner_principal_kind, owner_principal_id, owner_org_id, tier, target_ref)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (owner_principal_kind, owner_principal_id, owner_org_id, tier)
         DO UPDATE SET target_ref = EXCLUDED.target_ref, bound_at = now()",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(tier_to_str(req.tier))
    .bind(&req.target_ref)
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
    sqlx::query(
        "DELETE FROM proxima_core.inference_tier_bindings
         WHERE owner_principal_kind = $1
           AND owner_principal_id = $2
           AND owner_org_id = $3
           AND tier = $4",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(tier_to_str(tier))
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
    let rows = sqlx::query(
        "SELECT tier, target_ref
         FROM proxima_core.inference_tier_bindings
         WHERE owner_principal_kind = $1
           AND owner_principal_id = $2
           AND owner_org_id = $3
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
            Ok(InferenceTierBindingRow {
                owner: owner.clone(),
                tier: str_to_tier(&row.get::<String, _>("tier"))?,
                target_ref: row.get("target_ref"),
            })
        })
        .collect()
}
