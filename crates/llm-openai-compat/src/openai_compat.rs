use std::time::Duration;

use async_trait::async_trait;
use proxima_core::llm::{EmbeddingClient, LlmError, MIN_EMBED_INPUT_CAP_CHARS};
use proxima_core::models::EmbedCaps;
use serde::{Deserialize, Serialize};

use crate::{build_client, ensure_secure_base_url, join_endpoint};

// =====================================================================
// OpenAI-compatible embedding client — /embeddings
// =====================================================================

/// Default per-request timeout for `/embeddings` calls. Deliberately far
/// shorter than a text-generation timeout: a single embedding is a small,
/// fast request, and the in-process serial drainer must not stall for
/// minutes on one wedged call. Override with
/// [`OpenAiCompatConfig::with_timeout`] for unusually slow local models.
pub const DEFAULT_EMBED_TIMEOUT: Duration = proxima_core::llm::DEFAULT_EMBED_REQUEST_TIMEOUT;

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

impl OpenAiEmbeddingResponse {
    fn into_embeddings(
        self,
        expected_count: usize,
        caps: EmbedCaps,
    ) -> Result<Vec<Vec<f32>>, LlmError> {
        if self.data.len() != expected_count {
            return Err(LlmError::Embed(format!(
                "requested {} embeddings, response carried {}",
                expected_count,
                self.data.len()
            )));
        }
        let mut data = self.data;
        // Index-free compatible providers retain array order. Once any
        // index is supplied, require a complete permutation of the inputs:
        // sorting alone would silently accept duplicates or missing indices.
        if data.iter().any(|d| d.index.is_some()) {
            data.sort_by_key(|d| d.index);
            for (expected_index, datum) in data.iter().enumerate() {
                if datum.index != Some(expected_index) {
                    return Err(LlmError::Embed(format!(
                        "invalid embedding response index {:?}; expected {expected_index} after \
                         sorting a complete set of {expected_count} input indices",
                        datum.index,
                    )));
                }
            }
        }

        let expected = caps.dim as usize;
        data.into_iter()
            .map(|datum| {
                if datum.embedding.len() == expected {
                    Ok(datum.embedding)
                } else {
                    Err(LlmError::Embed(format!(
                        "expected dim {} (matryoshka={}), got {}",
                        expected,
                        caps.matryoshka,
                        datum.embedding.len()
                    )))
                }
            })
            .collect()
    }
}

/// Whether a non-success `/embeddings` status is evidence of a request that
/// retries cannot fix.
///
/// Only statuses that unambiguously identify the submitted entity as the
/// rejected cause are permanent. 400 is deliberately ambiguous: compatible
/// endpoints use it for input limits, malformed requests, authentication,
/// routing, and provider failures alike. Liveness probes at the drain and
/// per-item rescue boundaries handle that ambiguity without fencing a
/// repairable job forever.
/// Every other non-success status remains retryable, including auth, routing,
/// policy, and server responses.
///
/// Classification deliberately depends only on the HTTP status. Compatible
/// endpoints may format error bodies differently, and free-text bodies cannot
/// establish whether the input or the service caused a failure.
fn permanent_embed_status(status: reqwest::StatusCode) -> bool {
    matches!(status.as_u16(), 413 | 422)
}

