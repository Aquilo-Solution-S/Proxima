//! OpenAI-compatible HTTP `EmbeddingClient` impls — the reference
//! embedding adapter a host injects into the substrate (see docs/configuration.md).
//! Proxima ships no embedding client of its own; this crate is the
//! canonical one.
//!
//! The surface is intentionally minimal. No retries; failures bubble up
//! through typed input refusals (HTTP 413/422 or a local cap), configuration
//! faults (HTTP 400/401/403/404), and retryable failures with optional HTTP
//! status and Retry-After delay. Error-body diagnostics are bounded and best-effort;
//! they never delay a known refusal. Client construction returns `LlmError::Internal`.

use std::time::Duration;

use proxima_core::llm::LlmError;
use proxima_core::{EndpointUrlPolicy, PlaintextHosts, validate_endpoint_url_for_hosts};

pub mod openai_compat;

pub use openai_compat::*;

pub(crate) fn build_client(timeout: Duration) -> Result<reqwest::Client, LlmError> {
    build_http_client(reqwest::Client::builder().timeout(timeout))
}

fn build_http_client(builder: reqwest::ClientBuilder) -> Result<reqwest::Client, LlmError> {
    builder
        // An embedding request carries memory content. Following a redirect
        // would resend it to a provider-selected endpoint, so redirects are
        // refused at the HTTP client boundary.
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| LlmError::Internal(format!("reqwest builder: {e}")))
}

/// Append to the path component, preserving provider query parameters.
/// Validate before constructing a client that may send credentials or input.
pub(crate) fn embedding_endpoint(
    base_url: &str,
    plaintext_hosts: &PlaintextHosts,
) -> Result<reqwest::Url, LlmError> {
    validate_endpoint_url_for_hosts(
        base_url,
        EndpointUrlPolicy::AllowLoopbackHttp,
        plaintext_hosts,
    )
    .map_err(|error| {
        // With no host named the message is the one this lane always gave.
        let permitted = if plaintext_hosts.is_empty() {
            "loopback hosts"
        } else {
            "loopback hosts and hosts named in PROXIMA_PLAINTEXT_HOSTS"
        };
        LlmError::Internal(format!(
            "invalid or insecure embedding base_url: {error}; plaintext http is only \
             permitted for {permitted}"
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
