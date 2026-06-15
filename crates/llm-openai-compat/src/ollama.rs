use std::time::Duration;

use async_trait::async_trait;
use proxima_core::llm::{EmbeddingClient, LlmError};
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
    /// Returns `LlmError::Internal` if the reqwest client cannot be built.
    pub fn new(
        model_id: impl Into<String>,
        dim: usize,
        config: OllamaConfig,
    ) -> Result<Self, LlmError> {
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
    /// Returns `LlmError::Internal` if the reqwest client cannot be built.
    pub fn from_env(model_id: impl Into<String>, dim: usize) -> Result<Self, LlmError> {
        Self::new(model_id, dim, OllamaConfig::from_env())
    }
}

#[derive(Serialize)]
struct EmbedRequest<'a> {
    model: &'a str,
    input: &'a str,
}

#[derive(Deserialize)]
struct EmbedResponse {
    embeddings: Vec<Vec<f32>>,
}

#[async_trait]
impl EmbeddingClient for OllamaEmbeddingClient {
    async fn embed(&self, text: &str) -> Result<Vec<f32>, LlmError> {
        let url = format!("{}/api/embed", self.config.base_url);
        let body = EmbedRequest {
            model: &self.model_id,
            input: text,
        };

        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| LlmError::Embed(format!("HTTP send: {e}")))?;

        let status = resp.status();
        let text_body = resp
            .text()
            .await
            .map_err(|e| LlmError::Embed(format!("HTTP body read: {e}")))?;

        if !status.is_success() {
            return Err(LlmError::Embed(format!(
                "ollama /api/embed returned {status}: {text_body}"
            )));
        }

        let parsed: EmbedResponse = serde_json::from_str(&text_body).map_err(|e| {
            LlmError::Embed(format!("decode ollama envelope: {e}; body: {text_body}"))
        })?;

        let vec = parsed
            .embeddings
            .into_iter()
            .next()
            .ok_or_else(|| LlmError::Embed("ollama returned no embeddings".into()))?;

        if vec.len() != self.dim {
            return Err(LlmError::Embed(format!(
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
