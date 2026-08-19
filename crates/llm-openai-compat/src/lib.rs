//! OpenAI-compatible HTTP `EmbeddingClient` impls — the reference
//! embedding adapter a host injects into the substrate (see docs/10).
//! Proxima ships no embedding client of its own; this crate is the
//! canonical one.
//!
//! The surface is intentionally minimal. No retries; failures bubble up
//! as `LlmError::Embed`.

use std::time::Duration;

use proxima_core::llm::LlmError;
use proxima_core::{EndpointUrlPolicy, validate_endpoint_url};

pub mod openai_compat;

pub use openai_compat::*;

pub(crate) fn build_client(timeout: Duration) -> Result<reqwest::Client, LlmError> {
    reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|e| LlmError::Internal(format!("reqwest builder: {e}")))
}

/// Reject non-loopback plaintext embedding endpoints before constructing a
/// client that may attach credentials or sensitive request bodies.
pub(crate) fn ensure_secure_base_url(base_url: &str) -> Result<(), LlmError> {
    validate_endpoint_url(base_url, EndpointUrlPolicy::AllowLoopbackHttp).map_err(|error| {
        LlmError::Internal(format!(
            "invalid or insecure embedding base_url {base_url:?}: {error}; plaintext http is only \
             permitted for loopback hosts"
        ))
    })
}

pub(crate) fn join_endpoint(base_url: &str, path: &str) -> String {
    format!(
        "{}/{}",
        base_url.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}
