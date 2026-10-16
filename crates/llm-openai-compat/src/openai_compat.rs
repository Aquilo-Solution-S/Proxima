use std::fmt::Write as _;
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use futures::FutureExt;
use proxima_core::PlaintextHosts;
use proxima_core::llm::{
    EmbeddingClient, EmbeddingConfigurationFault, EmbeddingInputRefusal, LlmError,
    MIN_EMBED_INPUT_CAP_CHARS,
};
use proxima_core::models::EmbedCaps;
use serde::{Deserialize, Serialize};

use crate::{build_client, embedding_endpoint};

// =====================================================================
// OpenAI-compatible embedding client — /embeddings
// =====================================================================

/// Default per-request timeout for `/embeddings` calls. Deliberately far
/// shorter than a text-generation timeout: a single embedding is a small,
/// fast request, and the in-process serial drainer must not stall for
/// minutes on one wedged call. Override with
/// [`OpenAiCompatConfig::with_timeout`] for unusually slow local models.
pub const DEFAULT_EMBED_TIMEOUT: Duration = proxima_core::llm::DEFAULT_EMBED_REQUEST_TIMEOUT;

const RESPONSE_ENVELOPE_BYTES: usize = 64 * 1024;
const BYTES_PER_FLOAT_HEADROOM: usize = 32;
const ERROR_BODY_PREFIX_BYTES: usize = 512;

#[derive(Clone)]
pub struct OpenAiCompatConfig {
    /// Provider base URL. `/embeddings` is appended to its path; query
    /// parameters are preserved. Fragments are rejected at construction.
    pub base_url: String,
    pub timeout: Duration,
    pub bearer_token: Option<String>,
    /// Hosts, besides loopback, that `base_url` may name over plaintext
    /// `http://`. Redirects are refused. Empty by default; a host reads it from
    /// `PROXIMA_PLAINTEXT_HOSTS` with [`PlaintextHosts::from_lookup`].
    pub plaintext_hosts: PlaintextHosts,
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
            .field("plaintext_hosts", &self.plaintext_hosts)
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
            plaintext_hosts: PlaintextHosts::none(),
        }
    }

    /// Name the hosts, besides loopback, that may use plaintext `http://`.
    /// Defaults to none.
    #[must_use]
    pub fn with_plaintext_hosts(mut self, plaintext_hosts: PlaintextHosts) -> Self {
        self.plaintext_hosts = plaintext_hosts;
        self
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
    endpoint: reqwest::Url,
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
    /// `config.base_url` is invalid, contains a fragment, or is a plaintext
    /// `http://` endpoint on a host that is neither loopback nor named in
    /// `config.plaintext_hosts` (which would leak the bearer token in
    /// transit), or if
    /// [`EmbedCaps::max_input_chars`] is set below
    /// [`proxima_core::llm::MIN_EMBED_INPUT_CAP_CHARS`].
    pub fn new(
        model_id: impl Into<String>,
        caps: EmbedCaps,
        config: OpenAiCompatConfig,
    ) -> Result<Self, LlmError> {
        let endpoint = embedding_endpoint(&config.base_url, &config.plaintext_hosts)?;
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
            endpoint,
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
            return Err(LlmError::retryable(format!(
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
                    return Err(LlmError::retryable(format!(
                        "invalid embedding response index {:?}; expected {expected_index} after \
                         sorting a complete set of {expected_count} input indices",
                        datum.index,
                    )));
                }
            }
        }

        let expected = caps.dim as usize;
        // Preserve shape-error precedence across the entire batch before
        // examining components. Valid JSON numbers can overflow f32 while
        // deserializing, even though NaN/Infinity are not JSON number tokens.
        for datum in &data {
            if datum.embedding.len() != expected {
                return Err(LlmError::retryable(format!(
                    "expected dim {} (matryoshka={}), got {}",
                    expected,
                    caps.matryoshka,
                    datum.embedding.len()
                )));
            }
        }
        data.into_iter()
            .enumerate()
            .map(|(embedding_index, datum)| {
                if let Some(component_index) = datum.embedding.iter().position(|value| !value.is_finite()) {
                    Err(LlmError::retryable(format!(
                        "embedding {embedding_index} has non-finite component at position {component_index}"
                    )))
                } else {
                    Ok(datum.embedding)
                }
            })
            .collect()
    }
}

/// Classify the provider status independently of its diagnostic text.
fn embed_http_error(
    status: reqwest::StatusCode,
    retry_after: Option<Duration>,
    body: &str,
) -> LlmError {
    match status.as_u16() {
        413 => LlmError::InputRefused(EmbeddingInputRefusal::PayloadTooLarge),
        422 => LlmError::InputRefused(EmbeddingInputRefusal::UnprocessableContent),
        400 => LlmError::Configuration(EmbeddingConfigurationFault::BadRequest),
        401 => LlmError::Configuration(EmbeddingConfigurationFault::Unauthorized),
        403 => LlmError::Configuration(EmbeddingConfigurationFault::Forbidden),
        404 => LlmError::Configuration(EmbeddingConfigurationFault::NotFound),
        _ => LlmError::Retryable {
            status: Some(status.as_u16()),
            retry_after,
            message: format!("openai-compatible /embeddings returned {status}: {body}"),
        },
    }
}

