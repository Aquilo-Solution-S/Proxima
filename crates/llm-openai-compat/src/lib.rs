//! OpenAI-compatible HTTP `EmbeddingClient` impls.
//!
//! Substrate operator JSON-mode dispatch was retired in favor of the
//! Anthropic structured tool-call loop, so this crate is now an
//! embedding-only adapter. Native Ollama helpers remain for the CLI's
//! local-development path.
//!
//! v1 keeps the surface minimal. No retries; failures bubble up as
//! `LlmError::Embed`. Retries land alongside the dispatcher's worker
//! pool.

use std::time::Duration;

use proxima_core::llm::LlmError;

pub mod ollama;
pub mod openai_compat;

pub use ollama::*;
pub use openai_compat::*;

pub const DEFAULT_BASE_URL: &str = "http://localhost:11434";

pub(crate) fn build_client(timeout: Duration) -> Result<reqwest::Client, LlmError> {
    reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|e| LlmError::Internal(format!("reqwest builder: {e}")))
}

pub(crate) fn join_endpoint(base_url: &str, path: &str) -> String {
    format!(
        "{}/{}",
        base_url.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}
