//! Settings-side InferenceTarget types and request/response envelopes.
//!
//! Storage holds these in `proxima_core.inference_targets` +
//! `proxima_core.inference_tier_bindings`. Resolution at wake time
//! (next plan) reads them through `Storage::list_inference_targets` /
//! `list_inference_tier_bindings`.

use crate::{ModelTier, Owner};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, specta::Type)]
pub struct InferenceTargetRow {
    pub owner: Owner,
    pub target_ref: String,
    pub config: InferenceTargetConfig,
    pub created_at: time::OffsetDateTime,
    pub updated_at: time::OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, specta::Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InferenceTargetConfig {
    MistralChat(MistralChatConfig),
    #[serde(rename = "openai_chat")]
    OpenAIChat(OpenAIChatConfig),
    #[serde(rename = "openai_responses")]
    OpenAIResponses(OpenAIResponsesConfig),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, specta::Type)]
pub struct MistralChatConfig {
    pub base_url: String,
    pub model_id: String,
    pub api_key_env: String,
    pub temperature: Option<f32>,
    pub max_completion_tokens: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, specta::Type)]
pub struct OpenAIChatConfig {
    pub base_url: String,
    pub model_id: String,
    pub api_key_env: String,
    pub temperature: Option<f32>,
    pub max_completion_tokens: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, specta::Type)]
pub struct OpenAIResponsesConfig {
    pub base_url: String,
    pub model_id: String,
    pub api_key_env: String,
    pub reasoning_effort: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
pub struct InferenceTierBindingRow {
    pub owner: Owner,
    pub tier: ModelTier,
    pub target_ref: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