fn retry_after_delay(headers: &reqwest::header::HeaderMap, now: SystemTime) -> Option<Duration> {
    let value = headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim();
    if value.is_empty() {
        return None;
    }
    if value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Some(
            value
                .parse::<u64>()
                .map_or(Duration::MAX, Duration::from_secs),
        );
    }
    let deadline = httpdate::parse_http_date(value).ok()?;
    Some(deadline.duration_since(now).unwrap_or(Duration::ZERO))
}

fn response_body_limit(batch_size: usize, dimensions: u32) -> usize {
    RESPONSE_ENVELOPE_BYTES.saturating_add(
        batch_size
            .saturating_mul(dimensions as usize)
            .saturating_mul(BYTES_PER_FLOAT_HEADROOM),
    )
}

async fn read_body(response: &mut reqwest::Response, limit: usize) -> Result<Vec<u8>, LlmError> {
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| LlmError::retryable(format!("HTTP body read: {}", error.without_url())))?
    {
        if chunk.len() > limit.saturating_sub(body.len()) {
            return Err(LlmError::retryable(format!(
                "HTTP response body exceeded the {limit}-byte limit"
            )));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

async fn read_error_prefix(response: &mut reqwest::Response) -> Result<Vec<u8>, reqwest::Error> {
    let mut prefix = Vec::new();
    while prefix.len() < ERROR_BODY_PREFIX_BYTES {
        let Some(chunk) = response.chunk().await? else {
            break;
        };
        let remaining = ERROR_BODY_PREFIX_BYTES - prefix.len();
        prefix.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
    }
    Ok(prefix)
}

fn utf8_prefix(bytes: &[u8]) -> &str {
    let bytes = &bytes[..bytes.len().min(ERROR_BODY_PREFIX_BYTES)];
    match std::str::from_utf8(bytes) {
        Ok(prefix) => prefix,
        Err(error) => std::str::from_utf8(&bytes[..error.valid_up_to()])
            .expect("valid_up_to identifies a valid UTF-8 prefix"),
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

    /// Refuse over-cap input before sending it. The typed local refusal
    /// carries the actual length and configured bound into chunk rescue.
    fn refuse_over_cap(&self, inputs: &[&str]) -> Result<(), LlmError> {
        let Some(max) = self.caps.max_input_chars else {
            return Ok(());
        };
        let max = max.get() as usize;
        for input in inputs {
            let chars = input.chars().count();
            if chars > max {
                return Err(LlmError::InputRefused(EmbeddingInputRefusal::LocalCap {
                    actual_chars: chars,
                    max_chars: max,
                }));
            }
        }
        Ok(())
    }

    async fn embed_call(&self, inputs: &[&str]) -> Result<Vec<Vec<f32>>, LlmError> {
        self.refuse_over_cap(inputs)?;
        let body = EmbedRequest {
            model: &self.model_id,
            input: inputs,
            dimensions: self.caps.matryoshka.then_some(self.caps.dim),
        };

        let mut req = self.client.post(self.endpoint.clone()).json(&body);
        if let Some(token) = &self.config.bearer_token {
            req = req.bearer_auth(token);
        }
        let mut resp = req
            .send()
            .await
            .map_err(|error| LlmError::retryable(format!("HTTP send: {}", error.without_url())))?;

        let status = resp.status();
        let retry_after = retry_after_delay(resp.headers(), SystemTime::now());
        if status.is_redirection() {
            return Err(embed_http_error(
                status,
                retry_after,
                "HTTP redirect refused: embedding client does not follow redirects",
            ));
        }
        if !status.is_success() {
            let mut error = embed_http_error(status, retry_after, "");
            if let LlmError::Retryable { message, .. } = &mut error {
                // Diagnostics cannot delay an already known provider refusal.
                match read_error_prefix(&mut resp).now_or_never() {
                    Some(Ok(prefix)) => message.push_str(utf8_prefix(&prefix)),
                    Some(Err(error)) => {
                        write!(message, "HTTP body read: {}", error.without_url())
                            .expect("write to String is infallible");
                    }
                    None => {}
                }
            }
            return Err(error);
        }

        let body_limit = response_body_limit(inputs.len(), self.caps.dim);
        let body = read_body(&mut resp, body_limit).await?;
        let text_body = std::str::from_utf8(&body).map_err(|error| {
            LlmError::retryable(format!(
                "decode OpenAI-compatible envelope as UTF-8: {error}"
            ))
        })?;
        let parsed: OpenAiEmbeddingResponse = serde_json::from_str(text_body).map_err(|e| {
            LlmError::retryable(format!(
                "decode OpenAI-compatible envelope: {:?} at line {} column {}; body: {}",
                e.classify(),
                e.line(),
                e.column(),
                utf8_prefix(&body),
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
            .ok_or_else(|| LlmError::retryable("OpenAI-compatible response had no embeddings"))
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

    use proxima_core::PlaintextHosts;
    use proxima_core::llm::{
        EmbeddingClient, EmbeddingConfigurationFault, EmbeddingInputRefusal, LlmError,
        MIN_EMBED_INPUT_CAP_CHARS,
    };
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
                matches!(result, Err(LlmError::Retryable { .. })),
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
                matches!(result, Err(LlmError::Retryable { .. })),
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
            Err(LlmError::Retryable { .. })
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
            assert!(matches!(
                decode_vectors(data, 2),
                Err(LlmError::Retryable { .. })
            ));
        }
    }

    fn decode_raw_vectors(body: &str, expected_count: usize) -> Result<Vec<Vec<f32>>, LlmError> {
        let response: super::OpenAiEmbeddingResponse = serde_json::from_str(body)
            .map_err(|err| LlmError::retryable(format!("decode response: {err}")))?;
        response.into_embeddings(expected_count, EmbedCaps::new(2, false))
    }

    #[test]
    fn response_rejects_float32_overflow() {
        // Use wire JSON, not json!: finite f64 values can overflow when
        // deserialized into the response's f32 components.
        for (body, expected) in [
            (
                r#"{"data":[{"index":0,"embedding":[1.0,-0.5]}]}"#,
                [1.0_f32, -0.5],
            ),
            (r#"{"data":[{"embedding":[0.0,0.0]}]}"#, [0.0, 0.0]),
            (
                r#"{"data":[{"embedding":[3.4028234e38,-3.4028234e38]}]}"#,
                [f32::MAX, -f32::MAX],
            ),
            (
                r#"{"data":[{"embedding":[-0.0,1e-45]}]}"#,
                [-0.0, f32::from_bits(1)],
            ),
        ] {
            let vectors = decode_raw_vectors(body, 1).expect("finite response remains valid");
            assert_eq!(vectors.len(), 1);
            assert_eq!(vectors[0].len(), 2);
            assert!(vectors[0].iter().all(|value| value.is_finite()));
            assert_eq!(
                vectors[0]
                    .iter()
                    .map(|value| value.to_bits())
                    .collect::<Vec<_>>(),
                expected.map(f32::to_bits),
                "accepted values are not normalized, clamped or rewritten",
            );
        }
        for (body, count) in [
            (r#"{"data":[{"embedding":[1.0]}]}"#, 1),
            (r#"{"data":[{"embedding":[1.0,0.0]}]}"#, 2),
        ] {
            assert!(matches!(
                decode_raw_vectors(body, count),
                Err(LlmError::Retryable { .. })
            ));
        }
        let observed: Vec<_> = ["1e39", "-1e39", "1e100", "-1e100"]
            .into_iter()
            .map(|number| {
                let body = format!(r#"{{"data":[{{"index":0,"embedding":[{number},0.0]}}]}}"#);
                (number, decode_raw_vectors(&body, 1))
            })
            .collect();
        for (number, result) in observed {
            assert!(
                matches!(result, Err(LlmError::Retryable { .. })),
                "out-of-range component {number} must be a retryable malformed response: {result:?}",
            );
        }
    }

    #[test]
    fn response_checks_later_vectors_and_components() {
        // The invalid component is last in the second logical vector, even
        // when the provider returns the indexed batch in reverse order.
        for body in [
            r#"{"data":[{"embedding":[1.0,0.0]},{"embedding":[0.0,1e39]}]}"#,
            r#"{"data":[{"index":1,"embedding":[0.0,-1e39]},{"index":0,"embedding":[1.0,0.0]}]}"#,
        ] {
            let result = decode_raw_vectors(body, 2);
            assert!(
                matches!(result, Err(LlmError::Retryable { ref message, .. })
                    if message == "embedding 1 has non-finite component at position 1"),
                "later invalid values must reject the whole batch with bounded context: {result:?}",
            );
        }
    }

    #[test]
    fn response_shape_errors_precede_non_finite_components() {
        for (body, count, expected_error) in [
            (
                r#"{"data":[{"index":7,"embedding":[1e39,0.0]}]}"#,
                2,
                "requested 2 embeddings, response carried 1",
            ),
            (
                r#"{"data":[{"index":0,"embedding":[1e39,0.0]},{"index":0,"embedding":[1.0]}]}"#,
                2,
                "invalid embedding response index",
            ),
            (
                r#"{"data":[{"index":0,"embedding":[1e39,0.0]},{"index":1,"embedding":[1.0]}]}"#,
                2,
                "expected dim 2 (matryoshka=false), got 1",
            ),
        ] {
            let result = decode_raw_vectors(body, count);
            assert!(
                matches!(result, Err(LlmError::Retryable { ref message, .. }) if message.starts_with(expected_error)),
                "existing shape error takes precedence over overflow: {result:?}",
            );
        }
    }

    fn probe_caps() -> EmbedCaps {
        EmbedCaps::new(8, false)
    }

    /// An endpoint that cannot answer. Any request that actually leaves the
    /// process fails here, which is what makes "the input was refused before
    /// it was sent" observable: the guard returns a typed local input refusal,
    /// while a request returns a retryable transport failure.
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
            Err(LlmError::InputRefused(EmbeddingInputRefusal::LocalCap {
                actual_chars,
                max_chars,
            })) => {
                assert_eq!(actual_chars, MIN_EMBED_INPUT_CAP_CHARS + 1);
                assert_eq!(max_chars, MIN_EMBED_INPUT_CAP_CHARS);
            }
            other => panic!("expected a local input refusal before sending, got {other:?}"),
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
            matches!(
                err,
                LlmError::Retryable {
                    status: None,
                    retry_after: None,
                    ..
                }
            ),
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
                Err(LlmError::InputRefused(
                    EmbeddingInputRefusal::LocalCap { .. }
                ))
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
            matches!(
                err,
                LlmError::Retryable {
                    status: None,
                    retry_after: None,
                    ..
                }
            ),
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
    fn a_named_host_is_accepted_as_the_plaintext_base_url_and_others_are_not() {
        let build = |base: &str, hosts: Option<&str>| {
            let mut cfg = super::OpenAiCompatConfig::new(base, Some("t".into()));
            if let Some(hosts) = hosts {
                cfg = cfg.with_plaintext_hosts(PlaintextHosts::parse(hosts).unwrap());
            }
            super::OpenAiCompatEmbeddingClient::new("test-embed", probe_caps(), cfg)
        };
        build("http://gateway:9090/v1", Some("gateway")).expect("named host accepted");
        for (base, hosts) in [
            ("http://gateway:9090/v1", None),
            ("http://other:9090/v1", Some("gateway")),
            ("http://gateway.evil.example:9090/v1", Some("gateway")),
        ] {
            let err = build(base, hosts).expect_err("unnamed plaintext host refused");
            assert!(matches!(err, LlmError::Internal(_)), "{base} {hosts:?}");
        }
        build("http://127.0.0.1:9/v1", None).expect("loopback unchanged");
        build("https://api.example.com/v1", None).expect("https unchanged");
    }

    #[test]
    fn client_rejects_base_url_fragments_at_construction() {
        for base in ["http://127.0.0.1:9/v1#anchor", "https://api.example/v1#"] {
            let result = super::OpenAiCompatEmbeddingClient::new(
                "test-embed",
                probe_caps(),
                super::OpenAiCompatConfig::new(base, None),
            );
            assert!(
                matches!(result, Err(LlmError::Internal(_))),
                "fragment-bearing base must fail at construction: {base}",
            );
        }
    }

    pub(super) fn assert_http_failure(
        error: &LlmError,
        status: u16,
        retry_after: Option<std::time::Duration>,
    ) {
        let input = match status {
            413 => Some(EmbeddingInputRefusal::PayloadTooLarge),
            422 => Some(EmbeddingInputRefusal::UnprocessableContent),
            _ => None,
        };
        if let Some(expected) = input {
            assert!(
                matches!(error, LlmError::InputRefused(actual) if *actual == expected),
                "{error:?}"
            );
            return;
        }
        let configuration = match status {
            400 => Some(EmbeddingConfigurationFault::BadRequest),
            401 => Some(EmbeddingConfigurationFault::Unauthorized),
            403 => Some(EmbeddingConfigurationFault::Forbidden),
            404 => Some(EmbeddingConfigurationFault::NotFound),
            _ => None,
        };
        if let Some(expected) = configuration {
            assert!(
                matches!(error, LlmError::Configuration(actual) if *actual == expected),
                "{error:?}"
            );
            return;
        }
        let LlmError::Retryable {
            status: actual_status,
            retry_after: actual_delay,
            ..
        } = error
        else {
            panic!("status {status} must retain work: {error:?}");
        };
        assert_eq!(*actual_status, Some(status));
        assert_eq!(*actual_delay, retry_after);
    }

    #[test]
    fn status_policy_is_independent_of_error_body() {
        let bodies = [
            "",
            "EOF",
            r#"{"error":{"message":"input exceeds the configured limit"}}"#,
            r#"{"message":"temporary service condition"}"#,
            "arbitrary text with no protocol meaning",
        ];
        let retry_after = Some(std::time::Duration::from_mins(2));
        for code in (100..200).chain(300..1_000) {
            let status = reqwest::StatusCode::from_u16(code).expect("valid HTTP status");
            for body in bodies {
                let error = super::embed_http_error(status, retry_after, body);
                assert_http_failure(&error, code, retry_after);
                if let LlmError::Retryable { message, .. } = error {
                    assert!(message.ends_with(body));
                }
            }
        }
    }

    fn retry_after(value: Option<&str>, now: std::time::SystemTime) -> Option<std::time::Duration> {
        let mut headers = reqwest::header::HeaderMap::new();
        if let Some(value) = value {
            headers.insert(
                reqwest::header::RETRY_AFTER,
                value.parse().expect("test header"),
            );
        }
        super::retry_after_delay(&headers, now)
    }

    #[test]
    fn retry_after_parses_delta_seconds_with_conservative_overflow() {
        use std::time::{Duration, SystemTime};
        for (value, expected) in [
            ("0", Duration::ZERO),
            ("123", Duration::from_secs(123)),
            (" 00120 ", Duration::from_mins(2)),
            ("18446744073709551615", Duration::from_secs(u64::MAX)),
            ("18446744073709551616", Duration::MAX),
            (&"9".repeat(300), Duration::MAX),
        ] {
            assert_eq!(
                retry_after(Some(value), SystemTime::UNIX_EPOCH),
                Some(expected),
                "{value}"
            );
        }
    }

    #[test]
    fn retry_after_parses_http_dates_against_the_supplied_clock() {
        use std::time::{Duration, SystemTime};
        let now = SystemTime::UNIX_EPOCH + Duration::from_mins(13_068_529);
        for value in [
            "Sun, 06 Nov 1994 08:49:37 GMT",
            "Sunday, 06-Nov-94 08:49:37 GMT",
            "Sun Nov  6 08:49:37 1994",
        ] {
            assert_eq!(retry_after(Some(value), now), Some(Duration::from_secs(37)));
            assert_eq!(
                retry_after(Some(value), now + Duration::from_secs(37)),
                Some(Duration::ZERO)
            );
            assert_eq!(
                retry_after(Some(value), now + Duration::from_mins(1)),
                Some(Duration::ZERO)
            );
        }
    }

    #[test]
    fn retry_after_missing_or_invalid_is_absent() {
        use std::time::SystemTime;
        for value in [
            None,
            Some(""),
            Some(" "),
            Some("-1"),
            Some("+1"),
            Some("1.5"),
            Some("tomorrow"),
            Some("123, 456"),
            Some("１２３"),
            Some("Sun, 40 Nov 1994 08:49:37 GMT"),
        ] {
            assert_eq!(
                retry_after(value, SystemTime::UNIX_EPOCH),
                None,
                "{value:?}"
            );
        }
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::RETRY_AFTER,
            reqwest::header::HeaderValue::from_bytes(&[0xff]).expect("opaque header bytes"),
        );
        assert_eq!(
            super::retry_after_delay(&headers, SystemTime::UNIX_EPOCH),
            None
        );
    }
}

#[cfg(test)]
mod redirect_tests {
    use std::net::SocketAddr;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use proxima_core::PlaintextHosts;
    use proxima_core::llm::{EmbeddingClient, LlmError};
    use proxima_core::models::EmbedCaps;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    use super::{OpenAiCompatConfig, OpenAiCompatEmbeddingClient};

    enum Target {
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

    const PATIENCE: Duration = Duration::from_secs(30);

    struct ResponseTask(tokio::task::JoinHandle<()>);

    impl ResponseTask {
        async fn complete(mut self) {
            tokio::time::timeout(PATIENCE, &mut self.0)
                .await
                .expect("server completes before deadline")
                .expect("server task completes");
        }
    }

    impl Drop for ResponseTask {
        fn drop(&mut self) {
            self.0.abort();
        }
    }

    async fn spawn_response_server(response: String) -> (SocketAddr, ResponseTask) {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind server");
        let addr = listener.local_addr().expect("server address");
        let task = tokio::spawn(async move {
            tokio::time::timeout(PATIENCE, async {
                let (mut stream, _) = listener.accept().await.expect("accept request");
                let _ = read_request(&mut stream).await;
                let _ = stream.write_all(response.as_bytes()).await;
            })
            .await
            .expect("response finishes before deadline");
        });
        (addr, ResponseTask(task))
    }

    async fn spawn_body_server(status: u16, body: String) -> (SocketAddr, ResponseTask) {
        spawn_response_server(format!(
            "HTTP/1.1 {status} Response\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        ))
        .await
    }

    struct HeldResponse {
        addr: SocketAddr,
        headers_sent: tokio::sync::oneshot::Receiver<()>,
        release_body: tokio::sync::oneshot::Sender<()>,
        task: ResponseTask,
    }

    async fn spawn_held_response(status: u16) -> HeldResponse {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind held server");
        let addr = listener.local_addr().expect("server address");
        let (headers_tx, headers_sent) = tokio::sync::oneshot::channel();
        let (release_body, body_released) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            tokio::time::timeout(PATIENCE * 2, async {
                let (mut stream, _) = listener.accept().await.expect("accept request");
                let _ = read_request(&mut stream).await;
                let headers = format!(
                    "HTTP/1.1 {status} Response\r\nRetry-After: 120\r\nContent-Length: 4\r\nConnection: close\r\n\r\n"
                );
                stream.write_all(headers.as_bytes()).await.expect("write headers");
                stream.flush().await.expect("flush headers");
                headers_tx.send(()).expect("announce headers");
                if body_released.await.is_ok() {
                    let _ = stream.write_all(b"body").await;
                }
            }).await.expect("held response deadline");
        });
        HeldResponse {
            addr,
            headers_sent,
            release_body,
            task: ResponseTask(task),
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
        client_for_hosts(server, &PlaintextHosts::none())
    }

    fn client_for_hosts(
        server: &RedirectServer,
        hosts: &PlaintextHosts,
    ) -> OpenAiCompatEmbeddingClient {
        let mut client = OpenAiCompatEmbeddingClient::new(
            "test-embed",
            EmbedCaps::new(2, false),
            OpenAiCompatConfig::new(format!("http://{}/v1", server.addr), None)
                .with_plaintext_hosts(hosts.clone()),
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
    async fn embedding_base_query_preserves_request_path_and_parameters() {
        for (base_path, expected_target) in [
            (
                "/v1?api-version=2026-09&tenant=a%2Fb&route=/",
                "/v1/embeddings?api-version=2026-09&tenant=a%2Fb&route=/",
            ),
            ("/v1/?mode=x&mode=y", "/v1/embeddings?mode=x&mode=y"),
            ("/v1%2Ftenant?mode=x", "/v1%2Ftenant/embeddings?mode=x"),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind server");
            let addr = listener.local_addr().expect("server address");
            let request_task = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.expect("accept request");
                let request = read_request(&mut stream).await;
                let body = r#"{"data":[{"index":0,"embedding":[1.0,0.0]}]}"#;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream
                    .write_all(response.as_bytes())
                    .await
                    .expect("respond");
                request
            });
            let client = OpenAiCompatEmbeddingClient::new(
                "test-embed",
                EmbedCaps::new(2, false),
                OpenAiCompatConfig::new(format!("http://{addr}{base_path}"), None)
                    .with_timeout(Duration::from_secs(5)),
            )
            .expect("query-bearing base is valid");
            assert_eq!(
                client.embed("test text").await.expect("embed"),
                vec![1.0, 0.0]
            );
            let request = request_task.await.expect("request captured");
            assert_eq!(
                request.lines().next().expect("request line"),
                format!("POST {expected_target} HTTP/1.1"),
            );
        }
    }

    #[tokio::test]
    async fn successful_http_response_with_overflow_is_retryable() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind server");
        let addr = listener.local_addr().expect("server address");
        let request_task = tokio::spawn(async move {
            tokio::time::timeout(Duration::from_secs(5), async move {
                let (mut stream, _) = listener.accept().await.expect("accept request");
                let request = read_request(&mut stream).await;
                let body = r#"{"data":[{"index":0,"embedding":[0.0,1e39]}]}"#;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream
                    .write_all(response.as_bytes())
                    .await
                    .expect("respond");
                request
            })
            .await
            .expect("local response completes")
        });
        let mut client = OpenAiCompatEmbeddingClient::new(
            "test-embed",
            EmbedCaps::new(2, false),
            OpenAiCompatConfig::new(format!("http://{addr}/v1"), None),
        )
        .expect("loopback embedding endpoint");
        client.client = crate::build_http_client(
            reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .no_proxy(),
        )
        .expect("test client with production response handling");
        let result = client.embed("test text").await;
        let request = request_task.await.expect("request captured");
        assert!(request.starts_with("POST /v1/embeddings HTTP/1.1\r\n"));
        assert!(
            matches!(result, Err(LlmError::Retryable { ref message, .. })
                if message == "embedding 0 has non-finite component at position 1"),
            "HTTP success must not accept an invalid vector: {result:?}",
        );
    }

    #[tokio::test]
    async fn successful_response_larger_than_derived_limit_is_refused() {
        let body = format!(
            "{{\"data\":[{{\"embedding\":[1.0,0.0]}}]}}{}",
            " ".repeat(70_000)
        );
        let (addr, task) = spawn_body_server(200, body).await;
        let client = OpenAiCompatEmbeddingClient::new(
            "test-embed",
            EmbedCaps::new(2, false),
            OpenAiCompatConfig::new(format!("http://{addr}/v1"), None),
        )
        .expect("loopback embedding endpoint");
        let result = client.embed("test text").await;
        task.complete().await;
        assert!(
            matches!(result, Err(LlmError::Retryable { ref message, .. }) if message.contains("exceeded the 65600-byte limit")),
            "{result:?}"
        );
    }

    #[tokio::test]
    async fn malformed_success_body_is_limited_to_a_utf8_safe_prefix() {
        for (body, expected) in [
            (
                format!("{}€{}", "x".repeat(510), "y".repeat(1024 * 1024 - 513)),
                "x".repeat(510),
            ),
            (
                format!(r#"{{"data":"{}"}}"#, "x".repeat(1024 * 1024 - 11)),
                format!("{{\"data\":\"{}", "x".repeat(503)),
            ),
        ] {
            let (addr, task) = spawn_body_server(200, body).await;
            let client = OpenAiCompatEmbeddingClient::new(
                "test-embed",
                EmbedCaps::new(32_768, false),
                OpenAiCompatConfig::new(format!("http://{addr}/v1"), None),
            )
            .expect("loopback embedding endpoint");
            let result = client.embed("test text").await;
            task.complete().await;
            let Err(LlmError::Retryable { message, .. }) = result else {
                panic!("expected bounded decode error: {result:?}");
            };
            let (_, detail) = message.split_once("; body: ").expect("JSON decode error");
            assert!(detail.len() <= super::ERROR_BODY_PREFIX_BYTES);
            assert_eq!(detail, expected);
            assert!(message.len() < 700);
        }
    }

    #[tokio::test]
    async fn error_body_reader_is_limited_to_a_utf8_safe_prefix() {
        let body = format!("{}€{}", "a".repeat(510), "b".repeat(1024 * 1024 - 513));
        let (addr, task) = spawn_body_server(500, body).await;
        let client = OpenAiCompatEmbeddingClient::new(
            "test-embed",
            EmbedCaps::new(2, false),
            OpenAiCompatConfig::new(format!("http://{addr}/v1"), None),
        )
        .expect("loopback embedding endpoint");
        let mut response = client
            .client
            .post(client.endpoint.clone())
            .send()
            .await
            .expect("HTTP response");
        let prefix = tokio::time::timeout(PATIENCE, super::read_error_prefix(&mut response))
            .await
            .expect("bounded reader finishes")
            .expect("complete body");
        assert_eq!(prefix.len(), super::ERROR_BODY_PREFIX_BYTES);
        let status = response.status();
        drop(response);
        task.complete().await;
        let error = super::embed_http_error(status, None, super::utf8_prefix(&prefix));
        let LlmError::Retryable { message, .. } = error else {
            panic!("expected bounded HTTP error: {error:?}");
        };
        let detail = message
            .strip_prefix("openai-compatible /embeddings returned 500 Internal Server Error: ")
            .expect("HTTP error prefix");
        assert!(detail.len() <= super::ERROR_BODY_PREFIX_BYTES);
        assert_eq!(detail, "a".repeat(510));
    }

    #[tokio::test]
    async fn http_failures_keep_status_and_retry_after_when_diagnostics_fail() {
        for status in [400, 401, 403, 404, 413, 422, 429, 451, 503] {
            for content_length in [4, 100] {
                let response = format!(
                    "HTTP/1.1 {status} Response\r\nRetry-After: 120\r\nContent-Length: {content_length}\r\nConnection: close\r\n\r\nbody"
                );
                let (addr, task) = spawn_response_server(response).await;
                let client = OpenAiCompatEmbeddingClient::new(
                    "test-embed",
                    EmbedCaps::new(2, false),
                    OpenAiCompatConfig::new(format!("http://{addr}/v1"), None),
                )
                .expect("loopback embedding endpoint");
                let error = client.embed("test text").await.expect_err("HTTP refusal");
                task.complete().await;
                super::tests::assert_http_failure(&error, status, Some(Duration::from_mins(2)));
                if let LlmError::Retryable { message, .. } = error {
                    assert!(message.len() < 700, "diagnostic remains bounded: {message}");
                }
            }
        }
    }

    #[tokio::test]
    async fn error_body_reader_distinguishes_complete_and_truncated_bodies() {
        for content_length in [4, 100] {
            let response = format!(
                "HTTP/1.1 503 Response\r\nContent-Length: {content_length}\r\nConnection: close\r\n\r\nbody"
            );
            let (addr, task) = spawn_response_server(response).await;
            let client = OpenAiCompatEmbeddingClient::new(
                "test-embed",
                EmbedCaps::new(2, false),
                OpenAiCompatConfig::new(format!("http://{addr}/v1"), None),
            )
            .expect("loopback embedding endpoint");
            let mut response = client
                .client
                .post(client.endpoint.clone())
                .send()
                .await
                .expect("HTTP headers");
            let prefix = tokio::time::timeout(PATIENCE, super::read_error_prefix(&mut response))
                .await
                .expect("reader finishes within deadline");
            task.complete().await;
            if content_length == 4 {
                assert_eq!(prefix.expect("complete body"), b"body");
            } else {
                assert!(
                    prefix.is_err(),
                    "truncated diagnostic must surface a body-read error"
                );
            }
        }
    }

    #[tokio::test]
    async fn stalled_error_bodies_cannot_overwrite_captured_status_or_retry_after() {
        for status in [401, 413, 429] {
            let HeldResponse {
                addr,
                headers_sent,
                release_body,
                task,
            } = spawn_held_response(status).await;
            let client = OpenAiCompatEmbeddingClient::new(
                "test-embed",
                EmbedCaps::new(2, false),
                OpenAiCompatConfig::new(format!("http://{addr}/v1"), None)
                    .with_timeout(PATIENCE * 2),
            )
            .expect("loopback embedding endpoint");
            let (result, marker) = tokio::join!(
                proxima_core::llm::embed_with_timeout(&client, "test text", PATIENCE),
                tokio::time::timeout(PATIENCE, headers_sent),
            );
            marker
                .expect("headers sent before deadline")
                .expect("positive headers marker");
            let error = result.expect_err("HTTP refusal before diagnostic body is released");
            super::tests::assert_http_failure(&error, status, Some(Duration::from_mins(2)));
            release_body
                .send(())
                .expect("body was held until classification finished");
            task.complete().await;
        }
    }

    #[tokio::test]
    async fn redirect_failure_keeps_headers_without_consuming_the_body() {
        let response = "HTTP/1.1 307 Temporary Redirect\r\nRetry-After: 90\r\nLocation: http://redirect.example/target\r\nContent-Length: 100\r\nConnection: close\r\n\r\nx".to_owned();
        let (addr, task) = spawn_response_server(response).await;
        let client = OpenAiCompatEmbeddingClient::new(
            "test-embed",
            EmbedCaps::new(2, false),
            OpenAiCompatConfig::new(format!("http://{addr}/v1"), None),
        )
        .expect("loopback embedding endpoint");
        let error = client
            .embed("test text")
            .await
            .expect_err("redirect refusal");
        task.complete().await;
        super::tests::assert_http_failure(&error, 307, Some(Duration::from_secs(90)));
        assert!(error.to_string().contains("redirect refused"), "{error}");
        assert!(!error.to_string().contains("HTTP body read:"), "{error}");
    }

    #[tokio::test]
    async fn every_redirect_status_is_refused_without_reading_its_body() {
        for status in [300, 301, 302, 303, 304, 305, 306, 307, 308, 399] {
            let (addr, task) = spawn_body_server(status, "x".repeat(1_048_576)).await;
            let client = OpenAiCompatEmbeddingClient::new(
                "test-embed",
                EmbedCaps::new(2, false),
                OpenAiCompatConfig::new(format!("http://{addr}/v1"), None),
            )
            .expect("loopback embedding endpoint");
            let result = client.embed("test text").await;
            task.complete().await;
            assert!(
                matches!(result, Err(LlmError::Retryable { ref message, .. }) if message.contains("redirect refused")),
                "status {status}: {result:?}"
            );
        }
    }

    #[tokio::test]
    async fn nonloopback_plaintext_redirect_cannot_receive_embedding_input() {
        for status in [307, 308] {
            let server = spawn_redirect_server(status, Target::RemoteHttp).await;
            let result = client_for(&server).embed("private memory text").await;
            assert_eq!(server.target_requests.load(Ordering::SeqCst), 0);
            assert_eq!(server.requests.load(Ordering::SeqCst), 1);
            assert!(
                matches!(result, Err(LlmError::Retryable { ref message, .. }) if message.contains("redirect refused")),
                "{result:?}"
            );
        }
    }

    #[tokio::test]
    async fn redirects_are_refused_even_for_named_plaintext_hosts() {
        for status in [307, 308] {
            let server = spawn_redirect_server(status, Target::RemoteHttp).await;
            let named = PlaintextHosts::parse("redirect.example").unwrap();
            let result = client_for_hosts(&server, &named)
                .embed("private memory text")
                .await;
            assert_eq!(server.target_requests.load(Ordering::SeqCst), 0);
            assert_eq!(server.requests.load(Ordering::SeqCst), 1);
            assert!(
                matches!(result, Err(LlmError::Retryable { ref message, .. }) if message.contains("redirect refused")),
                "{result:?}"
            );

            let server = spawn_redirect_server(status, Target::RemoteHttp).await;
            let other = PlaintextHosts::parse("gateway").unwrap();
            let result = client_for_hosts(&server, &other)
                .embed("private memory text")
                .await;
            assert_eq!(server.target_requests.load(Ordering::SeqCst), 0);
            assert_eq!(server.requests.load(Ordering::SeqCst), 1);
            assert!(
                matches!(result, Err(LlmError::Retryable { ref message, .. }) if message.contains("redirect refused")),
                "{result:?}"
            );
        }
    }

    #[tokio::test]
    async fn loopback_redirects_are_refused() {
        for status in [307, 308] {
            let target = TcpListener::bind("127.0.0.1:0")
                .await
                .expect("loopback target");
            let target_addr = target.local_addr().expect("target address");
            let source = TcpListener::bind("127.0.0.1:0")
                .await
                .expect("provider stub");
            let source_addr = source.local_addr().expect("source address");
            let task = tokio::spawn(async move {
                let (mut stream, _) = source.accept().await.expect("provider request");
                let _ = read_request(&mut stream).await;
                let response = format!(
                    "HTTP/1.1 {status} Redirect\r\nLocation: http://{target_addr}/target\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
                stream
                    .write_all(response.as_bytes())
                    .await
                    .expect("redirect");
            });
            let mut client = OpenAiCompatEmbeddingClient::new(
                "test-embed",
                EmbedCaps::new(2, false),
                OpenAiCompatConfig::new(format!("http://{source_addr}/v1"), None),
            )
            .expect("provider stub endpoint");
            client.client = crate::build_http_client(
                reqwest::Client::builder()
                    .timeout(Duration::from_millis(500))
                    .no_proxy(),
            )
            .expect("production redirect policy");
            let result = client.embed("private memory text").await;
            task.await.expect("provider responded");
            assert!(
                tokio::time::timeout(Duration::from_millis(20), target.accept())
                    .await
                    .is_err(),
                "redirect target must receive no connection"
            );
            assert!(
                matches!(result, Err(LlmError::Retryable { ref message, .. }) if message.contains("redirect refused")),
                "{result:?}"
            );
        }
    }

    #[tokio::test]
    async fn redirect_loops_are_refused_without_hops() {
        let server = spawn_redirect_server(307, Target::Loop).await;
        let result = client_for(&server).embed("private memory text").await;
        assert!(
            matches!(result, Err(LlmError::Retryable { ref message, .. }) if message.contains("redirect refused")),
            "{result:?}"
        );
        assert_eq!(server.requests.load(Ordering::SeqCst), 1);
        assert_eq!(server.target_requests.load(Ordering::SeqCst), 0);
    }
}
