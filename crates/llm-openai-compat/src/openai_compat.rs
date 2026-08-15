use std::time::Duration;

use async_trait::async_trait;
use proxima_core::llm::{EMBEDDING_DIM, EmbeddingClient, LlmError, MIN_EMBED_INPUT_CAP_CHARS};
use proxima_core::models::EmbedCaps;
use serde::{Deserialize, Serialize};

use crate::{build_client, ensure_secure_base_url, join_endpoint};

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

#[derive(Serialize)]
struct EmbedRequest<'a> {
    model: &'a str,
    input: &'a [&'a str],
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
    /// Returns `LlmError::Internal` if the HTTP client cannot be built, if
    /// `config.base_url` is a non-loopback plaintext `http://` endpoint (which
    /// would leak the bearer token in transit), or if
    /// [`EmbedCaps::max_input_chars`] is set below
    /// [`proxima_core::llm::MIN_EMBED_INPUT_CAP_CHARS`].
    pub fn new(
        model_id: impl Into<String>,
        caps: EmbedCaps,
        config: OpenAiCompatConfig,
    ) -> Result<Self, LlmError> {
        ensure_secure_base_url(&config.base_url)?;
        // Rejected at construction rather than tolerated at call time: a cap
        // under the floor makes over-long input terminal instead of chunked,
        // and it does so invisibly — every component behaves as documented
        // and the memory is simply never embedded. Refusing to boot names the
        // misconfiguration while someone is still looking at it.
        if let Some(max) = caps.max_input_chars {
            let floor = MIN_EMBED_INPUT_CAP_CHARS;
            if (max.get() as usize) < floor {
                return Err(LlmError::Internal(format!(
                    "max_input_chars is {max}, below the {floor}-char floor the chunked-embedding \
                     rescue can satisfy; a longer input would go terminal instead of being split",
                )));
            }
        }
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
            EmbedCaps::new(dim, false),
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
    #[serde(default)]
    index: Option<usize>,
    embedding: Vec<f32>,
}

/// Signatures of a provider that never reached its model, as they appear
/// inside an otherwise input-shaped error body. All Go transport errors —
/// Ollama proxies `/v1/embeddings` to a per-model runner and reports the
/// runner's connection failures this way.
const TRANSPORT_FAILURE_MARKERS: [&str; 7] = [
    ": eof",
    "connection refused",
    "connection reset",
    "broken pipe",
    "context deadline exceeded",
    "i/o timeout",
    "no such host",
];

/// Whether a non-success `/embeddings` status is one retries cannot fix.
/// 400/413/422 reject the request itself (over-limit input, malformed
/// payload); 408/429/5xx are transient.
///
/// A 400 only means "your input is bad" when the provider looked at the
/// input. Ollama returns 400 on its own runner-connection EOF — a transport
/// failure wearing an input-rejection status. Treating that as permanent
/// sends the job to `fail_embedding_job_permanently`, which
/// `reconcile_embeddings` will not requeue.
fn permanent_embed_status(status: reqwest::StatusCode, body: &str) -> bool {
    if !matches!(status.as_u16(), 400 | 413 | 422) {
        return false;
    }
    let lowered = body.to_ascii_lowercase();
    !TRANSPORT_FAILURE_MARKERS
        .iter()
        .any(|marker| lowered.contains(marker))
}

impl OpenAiCompatEmbeddingClient {
    /// The capabilities this client was built with.
    ///
    /// Public so a host can assert what it configured — an input cap that
    /// silently failed to be read looks identical, at runtime, to a provider
    /// that never sees a long input.
    #[must_use]
    pub fn caps(&self) -> EmbedCaps {
        self.caps
    }

