//! OpenAI-compatible HTTP implementations of
//! `proxima_core::operators::LlmClient` and `EmbeddingClient`.
//!
//! The generic clients use `/chat/completions` and `/embeddings`;
//! registered runtime rows provide `base_url`, `model_id`, and
//! optional bearer credentials. Native Ollama helpers remain for the
//! CLI's local-development path.
//!
//! v1 keeps the surface minimal. No streaming, no tool-call shape, no
//! retries — failures bubble up as `OperatorError::Llm` /
//! `OperatorError::Embed`. Retries land in M6 alongside the
//! dispatcher's worker pool.

use std::time::Duration;

use proxima_core::operators::OperatorError;

pub mod ollama;
pub mod openai_compat;

pub use ollama::*;
pub use openai_compat::*;

pub const DEFAULT_BASE_URL: &str = "http://localhost:11434";

pub(crate) fn build_client(timeout: Duration) -> Result<reqwest::Client, OperatorError> {
    reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|e| OperatorError::Internal(format!("reqwest builder: {e}")))
}

pub(crate) fn join_endpoint(base_url: &str, path: &str) -> String {
    format!(
        "{}/{}",
        base_url.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}
