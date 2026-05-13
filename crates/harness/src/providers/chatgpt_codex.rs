//! ChatGPT (subscription) `/responses` provider adapter against
//! `chatgpt.com/backend-api/codex`.

use std::time::Duration;

use proxima_codex_auth::{AuthDotJsonPath, CodexAuthResolver, CodexCredentials};
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderName, HeaderValue};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::conversation::{Conversation, ToolSpec};

use super::responses_wire;
use super::{ProviderClient, ProviderError, RoundResult};

#[derive(Debug, Clone)]
pub struct ChatGPTCodexClient {
    pub http: reqwest::Client,
    pub base_url: String,
    pub model_id: String,
    pub reasoning_effort: Option<String>,
    pub auth_json: AuthDotJsonPath,
    pub request_timeout: Duration,
}

impl ChatGPTCodexClient {
    #[must_use]
    pub fn new(base_url: String, model_id: String, auth_json: AuthDotJsonPath) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url,
            model_id,
            reasoning_effort: None,
            auth_json,
            request_timeout: Duration::from_mins(3),
        }
    }

    fn build_headers(&self, creds: &CodexCredentials) -> Result<HeaderMap, ProviderError> {
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", creds.access_token))
                .map_err(|e| ProviderError::Network(format!("invalid access_token: {e}")))?,
        );
        headers.insert(
            HeaderName::from_static("chatgpt-account-id"),
            HeaderValue::from_str(&creds.account_id)
                .map_err(|e| ProviderError::Network(format!("invalid account_id: {e}")))?,
        );
        headers.insert(
            HeaderName::from_static("originator"),
            HeaderValue::from_static("proxima"),
        );
        Ok(headers)
    }

    fn build_body(&self, conv: &Conversation, tools: &[ToolSpec]) -> Value {
        let mut body = json!({
            "model": self.model_id,
            "instructions": conv.system_prompt,
            "input": responses_wire::build_input_array(conv, false),
            "tools": responses_wire::tools_array(tools),
            "tool_choice": "auto",
            "store": false,
            "stream": true,
        });
        if let Some(effort) = &self.reasoning_effort {
            body["reasoning"] = json!({ "effort": effort });
        }
        body
    }
}

#[async_trait::async_trait]
impl ProviderClient for ChatGPTCodexClient {
    async fn tool_round(
        &self,
        conversation: &Conversation,
        tools: &[ToolSpec],
        cancel: CancellationToken,
    ) -> Result<RoundResult, ProviderError> {
        let resolver =
            CodexAuthResolver::new(self.auth_json.clone()).map_err(|_| ProviderError::Auth)?;
        let creds = resolver.resolve().await.map_err(|_| ProviderError::Auth)?;
        let body = self.build_body(conversation, tools);
        let url = format!("{}/responses", self.base_url.trim_end_matches('/'));

        let headers = self.build_headers(&creds)?;
        let request = self
            .http
            .post(url)
            .headers(headers)
            .timeout(self.request_timeout)
            .json(&body)
            .send();
        let resp = tokio::select! {
            result = request => result.map_err(|err| ProviderError::Network(err.to_string()))?,
            () = cancel.cancelled() => return Err(ProviderError::Timeout),
        };

        responses_wire::classify_sse(resp).await
    }
}
