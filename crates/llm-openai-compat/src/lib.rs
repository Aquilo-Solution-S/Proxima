//! OpenAI-compatible HTTP `EmbeddingClient` impls — the reference
//! embedding adapter a host injects into the substrate (see docs/configuration.md).
//! Proxima ships no embedding client of its own; this crate is the
//! canonical one.
//!
//! The surface is intentionally minimal. No retries; failures bubble up
//! as `LlmError::Embed`, `LlmError::EmbedPermanent` when the provider refuses
//! the input itself (HTTP 413/422), or `LlmError::Internal` when the client
//! cannot be built.

use std::time::Duration;

use proxima_core::llm::LlmError;
use proxima_core::{EndpointUrlPolicy, PlaintextHosts, validate_endpoint_url_for_hosts};

pub mod openai_compat;

pub use openai_compat::*;

pub(crate) fn build_client(
    timeout: Duration,
    plaintext_hosts: &PlaintextHosts,
) -> Result<reqwest::Client, LlmError> {
    build_http_client(reqwest::Client::builder().timeout(timeout), plaintext_hosts)
}

fn build_http_client(
    builder: reqwest::ClientBuilder,
    plaintext_hosts: &PlaintextHosts,
) -> Result<reqwest::Client, LlmError> {
    // Redirects can resend the embedding input even when credentials are
    // stripped. Enforce the base URL's transport policy, named plaintext
    // hosts included, before every hop.
    let plaintext_hosts = plaintext_hosts.clone();
    let policy = reqwest::redirect::Policy::custom(move |attempt| {
        if let Err(error) = validate_endpoint_url_for_hosts(
            attempt.url().as_str(),
            EndpointUrlPolicy::AllowLoopbackHttp,
            &plaintext_hosts,
        ) {
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
