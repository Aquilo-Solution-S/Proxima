//! Embedding-client contracts used by vector retrieval.

use async_trait::async_trait;
use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("LLM call failed: {0}")]
    Llm(String),
    #[error("embedding call failed: {0}")]
    Embed(String),
    /// Embedding rejected for a cause retries cannot fix (e.g. input over
    /// the model's token limit — HTTP 400/413/422). Jobs hitting this must
    /// fail terminally instead of burning retry attempts forever.
    #[error("embedding permanently rejected: {0}")]
    EmbedPermanent(String),
    #[error("output validation failed: {0}")]
    OutputValidation(String),
    #[error("internal: {0}")]
    Internal(String),
}

pub const EMBEDDING_DIM: usize = 1024;

pub const PROXIMA_EMBED_REQUEST_TIMEOUT_SECONDS: &str = "PROXIMA_EMBED_REQUEST_TIMEOUT_SECONDS";
pub const PROXIMA_EMBED_BATCH_SIZE: &str = "PROXIMA_EMBED_BATCH_SIZE";
pub const PROXIMA_EMBED_WORKER_INTERVAL_SECONDS: &str = "PROXIMA_EMBED_WORKER_INTERVAL_SECONDS";
pub const PROXIMA_EMBED_STALE_CLAIM_TIMEOUT_SECONDS: &str =
    "PROXIMA_EMBED_STALE_CLAIM_TIMEOUT_SECONDS";

pub const DEFAULT_EMBED_REQUEST_TIMEOUT: Duration = Duration::from_mins(2);
pub const DEFAULT_EMBED_BATCH_SIZE: usize = 32;
pub const DEFAULT_EMBED_WORKER_INTERVAL: Duration = Duration::from_secs(5);
pub const DEFAULT_EMBED_STALE_CLAIM_TIMEOUT: Duration = Duration::from_mins(15);

pub const MAX_EMBED_REQUEST_TIMEOUT: Duration = Duration::from_hours(1);
pub const MAX_EMBED_BATCH_SIZE: usize = 1_024;
pub const MAX_EMBED_WORKER_INTERVAL: Duration = Duration::from_hours(1);
pub const MAX_EMBED_STALE_CLAIM_TIMEOUT: Duration = Duration::from_hours(24);

/// Generic host policy for durable embedding work.
///
/// The engine and maintenance boundaries apply the request timeout to every
/// installed client call. The shipped OpenAI-compatible adapter also applies
/// it at the HTTP layer. Claims are renewed on a separate task every third of
/// `stale_claim_timeout`, so poison isolation and chunk rescue may make
/// several bounded provider calls without looking abandoned to a concurrent
/// reconciler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmbeddingRuntimePolicy {
    request_timeout: Duration,
    batch_size: usize,
    worker_interval: Duration,
    stale_claim_timeout: Duration,
}

