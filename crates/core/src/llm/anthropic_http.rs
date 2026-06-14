use std::time::Duration;

use async_trait::async_trait;
use reqwest::header::CONTENT_TYPE;

use super::{AnthropicClient, LlmError, MessagesRequest, MessagesResponse};

const DEFAULT_ANTHROPIC_BASE_URL: &str = "https://api.anthropic.com";
const DEFAULT_ANTHROPIC_VERSION: &str = "2023-06-01";

#[derive(Debug, Clone)]
pub struct AnthropicHttpClient {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    model_id: String,
}

impl AnthropicHttpClient {
    /// Build a native Anthropic messages client.
    ///
    /// # Errors
    ///
    /// Returns [`LlmError::Internal`] if the HTTP client cannot be built.
    pub fn new(api_key: impl Into<String>, model_id: impl Into<String>) -> Result<Self, LlmError> {
        Self::with_base_url(DEFAULT_ANTHROPIC_BASE_URL, api_key, model_id)
    }

    /// Build a native Anthropic messages client with an alternate base
    /// URL for tests or local proxies.
    ///
    /// # Errors
    ///
    /// Returns [`LlmError::Internal`] if the HTTP client cannot be built.
    pub fn with_base_url(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        model_id: impl Into<String>,
    ) -> Result<Self, LlmError> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_mins(1))
            .build()
            .map_err(|e| LlmError::Internal(format!("reqwest builder: {e}")))?;
        Ok(Self {
            client,
            base_url: base_url.into(),
            api_key: api_key.into(),
            model_id: model_id.into(),
        })
    }
}

#[async_trait]
impl AnthropicClient for AnthropicHttpClient {
    async fn messages_create(
        &self,
        request: MessagesRequest,
    ) -> Result<MessagesResponse, LlmError> {
        let url = format!("{}/v1/messages", self.base_url.trim_end_matches('/'));
        let res = self
            .client
            .post(url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", DEFAULT_ANTHROPIC_VERSION)
            .header(CONTENT_TYPE, "application/json")
            .json(&request)
            .send()
            .await
            .map_err(|e| LlmError::Llm(format!("HTTP send: {e}")))?;
        let status = res.status();
        let body = res
            .text()
            .await
            .map_err(|e| LlmError::Llm(format!("HTTP body read: {e}")))?;
        if !status.is_success() {
            return Err(LlmError::Llm(format!(
                "Anthropic messages failed with status {status}: {body}"
            )));
        }
        serde_json::from_str(&body)
            .map_err(|e| LlmError::Llm(format!("decode Anthropic messages response: {e}")))
    }

    fn model_id(&self) -> &str {
        &self.model_id
    }
}