    /// Refuse over-cap input *before* it is sent, naming the bound it broke.
    ///
    /// Returned as [`LlmError::EmbedPermanent`] on two counts. It is true —
    /// no retry of this text at this length can succeed — and it is what
    /// routes a batch into per-input isolation and a single input into
    /// [`proxima_core::llm::embed_in_chunks`], so an over-long memory is
    /// still embedded, in pieces, without a request ever leaving the
    /// process.
    ///
    /// The message names the length that was sent and the cap, and never
    /// quotes a bound the input satisfies: a caller shortening a body needs
    /// to know by how much, and one told a limit they already meet reads it
    /// as a server fault and retries unchanged.
    fn refuse_over_cap(&self, inputs: &[&str]) -> Result<(), LlmError> {
        let Some(max) = self.caps.max_input_chars else {
            return Ok(());
        };
        let max = max.get() as usize;
        for input in inputs {
            let chars = input.chars().count();
            if chars > max {
                return Err(LlmError::EmbedPermanent(format!(
                    "input of {chars} chars exceeds the {max}-char limit configured for \
                     model {}; not sent",
                    self.model_id,
                )));
            }
        }
        Ok(())
    }

    async fn embed_call(&self, inputs: &[&str]) -> Result<Vec<Vec<f32>>, LlmError> {
        self.refuse_over_cap(inputs)?;
        let url = join_endpoint(&self.config.base_url, "embeddings");
        let body = EmbedRequest {
            model: &self.model_id,
            input: inputs,
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
            let message = format!("openai-compatible /embeddings returned {status}: {text_body}");
            return Err(if permanent_embed_status(status, &text_body) {
                LlmError::EmbedPermanent(message)
            } else {
                LlmError::Embed(message)
            });
        }

        let parsed: OpenAiEmbeddingResponse = serde_json::from_str(&text_body).map_err(|e| {
            LlmError::Embed(format!(
                "decode OpenAI-compatible envelope: {e}; body: {text_body}"
            ))
        })?;
        if parsed.data.len() != inputs.len() {
            return Err(LlmError::Embed(format!(
                "requested {} embeddings, response carried {}",
                inputs.len(),
                parsed.data.len()
            )));
        }
        // The OpenAI shape orders `data` by `index`; sort defensively when
        // the field is present so batch outputs align with batch inputs.
        let mut data = parsed.data;
        if data.iter().all(|d| d.index.is_some()) {
            data.sort_by_key(|d| d.index.unwrap_or(usize::MAX));
        }

        let expected = self.dim();
        data.into_iter()
            .map(|datum| {
                if datum.embedding.len() == expected {
                    Ok(datum.embedding)
                } else {
                    Err(LlmError::Embed(format!(
                        "expected dim {} (matryoshka={}), got {}",
                        expected,
                        self.caps.matryoshka,
                        datum.embedding.len()
                    )))
                }
            })
            .collect()
    }
}

#[async_trait]
impl EmbeddingClient for OpenAiCompatEmbeddingClient {
    async fn embed(&self, text: &str) -> Result<Vec<f32>, LlmError> {
        let mut vecs = self.embed_call(&[text]).await?;
        vecs.pop()
            .ok_or_else(|| LlmError::Embed("OpenAI-compatible response had no embeddings".into()))
    }

