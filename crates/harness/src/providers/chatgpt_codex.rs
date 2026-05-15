//! `ChatGPT` (subscription) `/responses` provider adapter against
//! `chatgpt.com/backend-api/codex`.

use std::sync::Arc;
use std::time::Duration;

use proxima_codex_auth::{AuthDotJsonPath, CodexAuthError, CodexAuthResolver, CodexCredentials};
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderName, HeaderValue};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::conversation::{Conversation, ToolSpec};

use super::responses_wire;
use super::{ProviderClient, ProviderError, RoundResult};

type ResolverFactory =
    Arc<dyn Fn() -> Result<CodexAuthResolver, CodexAuthError> + Send + Sync + 'static>;

#[derive(Clone)]
pub struct ChatGPTCodexClient {
    pub http: reqwest::Client,
    pub base_url: String,
    pub model_id: String,
    pub reasoning_effort: Option<String>,
    pub auth_json: AuthDotJsonPath,
    pub request_timeout: Duration,
    resolver_factory: Option<ResolverFactory>,
}

impl std::fmt::Debug for ChatGPTCodexClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChatGPTCodexClient")
            .field("base_url", &self.base_url)
            .field("model_id", &self.model_id)
            .field("reasoning_effort", &self.reasoning_effort)
            .field("auth_json", &self.auth_json)
            .field("request_timeout", &self.request_timeout)
            .finish_non_exhaustive()
    }
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
            resolver_factory: None,
        }
    }

    #[must_use]
    pub fn with_resolver_factory<F>(mut self, factory: F) -> Self
    where
        F: Fn() -> Result<CodexAuthResolver, CodexAuthError> + Send + Sync + 'static,
    {
        self.resolver_factory = Some(Arc::new(factory));
        self
    }

    fn build_headers(creds: &CodexCredentials) -> Result<HeaderMap, ProviderError> {
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
            "include": ["reasoning.encrypted_content"],
        });
        if let Some(effort) = &self.reasoning_effort {
            body["reasoning"] = json!({ "effort": effort });
        }
        body
    }

    async fn send(
        &self,
        url: &str,
        creds: &CodexCredentials,
        body: &Value,
        cancel: CancellationToken,
    ) -> Result<reqwest::Response, ProviderError> {
        let headers = Self::build_headers(creds)?;
        let fut = self
            .http
            .post(url)
            .headers(headers)
            .timeout(self.request_timeout)
            .json(body)
            .send();
        tokio::select! {
            result = fut => result.map_err(|err| ProviderError::Network(err.to_string())),
            () = cancel.cancelled() => Err(ProviderError::Timeout),
        }
    }

    fn make_resolver(&self) -> Result<CodexAuthResolver, ProviderError> {
        if let Some(factory) = &self.resolver_factory {
            return factory().map_err(|_| ProviderError::Auth);
        }
        CodexAuthResolver::new(self.auth_json.clone()).map_err(|_| ProviderError::Auth)
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
        let resolver = self.make_resolver()?;
        let creds = resolver.resolve().await.map_err(|_| ProviderError::Auth)?;
        let body = self.build_body(conversation, tools);
        let url = format!("{}/responses", self.base_url.trim_end_matches('/'));

        let resp = self.send(&url, &creds, &body, cancel.clone()).await?;
        if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
            let refreshed = resolver
                .invalidate_and_refresh()
                .await
                .map_err(|_| ProviderError::Auth)?;
            let resp2 = self.send(&url, &refreshed, &body, cancel).await?;
            return responses_wire::classify_sse(resp2).await;
        }
        responses_wire::classify_sse(resp).await
    }
}
