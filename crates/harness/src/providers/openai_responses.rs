//! OpenAI `/v1/responses` provider adapter.

use std::time::Duration;

use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::conversation::{Conversation, ToolSpec};

use super::responses_wire;
use super::{ProviderClient, ProviderError, RoundResult};

#[derive(Debug, Clone)]
pub struct OpenAIResponsesClient {
    pub http: reqwest::Client,
    pub base_url: String,
    pub model_id: String,
    pub api_key: String,
    pub reasoning_effort: Option<String>,
    pub request_timeout: Duration,
}

impl OpenAIResponsesClient {
    #[must_use]
    pub fn new(base_url: String, model_id: String, api_key: String) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url,
            model_id,
            api_key,
            reasoning_effort: None,
            request_timeout: Duration::from_mins(3),
        }
    }
}

#[async_trait::async_trait]
impl ProviderClient for OpenAIResponsesClient {
    async fn tool_round(
        &self,
        conversation: &Conversation,
        tools: &[ToolSpec],
        cancel: CancellationToken,
    ) -> Result<RoundResult, ProviderError> {
        let mut body = json!({
            "model": self.model_id,
            "input": responses_wire::build_input_array(conversation, true),
            "tools": responses_wire::tools_array(tools),
            "tool_choice": "auto",
        });
        if let Some(reasoning_effort) = &self.reasoning_effort {
            body["reasoning"] = json!({ "effort": reasoning_effort });
        }

        let url = format!("{}/v1/responses", self.base_url.trim_end_matches('/'));
        let request = self
            .http
            .post(url)
            .bearer_auth(&self.api_key)
            .timeout(self.request_timeout)
            .json(&body)
            .send();

        let resp = tokio::select! {
            result = request => result.map_err(|err| ProviderError::Network(err.to_string()))?,
            () = cancel.cancelled() => return Err(ProviderError::Timeout),
        };

        responses_wire::classify(resp).await
    }
}
