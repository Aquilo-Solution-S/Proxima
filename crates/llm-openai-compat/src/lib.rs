//! OpenAI-compatible HTTP `EmbeddingClient` impls — the reference
//! embedding adapter a host injects into the substrate (see docs/10).
//! Proxima ships no embedding client of its own; this crate is the
//! canonical one. Native Ollama helpers are included as a
//! local-development option.
//!
//! The surface is intentionally minimal. No retries; failures bubble up
//! as `LlmError::Embed`.

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
