//! Settings-side InferenceTarget types and request/response envelopes.
//!
//! Storage holds these in `proxima_core.inference_targets` +
//! `proxima_core.inference_tier_bindings`. Resolution at wake time
//! (next plan) reads them through `Storage::list_inference_targets` /
//! `list_inference_tier_bindings`.

use crate::{ModelTier, Owner};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
pub struct InferenceTargetRow {
    pub owner: Owner,
    pub target_ref: String,
    pub config: InferenceTargetConfig,
    pub created_at: time::OffsetDateTime,
    pub updated_at: time::OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InferenceTargetConfig {
    LocalCli(LocalCliConfig),
    RemoteModel(RemoteModelConfig),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
pub struct LocalCliConfig {
    pub command: String,
    pub profile: Option<String>,
    pub env_overrides: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
pub struct RemoteModelConfig {
    pub vendor: String,
    pub dialect: String,
    pub model_id: String,
    pub credentials_ref: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
pub struct InferenceTierBindingRow {
    pub owner: Owner,
    pub tier: ModelTier,
    pub target_ref: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisterInferenceTargetRequest {
    pub owner: Owner,
    pub target_ref: String,
    pub config: InferenceTargetConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisterInferenceTargetResponse {
    pub target_ref: String,
    pub idempotent_replay: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListInferenceTargetsRequest {
    pub owner: Owner,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoveInferenceTargetRequest {
    pub owner: Owner,
    pub target_ref: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoveInferenceTargetResponse {
    pub idempotent_replay: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindInferenceTierRequest {
    pub owner: Owner,
    pub tier: ModelTier,
    pub target_ref: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindInferenceTierResponse {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListInferenceTierBindingsRequest {
    pub owner: Owner,
}