fn embed_http_error(status: reqwest::StatusCode, body: &str) -> LlmError {
    let message = format!("openai-compatible /embeddings returned {status}: {body}");
    if permanent_embed_status(status) {
        LlmError::EmbedPermanent(message)
    } else {
        LlmError::Embed(message)
    }
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

    /// Complete-request timeout applied to the underlying HTTP client.
    #[must_use]
    pub const fn request_timeout(&self) -> Duration {
        self.config.timeout
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
            return Err(embed_http_error(status, &text_body));
        }

        let parsed: OpenAiEmbeddingResponse = serde_json::from_str(&text_body).map_err(|e| {
            LlmError::Embed(format!(
                "decode OpenAI-compatible envelope: {e}; body: {text_body}"
            ))
        })?;
        parsed.into_embeddings(inputs.len(), self.caps)
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

    fn decode_vectors(
        data: serde_json::Value,
        expected_count: usize,
    ) -> Result<Vec<Vec<f32>>, LlmError> {
        let response = super::OpenAiEmbeddingResponse {
            data: serde_json::from_value(data).expect("valid response data"),
        };
        response.into_embeddings(expected_count, EmbedCaps::new(2, false))
    }

    #[test]
    fn response_indices_restore_input_order() {
        let vectors = decode_vectors(
            serde_json::json!([
                { "index": 1, "embedding": [0.0, 1.0] },
                { "index": 0, "embedding": [1.0, 0.0] }
            ]),
            2,
        )
        .expect("a complete permutation is valid");
        assert_eq!(vectors, vec![vec![1.0, 0.0], vec![0.0, 1.0]]);
    }

    #[test]
    fn response_without_indices_preserves_provider_order() {
        let vectors = decode_vectors(
            serde_json::json!([
                { "embedding": [0.0, 1.0] },
                { "embedding": [1.0, 0.0] }
            ]),
            2,
        )
        .expect("compatible providers may omit all indices");
        assert_eq!(vectors, vec![vec![0.0, 1.0], vec![1.0, 0.0]]);
    }

    #[test]
    fn response_rejects_duplicate_or_out_of_range_indices() {
        for indices in [[0, 0], [1, 1], [0, 2], [1, 2]] {
            let result = decode_vectors(
                serde_json::json!([
                    { "index": indices[0], "embedding": [1.0, 0.0] },
                    { "index": indices[1], "embedding": [0.0, 1.0] }
                ]),
                2,
            );
            assert!(
                matches!(result, Err(LlmError::Embed(_))),
                "invalid indices {indices:?} must not assign vectors to inputs: {result:?}",
            );
        }
    }

    #[test]
    fn response_rejects_partially_indexed_batches() {
        for indices in [[Some(0), None], [None, Some(1)]] {
            let result = decode_vectors(
                serde_json::json!([
                    { "index": indices[0], "embedding": [1.0, 0.0] },
                    { "index": indices[1], "embedding": [0.0, 1.0] }
                ]),
                2,
            );
            assert!(
                matches!(result, Err(LlmError::Embed(_))),
                "partial indices must not silently fall back to array order: {result:?}",
            );
        }
    }

    #[test]
    fn response_rejects_nonzero_index_for_single_input() {
        assert!(matches!(
            decode_vectors(
                serde_json::json!([{ "index": 1, "embedding": [1.0, 0.0] }]),
                1,
            ),
            Err(LlmError::Embed(_))
        ));
    }

    #[test]
    fn response_rejects_wrong_count_or_dimensions() {
        for data in [
            serde_json::json!([]),
            serde_json::json!([{ "index": 0, "embedding": [1.0, 0.0] }]),
            serde_json::json!([
                { "index": 0, "embedding": [1.0, 0.0] },
                { "index": 1, "embedding": [1.0] }
            ]),
        ] {
            assert!(matches!(decode_vectors(data, 2), Err(LlmError::Embed(_))));
        }
    }

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
            "https://embeddings.example/v1",
            Some("sk-supersecret".into()),
        );
        let debug = format!("{cfg:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("sk-supersecret"));
        // Non-secret fields stay visible for diagnostics.
        assert!(debug.contains("embeddings.example"));
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
        let client = super::OpenAiCompatEmbeddingClient::new(
            "test-embed",
            probe_caps(),
            super::OpenAiCompatConfig::new(
                "https://embeddings.example/v1",
                Some("sk-topsecret".into()),
            ),
        )
        .expect("client builds");
        let debug = format!("{client:?}");
        assert!(!debug.contains("sk-topsecret"));
        assert!(debug.contains("<redacted>"));
    }

    #[test]
    fn client_rejects_plaintext_non_loopback_base_url() {
        let cfg = super::OpenAiCompatConfig::new("HTTP://api.example.com/v1", Some("t".into()));
        let err = super::OpenAiCompatEmbeddingClient::new("test-embed", probe_caps(), cfg)
            .expect_err("plaintext remote base must be rejected");
        assert!(matches!(err, LlmError::Internal(_)));
    }

    #[test]
    fn client_allows_https_remote_base_url() {
        let cfg = super::OpenAiCompatConfig::new("https://embeddings.example/v1", Some("t".into()));
        assert!(super::OpenAiCompatEmbeddingClient::new("test-embed", probe_caps(), cfg).is_ok());
    }

    #[test]
    fn client_allows_loopback_http_base_url() {
        // IPv4/IPv6 loopback plaintext must keep working.
        for base in [
            "http://LOCALHOST:11434/v1",
            "http://127.0.0.1:11434/v1",
            "http://[::1]:11434/v1",
        ] {
            let cfg = super::OpenAiCompatConfig::new(base, None);
            assert!(
                super::OpenAiCompatEmbeddingClient::new("test-embed", probe_caps(), cfg).is_ok(),
                "loopback base {base} must be allowed"
            );
        }
    }

    #[test]
    fn status_policy_is_independent_of_error_body() {
        use reqwest::StatusCode;
        let bodies = [
            "",
            "EOF",
            r#"{"error":{"message":"input exceeds the configured limit"}}"#,
            r#"{"message":"temporary service condition"}"#,
            "arbitrary text with no protocol meaning",
        ];
        let policy = [
            (400, false),
            (401, false),
            (402, false),
            (403, false),
            (404, false),
            (405, false),
            (406, false),
            (407, false),
            (408, false),
            (409, false),
            (410, false),
            (411, false),
            (412, false),
            (413, true),
            (414, false),
            (415, false),
            (416, false),
            (417, false),
            (418, false),
            (421, false),
            (422, true),
            (423, false),
            (424, false),
            (425, false),
            (426, false),
            (428, false),
            (429, false),
            (431, false),
            (451, false),
            (500, false),
            (501, false),
            (502, false),
            (503, false),
            (504, false),
            (505, false),
            (506, false),
            (507, false),
            (508, false),
            (510, false),
            (511, false),
        ];
        for (code, expected_permanent) in policy {
            let status = StatusCode::from_u16(code).expect("valid HTTP status");
            for body in bodies {
                let error = super::embed_http_error(status, body);
                assert_eq!(
                    matches!(error, LlmError::EmbedPermanent(_)),
                    expected_permanent,
                    "status {status} was classified from its body: {body:?}",
                );
            }
        }
    }

    #[test]
    fn embed_request_serializes_inputs_as_array() {
        // Providers' /embeddings endpoints take `input` as an array; the
        // batch width of one request is what divides request-rate-limit
        // pressure, so the wire shape is load-bearing.
        let body = super::EmbedRequest {
            model: "test-embed",
            input: &["first text", "second text"],
            dimensions: None,
        };
        let json = serde_json::to_value(&body).expect("serialize");
        assert_eq!(
            json,
            serde_json::json!({
                "model": "test-embed",
                "input": ["first text", "second text"],
            })
        );
    }
}

