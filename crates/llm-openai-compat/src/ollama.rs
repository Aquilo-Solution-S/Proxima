use std::time::Duration;

use async_trait::async_trait;
use proxima_core::operators::{EmbeddingClient, LlmClient, OperatorError};
use serde::{Deserialize, Serialize};

use crate::{DEFAULT_BASE_URL, build_client};

#[derive(Debug, Clone)]
pub struct OllamaConfig {
    pub base_url: String,
    pub timeout: Duration,
}

impl Default for OllamaConfig {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_string(),
            timeout: Duration::from_mins(2),
        }
    }
}

impl OllamaConfig {
    /// Read `OLLAMA_URL` from env (fallback `DEFAULT_BASE_URL`).
    #[must_use]
    pub fn from_env() -> Self {
        let base_url = std::env::var("OLLAMA_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.to_string());
        Self {
            base_url,
            ..Self::default()
        }
    }
}

// =====================================================================
// LLM client — /api/chat with format: "json"
// =====================================================================

#[derive(Debug, Clone)]
pub struct OllamaLlmClient {
    config: OllamaConfig,
    client: reqwest::Client,
    model_id: String,
}

impl OllamaLlmClient {
    /// Construct with explicit `model_id` and config.
    ///
    /// # Errors
    ///
    /// Returns `OperatorError::Internal` if the underlying reqwest
    /// client cannot be built.
    pub fn new(model_id: impl Into<String>, config: OllamaConfig) -> Result<Self, OperatorError> {
        let client = build_client(config.timeout)?;
        Ok(Self {
            config,
            client,
            model_id: model_id.into(),
        })
    }

    /// Convenience: read `OLLAMA_URL` from env, take the `model_id` explicitly.
    ///
    /// # Errors
    ///
    /// Returns `OperatorError::Internal` if the underlying reqwest
    /// client cannot be built.
    pub fn from_env(model_id: impl Into<String>) -> Result<Self, OperatorError> {
        Self::new(model_id, OllamaConfig::from_env())
    }
}

#[derive(Serialize)]
struct ChatMessage<'a> {
    role: &'static str,
    content: &'a str,
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: Vec<ChatMessage<'a>>,
    stream: bool,
    format: &'static str,
}

#[derive(Deserialize)]
struct ChatResponse {
    message: ChatResponseMessage,
}

#[derive(Deserialize)]
struct ChatResponseMessage {
    content: String,
}

#[async_trait]
impl LlmClient for OllamaLlmClient {
    async fn complete_json(
        &self,
        system_prompt: &str,
        user_prompt: &str,
    ) -> Result<serde_json::Value, OperatorError> {
        let url = format!("{}/api/chat", self.config.base_url);
        let body = ChatRequest {
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
            format: "json",
        };

        let resp = self
            .client
            .post(&url)
            .json(&body)
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
                "ollama /api/chat returned {status}: {text}"
            )));
        }

        let parsed: ChatResponse = serde_json::from_str(&text).map_err(|e| {
            OperatorError::Llm(format!("decode ollama envelope: {e}; body: {text}"))
        })?;

        // The model's `content` is a JSON string under `format: "json"`.
        let json_value: serde_json::Value =
            serde_json::from_str(&parsed.message.content).map_err(|e| {
                OperatorError::Llm(format!(
                    "decode model JSON: {e}; content: {}",
                    parsed.message.content
                ))
            })?;

        Ok(json_value)
    }

    fn model_id(&self) -> &str {
        &self.model_id
    }
}

// =====================================================================
// Embedding client — /api/embed
// =====================================================================

#[derive(Debug, Clone)]
pub struct OllamaEmbeddingClient {
    config: OllamaConfig,
    client: reqwest::Client,
    model_id: String,
    dim: usize,
}

impl OllamaEmbeddingClient {
    /// Construct without verifying the dim against the running
    /// Ollama instance — caller asserts the dim from spec
    /// (e.g. 4096 for `qwen3-embedding:8b`).
    ///
    /// # Errors
    ///
    /// Returns `OperatorError::Internal` if the reqwest client
    /// cannot be built.
    pub fn new(
        model_id: impl Into<String>,
        dim: usize,
        config: OllamaConfig,
    ) -> Result<Self, OperatorError> {
        let client = build_client(config.timeout)?;
        Ok(Self {
            config,
            client,
            model_id: model_id.into(),
            dim,
        })
    }

    /// Convenience: read `OLLAMA_URL` from env.
    ///
    /// # Errors
    ///
    /// Returns `OperatorError::Internal` if the reqwest client
    /// cannot be built.
    pub fn from_env(model_id: impl Into<String>, dim: usize) -> Result<Self, OperatorError> {
        Self::new(model_id, dim, OllamaConfig::from_env())
    }
}

#[derive(Serialize)]
struct EmbedRequest<'a> {
    model: &'a str,
    input: &'a str,
    /// Matryoshka truncation target. `OpenAI` `/embeddings` and Ollama's
    /// OpenAI-compatible endpoint honor this for nested-prefix models
    /// (qwen3-embedding, text-embedding-3-*, etc.). Omitted for
    /// non-Matryoshka models — some servers reject the field.
    #[serde(skip_serializing_if = "Option::is_none")]
    dimensions: Option<u32>,
}

#[derive(Deserialize)]
struct EmbedResponse {
    embeddings: Vec<Vec<f32>>,
}

#[async_trait]
impl EmbeddingClient for OllamaEmbeddingClient {
    async fn embed(&self, text: &str) -> Result<Vec<f32>, OperatorError> {
        let url = format!("{}/api/embed", self.config.base_url);
        let body = EmbedRequest {
            model: &self.model_id,
            input: text,
            dimensions: None,
        };

        let resp = self
            .client
            .post(&url)
            .json(&body)
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
                "ollama /api/embed returned {status}: {text_body}"
            )));
        }

        let parsed: EmbedResponse = serde_json::from_str(&text_body).map_err(|e| {
            OperatorError::Embed(format!("decode ollama envelope: {e}; body: {text_body}"))
        })?;

        let vec = parsed
            .embeddings
            .into_iter()
            .next()
            .ok_or_else(|| OperatorError::Embed("ollama returned no embeddings".into()))?;

        if vec.len() != self.dim {
            return Err(OperatorError::Embed(format!(
                "expected dim {}, got {}",
                self.dim,
                vec.len()
            )));
        }

        Ok(vec)
    }

    fn model_id(&self) -> &str {
        &self.model_id
    }

    fn dim(&self) -> usize {
        self.dim
    }
}
