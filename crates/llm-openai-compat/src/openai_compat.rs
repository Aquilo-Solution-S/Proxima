use std::time::Duration;

use async_trait::async_trait;
use proxima_core::llm::{EMBEDDING_DIM, EmbeddingClient, LlmError};
use proxima_core::models::EmbedCaps;
use serde::{Deserialize, Serialize};

use crate::{build_client, join_endpoint};

// =====================================================================
// OpenAI-compatible embedding client — /embeddings
// =====================================================================

pub const MISTRAL_EMBED_BASE_URL: &str = "https://api.mistral.ai/v1";
pub const MISTRAL_EMBED_MODEL: &str = "mistral-embed";

/// Default per-request timeout for `/embeddings` calls. Deliberately far
/// shorter than a text-generation timeout: a single embedding is a small,
/// fast request, and the in-process serial drainer must not stall for
/// minutes on one wedged call. Override with
/// [`OpenAiCompatConfig::with_timeout`] for unusually slow local models.
pub const DEFAULT_EMBED_TIMEOUT: Duration = Duration::from_mins(2);

#[derive(Clone)]
pub struct OpenAiCompatConfig {
    pub base_url: String,
    pub timeout: Duration,
    pub bearer_token: Option<String>,
}

// Manual Debug: the bearer token is a secret and must never surface in logs
// or panic messages. Renders `Some("<redacted>")` / `None`; `base_url` and
// `timeout` stay visible for diagnostics. `OpenAiCompatEmbeddingClient` derives
// Debug and holds this config, so it inherits the redaction.
impl std::fmt::Debug for OpenAiCompatConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiCompatConfig")
            .field("base_url", &self.base_url)
            .field("timeout", &self.timeout)
            .field(
                "bearer_token",
                &self.bearer_token.as_ref().map(|_| "<redacted>"),
            )
            .finish()
    }
}

impl OpenAiCompatConfig {
    #[must_use]
    pub fn new(base_url: impl Into<String>, bearer_token: Option<String>) -> Self {
        Self {
            base_url: base_url.into(),
            timeout: DEFAULT_EMBED_TIMEOUT,
            bearer_token,
        }
    }

    /// Override the per-request timeout (e.g. bump it for an unusually slow
    /// local embedding model). Defaults to [`DEFAULT_EMBED_TIMEOUT`].
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

/// Reject plaintext `http://` base URLs unless they target a loopback host.
/// The default local embedding endpoint (Ollama `http://localhost:11434`) is
/// loopback and stays allowed; any non-loopback plaintext base would ship the
/// bearer token in the clear.
fn ensure_secure_base_url(base_url: &str) -> Result<(), LlmError> {
    if let Some(rest) = base_url.strip_prefix("http://") {
        let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
        if !is_loopback_host(host_of_authority(authority)) {
            return Err(LlmError::Internal(format!(
                "insecure embedding base_url {base_url:?}: plaintext http is only \
                 permitted for loopback hosts (use https for remote endpoints)"
            )));
        }
    }
    Ok(())
}

/// Extract the host from an `authority` (`userinfo@host:port`), handling
/// bracketed IPv6 literals (`[::1]:8080`).
fn host_of_authority(authority: &str) -> &str {
    let hostport = authority.rsplit('@').next().unwrap_or(authority);
    if let Some(rest) = hostport.strip_prefix('[') {
        return rest.split(']').next().unwrap_or(rest);
    }
    hostport.split(':').next().unwrap_or(hostport)
}

fn is_loopback_host(host: &str) -> bool {
    host == "localhost"
        || host
            .parse::<std::net::Ipv4Addr>()
            .is_ok_and(|ip| ip.is_loopback())
        || host
            .parse::<std::net::Ipv6Addr>()
            .is_ok_and(|ip| ip.is_loopback())
}

#[derive(Serialize)]
struct EmbedRequest<'a> {
    model: &'a str,
    input: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    dimensions: Option<u32>,
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
    /// Returns `LlmError::Internal` if the HTTP client cannot be built or if
    /// `config.base_url` is a non-loopback plaintext `http://` endpoint (which
    /// would leak the bearer token in transit).
    pub fn new(
        model_id: impl Into<String>,
        caps: EmbedCaps,
        config: OpenAiCompatConfig,
    ) -> Result<Self, LlmError> {
        ensure_secure_base_url(&config.base_url)?;
        let client = build_client(config.timeout)?;
        Ok(Self {
            config,
            client,
            model_id: model_id.into(),
            caps,
        })
    }