#[cfg(test)]
mod redirect_tests {
    use std::net::SocketAddr;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use proxima_core::llm::{EmbeddingClient, LlmError};
    use proxima_core::models::EmbedCaps;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    use super::{OpenAiCompatConfig, OpenAiCompatEmbeddingClient};

    enum Target {
        Loopback,
        RemoteHttp,
        Loop,
    }

    struct RedirectServer {
        addr: SocketAddr,
        requests: Arc<AtomicUsize>,
        target_requests: Arc<AtomicUsize>,
        task: tokio::task::JoinHandle<()>,
    }

    impl Drop for RedirectServer {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    async fn read_request(stream: &mut TcpStream) -> String {
        let mut request = Vec::new();
        loop {
            let mut buffer = [0; 4096];
            let count = stream.read(&mut buffer).await.expect("read request");
            assert!(count > 0, "connection ended before a complete request");
            request.extend_from_slice(&buffer[..count]);
            let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") else {
                continue;
            };
            let headers = std::str::from_utf8(&request[..end]).expect("UTF-8 headers");
            let content_length: usize = headers
                .lines()
                .filter_map(|line| line.split_once(':'))
                .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                .map_or(0, |(_, value)| {
                    value.trim().parse().expect("content length")
                });
            if request.len() >= end + 4 + content_length {
                return String::from_utf8(request).expect("UTF-8 test request");
            }
        }
    }