    async fn embed_many(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, LlmError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let inputs: Vec<&str> = texts.iter().map(String::as_str).collect();
        self.embed_call(&inputs).await
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
    use std::num::NonZeroU32;
    use std::time::Duration;

    use proxima_core::llm::{EmbeddingClient, LlmError, MIN_EMBED_INPUT_CAP_CHARS};
    use proxima_core::models::EmbedCaps;

    fn probe_caps() -> EmbedCaps {
        EmbedCaps::new(8, false)
    }

    /// An endpoint that cannot answer. Any request that actually leaves the
    /// process fails here, which is what makes "the input was refused before
    /// it was sent" observable: the guard returns `EmbedPermanent`, a request
    /// returns a transport `Embed`.
    const UNREACHABLE: &str = "http://127.0.0.1:9/v1";

    fn floor_u32() -> u32 {
        u32::try_from(MIN_EMBED_INPUT_CAP_CHARS).expect("the floor fits u32")
    }

    fn capped_client(max_chars: u32) -> super::OpenAiCompatEmbeddingClient {
        super::OpenAiCompatEmbeddingClient::new(
            "test-embed",
            probe_caps().with_max_input_chars(NonZeroU32::new(max_chars).expect("positive")),
            super::OpenAiCompatConfig::new(UNREACHABLE, None),
        )
        .expect("a cap at or above the floor is accepted")
    }

    #[tokio::test]
    async fn an_over_cap_input_is_refused_without_being_sent() {
        let client = capped_client(floor_u32());
        let too_long = "a".repeat(MIN_EMBED_INPUT_CAP_CHARS + 1);

        match client.embed(&too_long).await {
            Err(LlmError::EmbedPermanent(message)) => {
                // The rejection must name what was sent and the limit, and
                // must not quote a bound the input already satisfies: a
                // caller told a limit they meet reads it as a server fault
                // and retries the same input unchanged, which is the loop
                // this whole guard exists to break.
                assert!(
                    message.contains(&(MIN_EMBED_INPUT_CAP_CHARS + 1).to_string()),
                    "the length that was refused is missing: {message}",
                );
                assert!(
                    message.contains(&MIN_EMBED_INPUT_CAP_CHARS.to_string()),
                    "the limit is missing: {message}",
                );
            }
            Err(LlmError::Embed(message)) => panic!(
                "the input reached the network before being judged: {message}. \
                 A provider that dies on over-long input is exactly the one \
                 that must never receive it."
            ),
            other => panic!("expected a permanent refusal, got {other:?}"),
        }
    }

    /// The other half: the guard must not stand in for the provider's own
    /// judgement. An input inside the cap is sent, and against an endpoint
    /// that cannot answer that surfaces as an ordinary retryable error.
    #[tokio::test]
    async fn an_input_within_the_cap_is_still_sent() {
        let client = capped_client(floor_u32());
        let err = client
            .embed(&"a".repeat(MIN_EMBED_INPUT_CAP_CHARS))
            .await
            .expect_err("the endpoint is unreachable");
        assert!(
            matches!(err, LlmError::Embed(_)),
            "an in-bounds input must reach the transport, got {err:?}",
        );
    }

    /// A batch is judged per input. One over-cap text refuses the call, which
    /// is what routes the drain into per-input isolation so the other 31
    /// still embed.
    #[tokio::test]
    async fn one_over_cap_text_refuses_the_batch_it_travels_in() {
        let client = capped_client(floor_u32());
        let batch = vec![
            "short".to_string(),
            "a".repeat(MIN_EMBED_INPUT_CAP_CHARS + 1),
            "also short".to_string(),
        ];
        assert!(
            matches!(
                client.embed_many(&batch).await,
                Err(LlmError::EmbedPermanent(_))
            ),
            "an over-cap member must refuse the batch before it is sent",
        );
    }

    /// No cap is the default and preserves the prior behaviour exactly: the
    /// provider judges its own input. A deployment whose provider rejects
    /// cleanly does not need this guard.
    #[tokio::test]
    async fn without_a_cap_every_input_is_offered_to_the_provider() {
        let client = super::OpenAiCompatEmbeddingClient::new(
            "test-embed",
            probe_caps(),
            super::OpenAiCompatConfig::new(UNREACHABLE, None),
        )
        .expect("no cap is a valid configuration");
        assert!(client.caps.max_input_chars.is_none());

        let err = client
            .embed(&"a".repeat(MIN_EMBED_INPUT_CAP_CHARS * 10))
            .await
            .expect_err("the endpoint is unreachable");
        assert!(
            matches!(err, LlmError::Embed(_)),
            "with no cap the input must still be offered, got {err:?}",
        );
    }

    /// A cap below the floor is refused while someone is looking at it. The
    /// alternative is a configuration that boots, behaves as documented at
    /// every layer, and silently never embeds a long memory.
    #[test]
    fn a_cap_the_chunked_rescue_cannot_satisfy_is_refused_at_construction() {
        let err = super::OpenAiCompatEmbeddingClient::new(
            "test-embed",
            probe_caps().with_max_input_chars(
                NonZeroU32::new(floor_u32() - 1).expect("one under the floor is positive"),
            ),
            super::OpenAiCompatConfig::new(UNREACHABLE, None),
        )
        .expect_err("a cap under the floor must not build a client");
        assert!(
            matches!(err, LlmError::Internal(ref m) if m.contains("terminal")),
            "the refusal must say what goes wrong, not just that it did: {err:?}",
        );
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
        let cfg = super::OpenAiCompatConfig::new("HTTP://api.example.com/v1", Some("t".into()));
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
            "http://LOCALHOST:11434/v1",
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
    fn permanent_statuses_are_request_rejections_only() {
        use reqwest::StatusCode;
        // Rejections of the request itself — retrying the same input can
        // never succeed, so jobs must go terminal.
        let input_rejection =
            r#"{"error":{"message":"input exceeds maximum context length of 512 tokens"}}"#;
        for permanent in [
            StatusCode::BAD_REQUEST,
            StatusCode::PAYLOAD_TOO_LARGE,
            StatusCode::UNPROCESSABLE_ENTITY,
        ] {
            assert!(
                super::permanent_embed_status(permanent, input_rejection),
                "{permanent}"
            );
        }
        // Auth, rate-limit, timeout, and server-side failures are all
        // retryable: the same input may embed fine later.
        for transient in [
            StatusCode::UNAUTHORIZED,
            StatusCode::FORBIDDEN,
            StatusCode::NOT_FOUND,
            StatusCode::REQUEST_TIMEOUT,
            StatusCode::TOO_MANY_REQUESTS,
            StatusCode::INTERNAL_SERVER_ERROR,
            StatusCode::BAD_GATEWAY,
            StatusCode::SERVICE_UNAVAILABLE,
        ] {
            assert!(
                !super::permanent_embed_status(transient, input_rejection),
                "{transient}"
            );
        }
    }

    /// Verbatim bodies a local Ollama returns when its connection to the
    /// model runner drops. The status is 400, but nothing looked at the
    /// input, so retrying is exactly what should happen.
    #[test]
    fn a_transport_failure_dressed_as_400_is_not_permanent() {
        use reqwest::StatusCode;
        for body in [
            r#"{"error":{"message":"do embedding request: Post \"http://127.0.0.1:50103/v1/embeddings\": EOF","type":"invalid_request_error"}}"#,
            r#"{"error":{"message":"Post \"http://127.0.0.1:9001/v1/embeddings\": dial tcp: connection refused"}}"#,
            r#"{"error":{"message":"Post \"http://127.0.0.1:9001/v1/embeddings\": context deadline exceeded"}}"#,
            r#"{"error":{"message":"read tcp 127.0.0.1:1: connection reset by peer"}}"#,
            r#"{"error":{"message":"write tcp: broken pipe"}}"#,
            r#"{"error":{"message":"dial tcp: lookup embed.internal: no such host"}}"#,
            r#"{"error":{"message":"Post \"http://x/v1/embeddings\": i/o timeout"}}"#,
        ] {
            assert!(
                !super::permanent_embed_status(StatusCode::BAD_REQUEST, body),
                "must stay retryable: {body}"
            );
        }
    }

    /// The marker match must not swallow a real input rejection that merely
    /// mentions the end of the input.
    #[test]
    fn an_input_rejection_naming_eof_is_still_permanent() {
        use reqwest::StatusCode;
        assert!(super::permanent_embed_status(
            StatusCode::BAD_REQUEST,
            r#"{"error":{"message":"unexpected EOF while parsing input array"}}"#
        ));
    }

    #[test]
    fn embed_request_serializes_inputs_as_array() {
        // Providers' /embeddings endpoints take `input` as an array; the
        // batch width of one request is what divides request-rate-limit
        // pressure, so the wire shape is load-bearing.
        let body = super::EmbedRequest {
            model: "mistral-embed",
            input: &["first text", "second text"],
            dimensions: None,
        };
        let json = serde_json::to_value(&body).expect("serialize");
        assert_eq!(
            json,
            serde_json::json!({
                "model": "mistral-embed",
                "input": ["first text", "second text"],
            })
        );
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