    /// Construct a Mistral `/embeddings` client using the OpenAI-compatible
    /// request/response shape.
    ///
    /// # Errors
    /// Returns `LlmError::Internal` if the HTTP client cannot be built.
    pub fn mistral(
        bearer_token: impl Into<String>,
        model_id: impl Into<String>,
        base_url: impl Into<String>,
    ) -> Result<Self, LlmError> {
        let dim = u32::try_from(EMBEDDING_DIM)
            .map_err(|_| LlmError::Internal("EMBEDDING_DIM does not fit u32".into()))?;
        Self::new(
            model_id,
            EmbedCaps {
                matryoshka: false,
                dim,
            },
            OpenAiCompatConfig::new(base_url, Some(bearer_token.into())),
        )
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
    async fn embed(&self, text: &str) -> Result<Vec<f32>, LlmError> {
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
            .map_err(|e| LlmError::Embed(format!("HTTP send: {e}")))?;

        let status = resp.status();
        let text_body = resp
            .text()
            .await
            .map_err(|e| LlmError::Embed(format!("HTTP body read: {e}")))?;

        if !status.is_success() {
            return Err(LlmError::Embed(format!(
                "openai-compatible /embeddings returned {status}: {text_body}"
            )));
        }

        let parsed: OpenAiEmbeddingResponse = serde_json::from_str(&text_body).map_err(|e| {
            LlmError::Embed(format!(
                "decode OpenAI-compatible envelope: {e}; body: {text_body}"
            ))
        })?;
        let vec = parsed
            .data
            .into_iter()
            .next()
            .ok_or_else(|| LlmError::Embed("OpenAI-compatible response had no embeddings".into()))?
            .embedding;

        let expected = self.dim();
        if vec.len() != expected {
            return Err(LlmError::Embed(format!(
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
    use std::time::Duration;

    use proxima_core::llm::LlmError;
    use proxima_core::models::EmbedCaps;

    fn probe_caps() -> EmbedCaps {
        EmbedCaps {
            matryoshka: false,
            dim: 8,
        }
    }

    #[test]
    fn embed_timeout_defaults_to_dedicated_short_window() {
        let cfg = super::OpenAiCompatConfig::new("http://localhost:11434/v1", None);
        assert_eq!(cfg.timeout, super::DEFAULT_EMBED_TIMEOUT);
        assert_eq!(cfg.timeout, Duration::from_mins(2));
        // Far shorter than a generation-style 10-minute window so a single
        // wedged /embeddings call cannot stall the serial drainer for minutes.
        assert!(cfg.timeout < Duration::from_mins(10));
    }

    #[test]
    fn embed_timeout_is_overridable_for_slow_local_models() {
        let cfg = super::OpenAiCompatConfig::new("http://localhost:11434/v1", None)
            .with_timeout(Duration::from_mins(5));
        assert_eq!(cfg.timeout, Duration::from_mins(5));
    }

    #[test]
    fn config_debug_redacts_bearer_token() {
        let cfg = super::OpenAiCompatConfig::new(
            "https://api.mistral.ai/v1",
            Some("sk-supersecret".into()),
        );
        let debug = format!("{cfg:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("sk-supersecret"));
        // Non-secret fields stay visible for diagnostics.
        assert!(debug.contains("api.mistral.ai"));
    }

    #[test]
    fn config_debug_shows_absent_bearer_as_none() {
        let cfg = super::OpenAiCompatConfig::new("http://localhost:11434/v1", None);
        let debug = format!("{cfg:?}");
        assert!(debug.contains("None"));
        assert!(!debug.contains("<redacted>"));
    }

    #[test]
    fn client_debug_inherits_bearer_redaction() {
        let client = super::OpenAiCompatEmbeddingClient::mistral(
            "sk-topsecret",
            super::MISTRAL_EMBED_MODEL,
            super::MISTRAL_EMBED_BASE_URL,
        )
        .expect("client builds");
        let debug = format!("{client:?}");
        assert!(!debug.contains("sk-topsecret"));
        assert!(debug.contains("<redacted>"));
    }

    #[test]
    fn client_rejects_plaintext_non_loopback_base_url() {
        let cfg = super::OpenAiCompatConfig::new("http://api.mistral.ai/v1", Some("t".into()));
        let err =
            super::OpenAiCompatEmbeddingClient::new(super::MISTRAL_EMBED_MODEL, probe_caps(), cfg)
                .expect_err("plaintext remote base must be rejected");
        assert!(matches!(err, LlmError::Internal(_)));
    }

    #[test]
    fn client_allows_https_remote_base_url() {
        let cfg = super::OpenAiCompatConfig::new(super::MISTRAL_EMBED_BASE_URL, Some("t".into()));
        assert!(
            super::OpenAiCompatEmbeddingClient::new(super::MISTRAL_EMBED_MODEL, probe_caps(), cfg)
                .is_ok()
        );
    }

    #[test]
    fn client_allows_loopback_http_base_url() {
        // Ollama default and IPv4/IPv6 loopback plaintext must keep working.
        for base in [
            "http://localhost:11434/v1",
            "http://127.0.0.1:11434/v1",
            "http://[::1]:11434/v1",
        ] {
            let cfg = super::OpenAiCompatConfig::new(base, None);
            assert!(
                super::OpenAiCompatEmbeddingClient::new(
                    super::MISTRAL_EMBED_MODEL,
                    probe_caps(),
                    cfg
                )
                .is_ok(),
                "loopback base {base} must be allowed"
            );
        }
    }

    #[test]
    fn mistral_preset_uses_core_embedding_space_without_matryoshka() {
        let client = super::OpenAiCompatEmbeddingClient::mistral(
            "secret",
            super::MISTRAL_EMBED_MODEL,
            super::MISTRAL_EMBED_BASE_URL,
        )
        .expect("client builds");

        assert_eq!(client.model_id, super::MISTRAL_EMBED_MODEL);
        assert_eq!(client.config.base_url, super::MISTRAL_EMBED_BASE_URL);
        assert_eq!(client.config.bearer_token.as_deref(), Some("secret"));
        assert_eq!(
            client.caps.dim,
            u32::try_from(proxima_core::llm::EMBEDDING_DIM).expect("EMBEDDING_DIM fits u32")
        );
        assert!(!client.caps.matryoshka);
    }
}