    async fn spawn_redirect_server(status: u16, target: Target) -> RedirectServer {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind server");
        let addr = listener.local_addr().expect("server address");
        let requests = Arc::new(AtomicUsize::new(0));
        let target_requests = Arc::new(AtomicUsize::new(0));
        let all_count = requests.clone();
        let target_count = target_requests.clone();
        let task = tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.expect("accept request");
                let request = read_request(&mut stream).await;
                all_count.fetch_add(1, Ordering::SeqCst);
                let response = if request.starts_with("POST /target ") {
                    target_count.fetch_add(1, Ordering::SeqCst);
                    let (_, body) = request.split_once("\r\n\r\n").expect("request body");
                    let body: serde_json::Value = serde_json::from_str(body).expect("JSON input");
                    assert_eq!(body["input"], serde_json::json!(["private memory text"]));
                    let body = r#"{"data":[{"index":0,"embedding":[1.0,0.0]}]}"#;
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                } else {
                    let destination = if request.starts_with("POST /v1/embeddings ") {
                        "/intermediate".to_string()
                    } else {
                        match target {
                            Target::Loopback => format!("http://{addr}/target"),
                            Target::RemoteHttp => {
                                format!("http://redirect.example:{}/target", addr.port())
                            }
                            Target::Loop => "/intermediate".to_string(),
                        }
                    };
                    format!(
                        "HTTP/1.1 {status} Redirect\r\nLocation: {destination}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    )
                };
                stream
                    .write_all(response.as_bytes())
                    .await
                    .expect("respond");
            }
        });
        RedirectServer {
            addr,
            requests,
            target_requests,
            task,
        }
    }

    fn client_for(server: &RedirectServer) -> OpenAiCompatEmbeddingClient {
        let mut client = OpenAiCompatEmbeddingClient::new(
            "test-embed",
            EmbedCaps::new(2, false),
            OpenAiCompatConfig::new(format!("http://{}/v1", server.addr), None),
        )
        .expect("loopback embedding endpoint");
        client.client = crate::build_http_client(
            reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .no_proxy()
                .resolve("redirect.example", server.addr),
        )
        .expect("build test client with production redirect policy");
        client
    }

    #[tokio::test]
    async fn nonloopback_plaintext_redirect_cannot_receive_embedding_input() {
        for status in [307, 308] {
            let server = spawn_redirect_server(status, Target::RemoteHttp).await;
            let result = client_for(&server).embed("private memory text").await;
            assert_eq!(server.target_requests.load(Ordering::SeqCst), 0);
            assert_eq!(server.requests.load(Ordering::SeqCst), 2);
            assert!(matches!(result, Err(LlmError::Embed(_))), "{result:?}");
        }
    }

    #[tokio::test]
    async fn valid_loopback_redirects_preserve_embedding_input() {
        for status in [307, 308] {
            let server = spawn_redirect_server(status, Target::Loopback).await;
            let vector = client_for(&server)
                .embed("private memory text")
                .await
                .expect("valid relative and absolute loopback redirects");
            assert_eq!(vector, vec![1.0, 0.0]);
            assert_eq!(server.target_requests.load(Ordering::SeqCst), 1);
        }
    }

    #[tokio::test]
    async fn redirect_loops_keep_the_default_ten_hop_limit() {
        let server = spawn_redirect_server(307, Target::Loop).await;
        let result = client_for(&server).embed("private memory text").await;
        assert!(matches!(result, Err(LlmError::Embed(_))), "{result:?}");
        assert_eq!(server.requests.load(Ordering::SeqCst), 11);
        assert_eq!(server.target_requests.load(Ordering::SeqCst), 0);
    }
}