impl Default for EmbeddingRuntimePolicy {
    fn default() -> Self {
        Self {
            request_timeout: DEFAULT_EMBED_REQUEST_TIMEOUT,
            batch_size: DEFAULT_EMBED_BATCH_SIZE,
            worker_interval: DEFAULT_EMBED_WORKER_INTERVAL,
            stale_claim_timeout: DEFAULT_EMBED_STALE_CLAIM_TIMEOUT,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EmbeddingRuntimePolicyError {
    #[error("{key} must be integer seconds, got {value:?}")]
    MalformedSeconds { key: &'static str, value: String },
    #[error("{key} must be a positive integer, got {value:?}")]
    MalformedBatch { key: &'static str, value: String },
    #[error("{field} must be in {min}..={max} seconds, got {actual}")]
    DurationOutOfRange {
        field: &'static str,
        min: u64,
        max: u64,
        actual: u64,
    },
    #[error("batch size must be in 1..={max}, got {actual}")]
    BatchSizeOutOfRange { max: usize, actual: usize },
    #[error("{field} must be an integral number of seconds")]
    NonIntegralSeconds { field: &'static str },
    #[error(
        "PROXIMA_EMBED_STALE_CLAIM_TIMEOUT_SECONDS ({stale_seconds}s) must be strictly greater than PROXIMA_EMBED_REQUEST_TIMEOUT_SECONDS ({request_seconds}s)"
    )]
    StaleClaimNotLongerThanRequest {
        stale_seconds: u64,
        request_seconds: u64,
    },
}

impl EmbeddingRuntimePolicy {
    /// Construct and validate a host embedding policy.
    ///
    /// # Errors
    ///
    /// Rejects zero, out-of-range, and unsafe stale-claim values.
    pub fn new(
        request_timeout: Duration,
        batch_size: usize,
        worker_interval: Duration,
        stale_claim_timeout: Duration,
    ) -> Result<Self, EmbeddingRuntimePolicyError> {
        validate_duration(
            PROXIMA_EMBED_REQUEST_TIMEOUT_SECONDS,
            request_timeout,
            MAX_EMBED_REQUEST_TIMEOUT,
        )?;
        if !(1..=MAX_EMBED_BATCH_SIZE).contains(&batch_size) {
            return Err(EmbeddingRuntimePolicyError::BatchSizeOutOfRange {
                max: MAX_EMBED_BATCH_SIZE,
                actual: batch_size,
            });
        }
        validate_duration(
            PROXIMA_EMBED_WORKER_INTERVAL_SECONDS,
            worker_interval,
            MAX_EMBED_WORKER_INTERVAL,
        )?;
        validate_duration(
            PROXIMA_EMBED_STALE_CLAIM_TIMEOUT_SECONDS,
            stale_claim_timeout,
            MAX_EMBED_STALE_CLAIM_TIMEOUT,
        )?;
        if stale_claim_timeout <= request_timeout {
            return Err(
                EmbeddingRuntimePolicyError::StaleClaimNotLongerThanRequest {
                    stale_seconds: stale_claim_timeout.as_secs(),
                    request_seconds: request_timeout.as_secs(),
                },
            );
        }
        Ok(Self {
            request_timeout,
            batch_size,
            worker_interval,
            stale_claim_timeout,
        })
    }

    /// Parse the canonical `PROXIMA_EMBED_*` policy block through an injected
    /// lookup. Unset fields retain finite defaults; empty values are unset.
    ///
    /// # Errors
    ///
    /// Rejects malformed, zero, out-of-range, and unsafe combinations.
    pub fn from_lookup(
        lookup: &impl Fn(&str) -> Option<String>,
    ) -> Result<Self, EmbeddingRuntimePolicyError> {
        let defaults = Self::default();
        let request_timeout = parse_duration_setting(
            lookup,
            PROXIMA_EMBED_REQUEST_TIMEOUT_SECONDS,
            defaults.request_timeout,
        )?;
        let batch_size = match crate::env_value(lookup, PROXIMA_EMBED_BATCH_SIZE) {
            Some(raw) => {
                raw.parse::<usize>()
                    .map_err(|_| EmbeddingRuntimePolicyError::MalformedBatch {
                        key: PROXIMA_EMBED_BATCH_SIZE,
                        value: raw,
                    })?
            }
            None => defaults.batch_size,
        };
        let worker_interval = parse_duration_setting(
            lookup,
            PROXIMA_EMBED_WORKER_INTERVAL_SECONDS,
            defaults.worker_interval,
        )?;
        let stale_claim_timeout = parse_duration_setting(
            lookup,
            PROXIMA_EMBED_STALE_CLAIM_TIMEOUT_SECONDS,
            defaults.stale_claim_timeout,
        )?;
        Self::new(
            request_timeout,
            batch_size,
            worker_interval,
            stale_claim_timeout,
        )
    }

    #[must_use]
    pub const fn request_timeout(self) -> Duration {
        self.request_timeout
    }

    #[must_use]
    pub const fn batch_size(self) -> usize {
        self.batch_size
    }

    #[must_use]
    pub const fn worker_interval(self) -> Duration {
        self.worker_interval
    }

    #[must_use]
    pub const fn stale_claim_timeout(self) -> Duration {
        self.stale_claim_timeout
    }

    #[must_use]
    pub fn claim_heartbeat_interval(self) -> Duration {
        self.stale_claim_timeout / 3
    }

    #[must_use]
    pub fn stale_claim_timeout_seconds(self) -> i64 {
        i64::try_from(self.stale_claim_timeout.as_secs()).unwrap_or(i64::MAX)
    }
}

fn validate_duration(
    field: &'static str,
    value: Duration,
    max: Duration,
) -> Result<(), EmbeddingRuntimePolicyError> {
    if value.subsec_nanos() != 0 {
        return Err(EmbeddingRuntimePolicyError::NonIntegralSeconds { field });
    }
    if value < Duration::from_secs(1) || value > max {
        return Err(EmbeddingRuntimePolicyError::DurationOutOfRange {
            field,
            min: 1,
            max: max.as_secs(),
            actual: value.as_secs(),
        });
    }
    Ok(())
}

fn parse_duration_setting(
    lookup: &impl Fn(&str) -> Option<String>,
    key: &'static str,
    default: Duration,
) -> Result<Duration, EmbeddingRuntimePolicyError> {
    let Some(raw) = crate::env_value(lookup, key) else {
        return Ok(default);
    };
    let seconds = raw
        .parse::<u64>()
        .map_err(|_| EmbeddingRuntimePolicyError::MalformedSeconds { key, value: raw })?;
    Ok(Duration::from_secs(seconds))
}

/// Embedding client surface. Concrete impls live outside core.
#[async_trait]
pub trait EmbeddingClient: Send + Sync + std::fmt::Debug {
    async fn embed(&self, text: &str) -> Result<Vec<f32>, LlmError>;

    /// Embed several texts in one provider call, one vector per input, in
    /// input order. The default falls back to sequential single embeds so
    /// existing impls stay correct; batching impls should override.
    async fn embed_many(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, LlmError> {
        let mut out = Vec::with_capacity(texts.len());
        for text in texts {
            out.push(self.embed(text).await?);
        }
        Ok(out)
    }

    fn model_id(&self) -> &str;
    fn dim(&self) -> usize;
}

/// Run one provider request under the host's generic request deadline.
///
/// Timeout is a retryable provider failure. This boundary is required even
/// for custom clients; adapter-native timeouts remain useful for cancelling
/// socket work promptly.
///
/// # Errors
///
/// Returns the client's error, or retryable [`LlmError::Embed`] when the
/// deadline elapses.
pub async fn embed_with_timeout(
    client: &dyn EmbeddingClient,
    text: &str,
    request_timeout: Duration,
) -> Result<Vec<f32>, LlmError> {
    tokio::time::timeout(request_timeout, client.embed(text))
        .await
        .map_err(|_| {
            LlmError::Embed(format!(
                "request timed out after {} seconds",
                request_timeout.as_secs()
            ))
        })?
}

/// Run one batched provider request under the host's generic request
/// deadline. Timeout is retryable.
///
/// # Errors
///
/// Returns the client's error, or retryable [`LlmError::Embed`] when the
/// deadline elapses.
pub async fn embed_many_with_timeout(
    client: &dyn EmbeddingClient,
    texts: &[String],
    request_timeout: Duration,
) -> Result<Vec<Vec<f32>>, LlmError> {
    tokio::time::timeout(request_timeout, client.embed_many(texts))
        .await
        .map_err(|_| {
            LlmError::Embed(format!(
                "request timed out after {} seconds",
                request_timeout.as_secs()
            ))
        })?
}

/// Trivial liveness probe after a provider refuses a real input.
/// Short enough that no cap, context window, or token limit can refuse it.
#[doc(hidden)]
pub const EMBED_LIVENESS_PROBE: &str = "ok";

/// Whether an embedding failure is the input's fault.
///
/// [`LlmError::EmbedPermanent`]: the provider refused this input. Every
/// other variant is ambiguous (a runner that dies on the input looks like
/// a runner that was already down). Probe [`EMBED_LIVENESS_PROBE`]: if it
/// succeeds, blame the input; if it fails, the provider is down.
/// Same question [`crate::Engine::drain_embedding_jobs`] asks of a batch.
pub(crate) async fn embed_failure_blames_the_input(
    client: &dyn EmbeddingClient,
    err: &LlmError,
) -> bool {
    if matches!(err, LlmError::EmbedPermanent(_)) {
        return true;
    }
    client.embed(EMBED_LIVENESS_PROBE).await.is_ok()
}

/// Smallest piece, in bytes, the chunked-embedding rescue is willing to
/// produce. A segment whose halves would fall below this is not split
/// further: at that size a rejection is read as genuinely invalid input
/// rather than an over-limit one, so the effective floor on a segment the
/// rescue will still bisect is twice this value.
pub const CHUNKED_EMBED_MIN_BYTES: usize = 2048;

/// Lowest [`crate::models::EmbedCaps::max_input_chars`] [`embed_in_chunks`]
/// can still satisfy. Coupled to the cap: over-long input is
/// [`LlmError::EmbedPermanent`], which triggers bisection — a cap below
/// this floor turns a rescuable input terminal.
///
/// Floor = largest piece the split can emit. A segment is cut only when
/// each half is still ≥ [`CHUNKED_EMBED_MIN_BYTES`], so a piece is at most
/// `2 * CHUNKED_EMBED_MIN_BYTES - 1` bytes. Character count ≤ byte count.
/// The `- 1` is load-bearing (test: halves land exactly on the boundary).
pub const MIN_EMBED_INPUT_CAP_CHARS: usize = 2 * CHUNKED_EMBED_MIN_BYTES - 1;

/// Bisect an over-limit input into pieces the provider accepts.
///
/// Rejection does not say *why*: over-limit starts embedding once pieces
/// are short enough; any other reason fails all the way down (`Ok(None)`).
/// Halves below [`CHUNKED_EMBED_MIN_BYTES`] abort — partial coverage would
/// mask poison input.
///
/// Lives here, not on `Engine`: both the in-process drain and
/// `maintain-embeddings --drain` must rescue the same way.
///
/// `Ok(Some(vectors))` in text order, `Ok(None)` if every length is
/// refused, `Err` on the first transient provider error.
///
/// # Errors
///
/// First non-`EmbedPermanent` provider error.
pub async fn embed_in_chunks(
    client: &dyn EmbeddingClient,
    text: &str,
) -> Result<Option<Vec<Vec<f32>>>, LlmError> {
    embed_in_chunks_with(client, text, |client, segment| {
        Box::pin(client.embed(segment))
    })
    .await
}

/// [`embed_in_chunks`] with a deadline around every provider request.
///
/// One rescue can legitimately issue several calls; each individual call,
/// rather than the whole rescue, receives the configured request budget.
///
/// # Errors
///
/// Returns the first non-permanent client error, including retryable
/// [`LlmError::Embed`] when an individual request deadline elapses.
pub async fn embed_in_chunks_with_timeout(
    client: &dyn EmbeddingClient,
    text: &str,
    request_timeout: Duration,
) -> Result<Option<Vec<Vec<f32>>>, LlmError> {
    embed_in_chunks_with(client, text, |client, segment| {
        Box::pin(embed_with_timeout(client, segment, request_timeout))
    })
    .await
}

async fn embed_in_chunks_with<'a, F>(
    client: &'a dyn EmbeddingClient,
    text: &'a str,
    mut embed: F,
) -> Result<Option<Vec<Vec<f32>>>, LlmError>
where
    F: for<'b> FnMut(
        &'b dyn EmbeddingClient,
        &'b str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Vec<f32>, LlmError>> + Send + 'b>,
    >,
{
    // Depth-first, left-to-right bisection keeps chunk vectors in text
    // order without recursion (async fns don't recurse).
    let mut pending: Vec<&str> = vec![text];
    let mut vectors: Vec<Vec<f32>> = Vec::new();
    while let Some(segment) = pending.pop() {
        match embed(client, segment).await {
            Ok(vector) => vectors.push(vector),
            Err(LlmError::EmbedPermanent(_)) => {
                let mut cut = segment.len() / 2;
                while cut > 0 && !segment.is_char_boundary(cut) {
                    cut -= 1;
                }
                if cut < CHUNKED_EMBED_MIN_BYTES {
                    return Ok(None);
                }
                // Pop order: push right half first so the left half embeds
                // (or splits) next.
                pending.push(&segment[cut..]);
                pending.push(&segment[..cut]);
            }
            Err(err) => return Err(err),
        }
    }
    Ok(Some(vectors))
}

#[cfg(test)]
mod tests {
    use super::{
        CHUNKED_EMBED_MIN_BYTES, DEFAULT_EMBED_BATCH_SIZE, DEFAULT_EMBED_REQUEST_TIMEOUT,
        DEFAULT_EMBED_STALE_CLAIM_TIMEOUT, DEFAULT_EMBED_WORKER_INTERVAL, EmbeddingClient,
        EmbeddingRuntimePolicy, EmbeddingRuntimePolicyError, LlmError, MIN_EMBED_INPUT_CAP_CHARS,
        PROXIMA_EMBED_BATCH_SIZE, PROXIMA_EMBED_REQUEST_TIMEOUT_SECONDS,
        PROXIMA_EMBED_STALE_CLAIM_TIMEOUT_SECONDS, PROXIMA_EMBED_WORKER_INTERVAL_SECONDS,
        embed_in_chunks, embed_in_chunks_with_timeout, embed_many_with_timeout, embed_with_timeout,
    };
    use async_trait::async_trait;
    use std::sync::Mutex;
    use std::time::Duration;

    fn lookup<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |key| {
            pairs
                .iter()
                .find(|(candidate, _)| *candidate == key)
                .map(|(_, value)| (*value).to_string())
        }
    }

    #[test]
    fn embedding_runtime_policy_has_finite_defaults() {
        let policy = EmbeddingRuntimePolicy::default();
        assert_eq!(policy.request_timeout(), DEFAULT_EMBED_REQUEST_TIMEOUT);
        assert_eq!(policy.batch_size(), DEFAULT_EMBED_BATCH_SIZE);
        assert_eq!(policy.worker_interval(), DEFAULT_EMBED_WORKER_INTERVAL);
        assert_eq!(
            policy.stale_claim_timeout(),
            DEFAULT_EMBED_STALE_CLAIM_TIMEOUT
        );
        assert_eq!(policy.claim_heartbeat_interval(), Duration::from_mins(5));
    }

    #[test]
    fn embedding_runtime_policy_parses_canonical_env_block() {
        let policy = EmbeddingRuntimePolicy::from_lookup(&lookup(&[
            (PROXIMA_EMBED_REQUEST_TIMEOUT_SECONDS, "30"),
            (PROXIMA_EMBED_BATCH_SIZE, "17"),
            (PROXIMA_EMBED_WORKER_INTERVAL_SECONDS, "9"),
            (PROXIMA_EMBED_STALE_CLAIM_TIMEOUT_SECONDS, "91"),
        ]))
        .expect("valid policy");
        assert_eq!(policy.request_timeout(), Duration::from_secs(30));
        assert_eq!(policy.batch_size(), 17);
        assert_eq!(policy.worker_interval(), Duration::from_secs(9));
        assert_eq!(policy.stale_claim_timeout(), Duration::from_secs(91));
    }

    #[test]
    fn embedding_runtime_policy_rejects_bad_values_and_unsafe_relation() {
        for (key, value) in [
            (PROXIMA_EMBED_REQUEST_TIMEOUT_SECONDS, "0"),
            (PROXIMA_EMBED_REQUEST_TIMEOUT_SECONDS, "3601"),
            (PROXIMA_EMBED_BATCH_SIZE, "0"),
            (PROXIMA_EMBED_BATCH_SIZE, "1025"),
            (PROXIMA_EMBED_WORKER_INTERVAL_SECONDS, "nope"),
            (PROXIMA_EMBED_STALE_CLAIM_TIMEOUT_SECONDS, "86401"),
        ] {
            let err = EmbeddingRuntimePolicy::from_lookup(&lookup(&[(key, value)]))
                .expect_err("invalid setting must fail");
            assert!(err.to_string().contains(key) || err.to_string().contains("batch size"));
        }

        let err = EmbeddingRuntimePolicy::from_lookup(&lookup(&[
            (PROXIMA_EMBED_REQUEST_TIMEOUT_SECONDS, "120"),
            (PROXIMA_EMBED_STALE_CLAIM_TIMEOUT_SECONDS, "120"),
        ]))
        .expect_err("stale claim must outlive one request");
        assert!(err.to_string().contains("strictly greater"));
    }

    #[test]
    fn embedding_runtime_policy_rejects_fractional_programmatic_durations() {
        for (field, result) in [
            (
                PROXIMA_EMBED_REQUEST_TIMEOUT_SECONDS,
                EmbeddingRuntimePolicy::new(
                    Duration::from_millis(1_500),
                    2,
                    Duration::from_secs(1),
                    Duration::from_secs(3),
                ),
            ),
            (
                PROXIMA_EMBED_WORKER_INTERVAL_SECONDS,
                EmbeddingRuntimePolicy::new(
                    Duration::from_secs(1),
                    2,
                    Duration::from_millis(1_500),
                    Duration::from_secs(3),
                ),
            ),
            (
                PROXIMA_EMBED_STALE_CLAIM_TIMEOUT_SECONDS,
                EmbeddingRuntimePolicy::new(
                    Duration::from_secs(1),
                    2,
                    Duration::from_secs(1),
                    Duration::from_millis(3_500),
                ),
            ),
        ] {
            assert!(
                matches!(
                    result,
                    Err(EmbeddingRuntimePolicyError::NonIntegralSeconds { field: actual })
                        if actual == field
                ),
                "fractional {field} must be rejected"
            );
        }
    }

    #[derive(Debug)]
    struct HangingEmbedding;

    #[async_trait]
    impl EmbeddingClient for HangingEmbedding {
        async fn embed(&self, _text: &str) -> Result<Vec<f32>, LlmError> {
            std::future::pending().await
        }

        async fn embed_many(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>, LlmError> {
            std::future::pending().await
        }

        fn model_id(&self) -> &'static str {
            "hanging"
        }

        fn dim(&self) -> usize {
            4
        }
    }

    #[tokio::test(start_paused = true)]
    async fn request_timeout_bounds_single_and_batch_custom_client_calls() {
        let timeout = Duration::from_secs(1);
        for result in [
            embed_with_timeout(&HangingEmbedding, "one", timeout)
                .await
                .map(|_| ()),
            embed_many_with_timeout(&HangingEmbedding, &["one".to_owned()], timeout)
                .await
                .map(|_| ()),
        ] {
            assert!(
                matches!(result, Err(LlmError::Embed(ref message)) if message.contains("timed out")),
                "timeout must be a retryable embedding error: {result:?}"
            );
        }
    }

    #[derive(Debug)]
    struct RescueThenHang {
        calls: std::sync::atomic::AtomicUsize,
    }

    #[async_trait]
    impl EmbeddingClient for RescueThenHang {
        async fn embed(&self, _text: &str) -> Result<Vec<f32>, LlmError> {
            if self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                return Err(LlmError::EmbedPermanent("split me".into()));
            }
            std::future::pending().await
        }

        fn model_id(&self) -> &'static str {
            "rescue-then-hang"
        }

        fn dim(&self) -> usize {
            4
        }
    }

    #[tokio::test(start_paused = true)]
    async fn request_timeout_bounds_each_chunk_rescue_provider_call() {
        let client = RescueThenHang {
            calls: std::sync::atomic::AtomicUsize::new(0),
        };
        let result = embed_in_chunks_with_timeout(
            &client,
            &"a".repeat(CHUNKED_EMBED_MIN_BYTES * 2),
            Duration::from_secs(1),
        )
        .await;
        assert!(
            matches!(result, Err(LlmError::Embed(ref message)) if message.contains("timed out")),
            "a hung chunk request must be retryable: {result:?}"
        );
    }

    /// A provider that refuses over-cap input the way a client-side
    /// `max_input_chars` does — and records every length it was offered, so a
    /// test can assert what was *sent*, not merely what came back.
    #[derive(Debug)]
    struct CappedEmbedding {
        max_chars: usize,
        offered: Mutex<Vec<usize>>,
    }

    impl CappedEmbedding {
        fn new(max_chars: usize) -> Self {
            Self {
                max_chars,
                offered: Mutex::new(Vec::new()),
            }
        }

        fn accepted(&self) -> Vec<usize> {
            let offered = self
                .offered
                .lock()
                .expect("no test holds this across a panic");
            offered
                .iter()
                .copied()
                .filter(|chars| *chars <= self.max_chars)
                .collect()
        }
    }

    #[async_trait]
    impl EmbeddingClient for CappedEmbedding {
        async fn embed(&self, text: &str) -> Result<Vec<f32>, LlmError> {
            let chars = text.chars().count();
            self.offered
                .lock()
                .expect("no test holds this across a panic")
                .push(chars);
            if chars > self.max_chars {
                return Err(LlmError::EmbedPermanent(format!(
                    "input of {chars} chars exceeds the {}-char limit",
                    self.max_chars
                )));
            }
            Ok(vec![0.0; 4])
        }

        fn model_id(&self) -> &'static str {
            "capped"
        }

        fn dim(&self) -> usize {
            4
        }
    }

