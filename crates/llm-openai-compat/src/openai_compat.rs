use std::time::Duration;

use async_trait::async_trait;
use proxima_core::models::EmbedCaps;
use proxima_core::operators::{EmbeddingClient, LlmClient, OperatorError};
use serde::{Deserialize, Serialize};

use crate::{build_client, join_endpoint};

// =====================================================================
// OpenAI-compatible clients — /chat/completions and /embeddings
// =====================================================================

#[derive(Debug, Clone)]
pub struct OpenAiCompatConfig {
    pub base_url: String,
    pub timeout: Duration,
    pub bearer_token: Option<String>,
}

impl OpenAiCompatConfig {
    #[must_use]
    pub fn new(base_url: impl Into<String>, bearer_token: Option<String>) -> Self {
        Self {
            base_url: base_url.into(),
            timeout: Duration::from_mins(10),
            bearer_token,
        }
    }
}

#[derive(Serialize)]
struct ChatMessage<'a> {
    role: &'static str,
    content: &'a str,
}

#[derive(Serialize)]
struct EmbedRequest<'a> {
    model: &'a str,
    input: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    dimensions: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct OpenAiCompatLlmClient {
    config: OpenAiCompatConfig,
    client: reqwest::Client,
    model_id: String,
}

impl OpenAiCompatLlmClient {
    /// Construct an OpenAI-compatible JSON-mode chat client.
    ///
    /// # Errors
    /// Returns `OperatorError::Internal` if the HTTP client cannot be built.
    pub fn new(
        model_id: impl Into<String>,
        config: OpenAiCompatConfig,
    ) -> Result<Self, OperatorError> {
        let client = build_client(config.timeout)?;
        Ok(Self {
            config,
            client,
            model_id: model_id.into(),
        })
    }
}

#[derive(Serialize)]
struct OpenAiResponseFormat {
    #[serde(rename = "type")]
    kind: &'static str,
}

#[derive(Serialize)]
struct OpenAiChatRequest<'a> {
    model: &'a str,
    messages: Vec<ChatMessage<'a>>,
    stream: bool,
    response_format: OpenAiResponseFormat,
}

#[derive(Deserialize)]
struct OpenAiChatResponse {
    choices: Vec<OpenAiChoice>,
}

#[derive(Deserialize)]
struct OpenAiChoice {
    message: OpenAiMessage,
}

#[derive(Deserialize)]
struct OpenAiMessage {
    content: String,
}

#[async_trait]
impl LlmClient for OpenAiCompatLlmClient {
    async fn complete_json(
        &self,
        system_prompt: &str,
        user_prompt: &str,
    ) -> Result<serde_json::Value, OperatorError> {
        let url = join_endpoint(&self.config.base_url, "chat/completions");
        let body = OpenAiChatRequest {
            model: &self.model_id,
            messages: vec![
                ChatMessage {
                    role: "system",
                    content: system_prompt,
                },
                ChatMessage {
                    role: "user",
                    content: user_prompt,
                },
            ],
            stream: false,
            response_format: OpenAiResponseFormat {
                kind: "json_object",
            },
        };

        let mut req = self.client.post(&url).json(&body);
        if let Some(token) = &self.config.bearer_token {
            req = req.bearer_auth(token);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| OperatorError::Llm(format!("HTTP send: {e}")))?;

        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| OperatorError::Llm(format!("HTTP body read: {e}")))?;

        if !status.is_success() {
            return Err(OperatorError::Llm(format!(
                "openai-compatible /chat/completions returned {status}: {text}"
            )));
        }

        let parsed: OpenAiChatResponse = serde_json::from_str(&text).map_err(|e| {
            OperatorError::Llm(format!(
                "decode OpenAI-compatible envelope: {e}; body: {text}"
            ))
        })?;
        let content = parsed
            .choices
            .first()
            .ok_or_else(|| OperatorError::Llm("OpenAI-compatible response had no choices".into()))?
            .message
            .content
            .as_str();
        serde_json::from_str(content)
            .map_err(|e| OperatorError::Llm(format!("decode model JSON: {e}; content: {content}")))
    }

    fn model_id(&self) -> &str {
        &self.model_id
    }
}

#[derive(Debug, Clone)]
pub struct OpenAiCompatEmbeddingClient {
    config: OpenAiCompatConfig,
    client: reqwest::Client,
    model_id: String,
    caps: EmbedCaps,
}

impl OpenAiCompatEmbeddingClient {
    /// Construct an OpenAI-compatible embedding client. Matryoshka caps
    /// drive a `dimensions` parameter on the request so nested-prefix
    /// models (qwen3-embedding, text-embedding-3-*) return vectors at
    /// `caps.dim` rather than the model's native size.
    ///
    /// # Errors
    /// Returns `OperatorError::Internal` if the HTTP client cannot be built.
    pub fn new(
        model_id: impl Into<String>,
        caps: EmbedCaps,
        config: OpenAiCompatConfig,
    ) -> Result<Self, OperatorError> {
        let client = build_client(config.timeout)?;
        Ok(Self {
            config,
            client,
            model_id: model_id.into(),
            caps,
        })
    }
}

#[derive(Deserialize)]
struct OpenAiEmbeddingResponse {
    data: Vec<OpenAiEmbeddingDatum>,
}

#[derive(Deserialize)]
struct OpenAiEmbeddingDatum {
    embedding: Vec<f32>,
}

#[async_trait]
impl EmbeddingClient for OpenAiCompatEmbeddingClient {
    async fn embed(&self, text: &str) -> Result<Vec<f32>, OperatorError> {
        let url = join_endpoint(&self.config.base_url, "embeddings");
        let body = EmbedRequest {
            model: &self.model_id,
            input: text,
            dimensions: self.caps.matryoshka.then_some(self.caps.dim),
        };

        let mut req = self.client.post(&url).json(&body);
        if let Some(token) = &self.config.bearer_token {
            req = req.bearer_auth(token);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| OperatorError::Embed(format!("HTTP send: {e}")))?;

        let status = resp.status();
        let text_body = resp
            .text()
            .await
            .map_err(|e| OperatorError::Embed(format!("HTTP body read: {e}")))?;

        if !status.is_success() {
            return Err(OperatorError::Embed(format!(
                "openai-compatible /embeddings returned {status}: {text_body}"
            )));
        }

        let parsed: OpenAiEmbeddingResponse = serde_json::from_str(&text_body).map_err(|e| {
            OperatorError::Embed(format!(
                "decode OpenAI-compatible envelope: {e}; body: {text_body}"
            ))
        })?;
        let vec = parsed
            .data
            .into_iter()
            .next()
            .ok_or_else(|| {
                OperatorError::Embed("OpenAI-compatible response had no embeddings".into())
            })?
            .embedding;

        let expected = self.dim();
        if vec.len() != expected {
            return Err(OperatorError::Embed(format!(
                "expected dim {} (matryoshka={}), got {}",
                expected,
                self.caps.matryoshka,
                vec.len()
            )));
        }

        Ok(vec)
    }

    fn model_id(&self) -> &str {
        &self.model_id
    }

    fn dim(&self) -> usize {
        self.caps.dim as usize
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn openai_compat_timeout_allows_slow_local_models() {
        let cfg = super::OpenAiCompatConfig::new("http://localhost:11434/v1", None);
        assert_eq!(cfg.timeout, std::time::Duration::from_mins(10));
    }
}
