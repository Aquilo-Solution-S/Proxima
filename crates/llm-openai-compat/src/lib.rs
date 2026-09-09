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
    build_http_client(reqwest::Client::builder().timeout(timeout))
}

fn build_http_client(builder: reqwest::ClientBuilder) -> Result<reqwest::Client, LlmError> {
    // Redirects can resend the embedding input even when credentials are
    // stripped. Enforce the base URL's transport policy before every hop.
    let policy = reqwest::redirect::Policy::custom(|attempt| {
        if let Err(error) =
            validate_endpoint_url(attempt.url().as_str(), EndpointUrlPolicy::AllowLoopbackHttp)
        {
            return attempt.error(error);
        }
        reqwest::redirect::Policy::default().redirect(attempt)
    });
    builder
        .redirect(policy)
        .build()
        .map_err(|e| LlmError::Internal(format!("reqwest builder: {e}")))
}

/// Append to the path component, preserving provider query parameters.
/// Validate before constructing a client that may send credentials or input.
pub(crate) fn embedding_endpoint(base_url: &str) -> Result<reqwest::Url, LlmError> {
    validate_endpoint_url(base_url, EndpointUrlPolicy::AllowLoopbackHttp).map_err(|error| {
        LlmError::Internal(format!(
            "invalid or insecure embedding base_url: {error}; plaintext http is only \
             permitted for loopback hosts"
        ))
    })?;
    let mut url = reqwest::Url::parse(base_url)
        .map_err(|error| LlmError::Internal(format!("invalid embedding base_url: {error}")))?;
    if url.fragment().is_some() {
        return Err(LlmError::Internal(
            "embedding base_url must not contain a fragment".into(),
        ));
    }
    url.set_path(&format!("{}/embeddings", url.path().trim_end_matches('/')));
    Ok(url)
}
