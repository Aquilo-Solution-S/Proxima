//! Settings-side `InferenceTarget` types and request/response envelopes.
//!
//! Storage holds these in `proxima_core.inference_targets` +
//! `proxima_core.inference_tier_bindings`. Resolution at wake time
//! reads them through `Storage::list_inference_targets` /
//! `list_inference_tier_bindings`.

use crate::{ModelTier, OrgId, Owner, Principal};
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
    #[serde(rename = "chatgpt_codex")]
    ChatGPTCodex(ChatGPTCodexConfig),
}

/// Rust mirror of `proxima_core.inference_target_kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, specta::Type, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(
    type_name = "proxima_core.inference_target_kind",
    rename_all = "snake_case"
)]
pub enum InferenceTargetKind {
    MistralChat,
    #[serde(rename = "openai_chat")]
    #[sqlx(rename = "openai_chat")]
    OpenAIChat,
    #[serde(rename = "openai_responses")]
    #[sqlx(rename = "openai_responses")]
    OpenAIResponses,
    #[serde(rename = "chatgpt_codex")]
    #[sqlx(rename = "chatgpt_codex")]
    ChatGPTCodex,
}

impl InferenceTargetConfig {
    #[must_use]
    pub fn kind(&self) -> InferenceTargetKind {
        match self {
            Self::MistralChat(_) => InferenceTargetKind::MistralChat,
            Self::OpenAIChat(_) => InferenceTargetKind::OpenAIChat,
            Self::OpenAIResponses(_) => InferenceTargetKind::OpenAIResponses,
            Self::ChatGPTCodex(_) => InferenceTargetKind::ChatGPTCodex,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, specta::Type)]
pub struct MistralChatConfig {
    pub base_url: String,
    pub model_id: String,
    pub api_key_env: String,
    pub temperature: Option<f32>,
    pub max_completion_tokens: Option<u32>,
    pub reasoning_effort: Option<String>,
    pub context_window_tokens: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, specta::Type)]
pub struct OpenAIChatConfig {
    pub base_url: String,
    pub model_id: String,
    pub api_key_env: String,
    pub temperature: Option<f32>,
    pub max_completion_tokens: Option<u32>,
    pub context_window_tokens: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, specta::Type)]
pub struct OpenAIResponsesConfig {
    pub base_url: String,
    pub model_id: String,
    pub api_key_env: String,
    pub reasoning_effort: Option<String>,
    pub context_window_tokens: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, specta::Type)]
pub struct ChatGPTCodexConfig {
    pub base_url: String,
    pub model_id: String,
    pub reasoning_effort: Option<String>,
    pub context_window_tokens: Option<u32>,
}

impl ChatGPTCodexConfig {
    pub const DEFAULT_BASE_URL: &'static str = "https://chatgpt.com/backend-api/codex";
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
pub struct InferenceTierBindingRow {
    pub owner: Owner,
    pub tier: ModelTier,
    pub target_ref: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RegisterInferenceTargetRequest {
    pub principal: Principal,
    #[serde(skip)]
    pub org_id: Option<OrgId>,
    pub target_ref: String,
    pub config: InferenceTargetConfig,
}

impl RegisterInferenceTargetRequest {
    /// Reconstructs the storage `Owner` after verb-layer stamping.
    ///
    /// # Panics
    ///
    /// Panics if `stamp_owner` has not populated `org_id` before storage or hash use.
    #[must_use]
    pub fn owner(&self) -> Owner {
        Owner {
            principal: self.principal.clone(),
            org_id: self
                .org_id
                .expect("RegisterInferenceTargetRequest org_id must be stamped before storage use"),
        }
    }