    /// The cap and the rescue have to compose, because the cap's refusal is
    /// what triggers the rescue. Everything over the limit comes back split,
    /// and every piece actually embedded is within it.
    #[tokio::test]
    async fn a_client_side_cap_is_rescued_into_chunks() {
        let client = CappedEmbedding::new(MIN_EMBED_INPUT_CAP_CHARS);
        let text = "a".repeat(MIN_EMBED_INPUT_CAP_CHARS * 5);

        let vectors = embed_in_chunks(&client, &text)
            .await
            .expect("a cap refusal is not a transient error")
            .expect("an over-cap input is splittable, not invalid");

        assert!(vectors.len() > 1, "an over-cap input must come back split");
        assert_eq!(
            vectors.len(),
            client.accepted().len(),
            "one vector per piece the provider accepted",
        );
        for chars in client.accepted() {
            assert!(
                chars <= MIN_EMBED_INPUT_CAP_CHARS,
                "a piece of {chars} chars was embedded above the cap",
            );
        }
    }

    /// Floor is a real bound: rescued at the floor, terminal one char below.
    /// Input length must be `2 * MIN_EMBED_INPUT_CAP_CHARS` so halves land
    /// on the widest piece the split can emit.
    #[tokio::test]
    async fn one_char_under_the_floor_is_the_difference_between_split_and_terminal() {
        // Halves to exactly MIN_EMBED_INPUT_CAP_CHARS, the widest piece the
        // bisection can hand the provider.
        let text = "a".repeat(2 * MIN_EMBED_INPUT_CAP_CHARS);

        let at_floor = CappedEmbedding::new(MIN_EMBED_INPUT_CAP_CHARS);
        assert_eq!(
            embed_in_chunks(&at_floor, &text)
                .await
                .expect("not a transient error")
                .map(|vectors| vectors.len()),
            Some(2),
            "at the floor the widest piece is acceptable, so the rescue finishes",
        );

        let under_floor = CappedEmbedding::new(MIN_EMBED_INPUT_CAP_CHARS - 1);
        assert!(
            embed_in_chunks(&under_floor, &text)
                .await
                .expect("not a transient error")
                .is_none(),
            "one char lower, that piece is refused and is too small to split \
             again, so a rescuable input goes terminal — which is why a cap \
             under MIN_EMBED_INPUT_CAP_CHARS is refused at construction",
        );
    }

