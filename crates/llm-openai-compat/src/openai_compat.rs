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

/// Whether a non-success `/embeddings` status is evidence of a request that
/// retries cannot fix.
///
/// Every 4xx status is a permanent client/request failure except statuses
/// whose HTTP semantics explicitly permit retrying: 408 (timeout), 409
/// (conflict), 421 (misdirected request), 423 (locked), 424 (failed
/// dependency), 425 (too early), and 429 (rate limit). Every 5xx status is a
/// retryable server failure. Other non-success classes remain retryable so an
/// unexpected protocol response does not terminally reject the input.
///
/// Classification deliberately depends only on the HTTP status. Compatible
/// endpoints may format error bodies differently, and free-text bodies cannot
/// establish whether the input or the service caused a failure.
fn permanent_embed_status(status: reqwest::StatusCode) -> bool {
    if status.is_client_error() {
        return !matches!(status.as_u16(), 408 | 409 | 421 | 423 | 424 | 425 | 429);
    }
    false
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
            r#"{"error":{"message":"input exceeds the configured limit"}}"#,
            r#"{"message":"temporary service condition"}"#,
            "arbitrary text with no protocol meaning",
        ];
        let policy = [
            (400, true),
            (401, true),
            (402, true),
            (403, true),
            (404, true),
            (405, true),
            (406, true),
            (407, true),
            (408, false),
            (409, false),
            (410, true),
            (411, true),
            (412, true),
            (413, true),
            (414, true),
            (415, true),
            (416, true),
            (417, true),
            (418, true),
            (421, false),
            (422, true),
            (423, false),
            (424, false),
            (425, false),
            (426, true),
            (428, true),
            (429, false),
            (431, true),
            (451, true),
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