    pub fn stamp_owner(&mut self, stamped: Owner) {
        self.principal = stamped.principal;
        self.org_id = Some(stamped.org_id);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisterInferenceTargetResponse {
    pub target_ref: String,
    pub idempotent_replay: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListInferenceTargetsRequest {
    pub principal: Principal,
    #[serde(skip)]
    pub org_id: Option<OrgId>,
}

impl ListInferenceTargetsRequest {
    /// Reconstructs the storage `Owner` after verb-layer stamping.
    ///
    /// # Panics
    ///
    /// Panics if `stamp_owner` has not populated `org_id` before storage or hash use.
    #[must_use]
    pub fn owner(&self) -> Owner {
        Owner {
            principal: self.principal.clone(),
            org_id: self
                .org_id
                .expect("ListInferenceTargetsRequest org_id must be stamped before storage use"),
        }
    }

    pub fn stamp_owner(&mut self, stamped: Owner) {
        self.principal = stamped.principal;
        self.org_id = Some(stamped.org_id);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoveInferenceTargetRequest {
    pub principal: Principal,
    #[serde(skip)]
    pub org_id: Option<OrgId>,
    pub target_ref: String,
}

impl RemoveInferenceTargetRequest {
    /// Reconstructs the storage `Owner` after verb-layer stamping.
    ///
    /// # Panics
    ///
    /// Panics if `stamp_owner` has not populated `org_id` before storage or hash use.
    #[must_use]
    pub fn owner(&self) -> Owner {
        Owner {
            principal: self.principal.clone(),
            org_id: self
                .org_id
                .expect("RemoveInferenceTargetRequest org_id must be stamped before storage use"),
        }
    }

    pub fn stamp_owner(&mut self, stamped: Owner) {
        self.principal = stamped.principal;
        self.org_id = Some(stamped.org_id);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoveInferenceTargetResponse {
    pub idempotent_replay: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindInferenceTierRequest {
    pub principal: Principal,
    #[serde(skip)]
    pub org_id: Option<OrgId>,
    pub tier: ModelTier,
    pub target_ref: String,
}

impl BindInferenceTierRequest {
    /// Reconstructs the storage `Owner` after verb-layer stamping.
    ///
    /// # Panics
    ///
    /// Panics if `stamp_owner` has not populated `org_id` before storage or hash use.
    #[must_use]
    pub fn owner(&self) -> Owner {
        Owner {
            principal: self.principal.clone(),
            org_id: self
                .org_id
                .expect("BindInferenceTierRequest org_id must be stamped before storage use"),
        }
    }

    pub fn stamp_owner(&mut self, stamped: Owner) {
        self.principal = stamped.principal;
        self.org_id = Some(stamped.org_id);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindInferenceTierResponse {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListInferenceTierBindingsRequest {
    pub principal: Principal,
    #[serde(skip)]
    pub org_id: Option<OrgId>,
}

impl ListInferenceTierBindingsRequest {
    /// Reconstructs the storage `Owner` after verb-layer stamping.
    ///
    /// # Panics
    ///
    /// Panics if `stamp_owner` has not populated `org_id` before storage or hash use.
    #[must_use]
    pub fn owner(&self) -> Owner {
        Owner {
            principal: self.principal.clone(),
            org_id: self.org_id.expect(
                "ListInferenceTierBindingsRequest org_id must be stamped before storage use",
            ),
        }
    }

    pub fn stamp_owner(&mut self, stamped: Owner) {
        self.principal = stamped.principal;
        self.org_id = Some(stamped.org_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chatgpt_codex_roundtrip_through_json() {
        let original = InferenceTargetConfig::ChatGPTCodex(ChatGPTCodexConfig {
            base_url: ChatGPTCodexConfig::DEFAULT_BASE_URL.to_string(),
            model_id: "gpt-5.3-codex".to_string(),
            reasoning_effort: Some("medium".to_string()),
            context_window_tokens: Some(272_000),
        });

        let json = serde_json::to_string(&original).unwrap();
        // Tag-based discriminator should land as "kind":"chatgpt_codex"
        assert!(json.contains(r#""kind":"chatgpt_codex""#));
        assert!(json.contains(r#""model_id":"gpt-5.3-codex""#));

        let parsed: InferenceTargetConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, original);
    }

    #[test]
    fn chatgpt_codex_default_base_url_is_chatgpt_internal_endpoint() {
        assert_eq!(
            ChatGPTCodexConfig::DEFAULT_BASE_URL,
            "https://chatgpt.com/backend-api/codex"
        );
    }
}
