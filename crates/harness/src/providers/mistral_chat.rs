//! Mistral Chat Completions provider adapter.

use tokio_util::sync::CancellationToken;

use crate::conversation::{Conversation, ToolSpec};

use super::chat_completions_wire::{
    ChatCompletionsRequestOptions, TokenLimitField, build_request, classify_and_parse,
};
use super::{ProviderClient, ProviderError, RoundResult};

#[derive(Debug, Clone)]
pub struct MistralChatClient {
    pub http: reqwest::Client,
    pub base_url: String,
    pub model_id: String,
    pub api_key: String,
    pub temperature: Option<f32>,
    pub max_completion_tokens: Option<u32>,
}

#[async_trait::async_trait]
impl ProviderClient for MistralChatClient {
    async fn tool_round(
        &self,
        conversation: &Conversation,
        tools: &[ToolSpec],
        cancel: CancellationToken,
    ) -> Result<RoundResult, ProviderError> {
        let body = build_request(
            ChatCompletionsRequestOptions {
                model_id: &self.model_id,
                temperature: self.temperature,
                max_completion_tokens: self.max_completion_tokens,
                token_limit_field: TokenLimitField::MaxTokens,
            },
            conversation,
            tools,
        );
        let url = format!(
            "{}/v1/chat/completions",
            self.base_url.trim_end_matches('/')
        );
        let request = self
            .http
            .post(url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send();

        let resp = tokio::select! {
            result = request => result.map_err(|err| ProviderError::Network(err.to_string()))?,
            () = cancel.cancelled() => return Err(ProviderError::Timeout),
        };

        classify_and_parse(resp).await
    }
}
