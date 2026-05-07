use std::sync::Arc;

use proxima_core::auth::Credentials;
use proxima_core::error::ProtocolError;
use proxima_core::{
    BindInferenceTierRequest, Engine, InferenceTargetConfig, Owner, RegisterInferenceTargetRequest,
    RemoveInferenceTargetRequest,
};
use tauri::State;
use time::format_description::well_known::Rfc3339;

use crate::commands::engine::{ModelTierTs, tier_from_ts, tier_to_ts};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InferenceTargetConfigTs {
    LocalCli(LocalCliConfigTs),
    RemoteModel(RemoteModelConfigTs),
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct LocalCliConfigTs {
    pub command: String,
    pub profile: Option<String>,
    pub env_overrides: Vec<(String, String)>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct RemoteModelConfigTs {
    pub vendor: String,
    pub dialect: String,
    pub model_id: String,
    pub credentials_ref: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct InferenceTargetTs {
    pub target_ref: String,
    pub config: InferenceTargetConfigTs,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct InferenceTierBindingTs {
    pub tier: ModelTierTs,
    pub target_ref: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct RegisterInferenceTargetTs {
    pub owner: Owner,
    pub target_ref: String,
    pub config: InferenceTargetConfigTs,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct RegisterInferenceTargetOutcomeTs {
    pub target_ref: String,
    pub idempotent_replay: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct ListInferenceTargetsTs {
    pub owner: Owner,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct RemoveInferenceTargetTs {
    pub owner: Owner,
    pub target_ref: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct RemoveInferenceTargetOutcomeTs {
    pub idempotent_replay: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct BindInferenceTierTs {
    pub owner: Owner,
    pub tier: ModelTierTs,
    pub target_ref: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct ListInferenceTierBindingsTs {
    pub owner: Owner,
}

fn config_to_core(config: InferenceTargetConfigTs) -> InferenceTargetConfig {
    match config {
        InferenceTargetConfigTs::LocalCli(local) => {
            InferenceTargetConfig::LocalCli(proxima_core::LocalCliConfig {
                command: local.command,
                profile: local.profile,
                env_overrides: local.env_overrides,
            })
        }
        InferenceTargetConfigTs::RemoteModel(remote) => {
            InferenceTargetConfig::RemoteModel(proxima_core::RemoteModelConfig {
                vendor: remote.vendor,
                dialect: remote.dialect,
                model_id: remote.model_id,
                credentials_ref: remote.credentials_ref,
            })
        }
    }
}

fn config_from_core(config: &InferenceTargetConfig) -> InferenceTargetConfigTs {
    match config {
        InferenceTargetConfig::LocalCli(local) => {
            InferenceTargetConfigTs::LocalCli(LocalCliConfigTs {
                command: local.command.clone(),
                profile: local.profile.clone(),
                env_overrides: local.env_overrides.clone(),
            })
        }
        InferenceTargetConfig::RemoteModel(remote) => {
            InferenceTargetConfigTs::RemoteModel(RemoteModelConfigTs {
                vendor: remote.vendor.clone(),
                dialect: remote.dialect.clone(),
                model_id: remote.model_id.clone(),
                credentials_ref: remote.credentials_ref.clone(),
            })
        }
    }
}

#[tauri::command]
#[specta::specta]
pub async fn register_inference_target(
    engine: State<'_, Arc<Engine>>,
    req: RegisterInferenceTargetTs,
) -> Result<RegisterInferenceTargetOutcomeTs, ProtocolError> {
    let req_bytes = crate::perf::ipc::req_size(&req);
    crate::perf::ipc::record("register_inference_target", req_bytes, async move {
        let core_req = RegisterInferenceTargetRequest {
            owner: req.owner,
            target_ref: req.target_ref,
            config: config_to_core(req.config),
        };
        let out = engine
            .register_inference_target(&Credentials::None, &core_req)
            .await?;
        Ok(RegisterInferenceTargetOutcomeTs {
            target_ref: out.target_ref,
            idempotent_replay: out.idempotent_replay,
        })
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn list_inference_targets(
    engine: State<'_, Arc<Engine>>,
    req: ListInferenceTargetsTs,
) -> Result<Vec<InferenceTargetTs>, ProtocolError> {
    let req_bytes = crate::perf::ipc::req_size(&req);
    crate::perf::ipc::record("list_inference_targets", req_bytes, async move {
        let rows = engine
            .list_inference_targets(&Credentials::None, &req.owner)
            .await?;
        rows.iter()
            .map(|row| {
                Ok(InferenceTargetTs {
                    target_ref: row.target_ref.clone(),
                    config: config_from_core(&row.config),
                    created_at: row
                        .created_at
                        .format(&Rfc3339)
                        .map_err(|e| ProtocolError::internal(e.to_string()))?,
                    updated_at: row
                        .updated_at
                        .format(&Rfc3339)
                        .map_err(|e| ProtocolError::internal(e.to_string()))?,
                })
            })
            .collect()
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn remove_inference_target(
    engine: State<'_, Arc<Engine>>,
    req: RemoveInferenceTargetTs,
) -> Result<RemoveInferenceTargetOutcomeTs, ProtocolError> {
    let req_bytes = crate::perf::ipc::req_size(&req);
    crate::perf::ipc::record("remove_inference_target", req_bytes, async move {
        let core_req = RemoveInferenceTargetRequest {
            owner: req.owner,
            target_ref: req.target_ref,
        };
        let out = engine
            .remove_inference_target(&Credentials::None, &core_req)
            .await?;
        Ok(RemoveInferenceTargetOutcomeTs {
            idempotent_replay: out.idempotent_replay,
        })
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn bind_inference_tier(
    engine: State<'_, Arc<Engine>>,
    req: BindInferenceTierTs,
) -> Result<(), ProtocolError> {
    let req_bytes = crate::perf::ipc::req_size(&req);
    crate::perf::ipc::record("bind_inference_tier", req_bytes, async move {
        let core_req = BindInferenceTierRequest {
            owner: req.owner,
            tier: tier_from_ts(req.tier),
            target_ref: req.target_ref,
        };
        engine
            .bind_inference_tier(&Credentials::None, &core_req)
            .await?;
        Ok(())
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn list_inference_tier_bindings(
    engine: State<'_, Arc<Engine>>,
    req: ListInferenceTierBindingsTs,
) -> Result<Vec<InferenceTierBindingTs>, ProtocolError> {
    let req_bytes = crate::perf::ipc::req_size(&req);
    crate::perf::ipc::record("list_inference_tier_bindings", req_bytes, async move {
        let rows = engine
            .list_inference_tier_bindings(&Credentials::None, &req.owner)
            .await?;
        Ok(rows
            .iter()
            .map(|row| InferenceTierBindingTs {
                tier: tier_to_ts(row.tier),
                target_ref: row.target_ref.clone(),
            })
            .collect())
    })
    .await
}