    /// The floor is derived, so it must move if what it is derived from
    /// moves.
    #[test]
    fn the_floor_tracks_the_split_minimum_it_is_derived_from() {
        assert_eq!(MIN_EMBED_INPUT_CAP_CHARS, 2 * CHUNKED_EMBED_MIN_BYTES - 1);
    }

    /// Smallest cap that works for every length: the test above builds its
    /// input from the constant, so it would pass a range of wrong values.
    #[tokio::test]
    async fn the_floor_is_the_smallest_cap_that_works_for_every_input() {
        async fn rescues_every_length(cap: usize) -> bool {
            // Every length whose halves can straddle the split minimum.
            for len in CHUNKED_EMBED_MIN_BYTES..=(4 * CHUNKED_EMBED_MIN_BYTES) {
                let client = CappedEmbedding::new(cap);
                let rescued = embed_in_chunks(&client, &"a".repeat(len))
                    .await
                    .expect("not a transient error");
                if rescued.is_none() {
                    return false;
                }
            }
            true
        }

        assert!(
            rescues_every_length(MIN_EMBED_INPUT_CAP_CHARS).await,
            "the floor must leave no input un-rescuable",
        );
        assert!(
            !rescues_every_length(MIN_EMBED_INPUT_CAP_CHARS - 1).await,
            "one char below the floor some input must fail, or the floor is \
             higher than it needs to be and refuses workable configurations",
        );
    }

    /// A cap is not the only reason a provider says `EmbedPermanent`. Input
    /// that is invalid rather than long is refused all the way down, and the
    /// caller must see that as "no rescue" so the job goes terminal instead
    /// of cycling — the behaviour the cap must not change.
    #[tokio::test]
    async fn input_refused_at_every_length_is_not_rescued() {
        let client = CappedEmbedding::new(0);
        let outcome = embed_in_chunks(&client, &"a".repeat(MIN_EMBED_INPUT_CAP_CHARS * 4))
            .await
            .expect("not a transient error");
        assert!(outcome.is_none(), "nothing was acceptable at any length");
    }
}
