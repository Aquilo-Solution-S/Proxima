//! Embedding-client contracts used by vector retrieval.

use async_trait::async_trait;
use std::time::Duration;

mod query;
mod route;
mod space;

pub use query::{
    QUERY_PLACEHOLDER, QueryInstruction, QueryInstructionError, QueryInstructions, QueryTask,
    SemanticWeight, SemanticWeightError,
};
pub use route::{EmbeddingRoute, EmbeddingRouteError, EmbeddingRouter, SingleClientRouter};
pub use space::{
    BoundEmbeddingClient, EmbeddingDim, EmbeddingSpace, SpaceVector, UnsupportedEmbeddingWidth,
    VectorWidthMismatch,
};

/// A submitted input was refused; isolation and chunk rescue may still succeed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum EmbeddingInputRefusal {
    #[error("HTTP 413: embedding input is too large")]
    PayloadTooLarge,
    #[error("HTTP 422: embedding input cannot be processed")]
    UnprocessableContent,
    #[error(
        "embedding input has {actual_chars} characters, exceeding the local cap of {max_chars}"
    )]
    LocalCap {
        actual_chars: usize,
        max_chars: usize,
    },
}

impl EmbeddingInputRefusal {
    /// The provider's refusing status, or no HTTP status for a local cap.
    #[must_use]
    pub const fn status(self) -> Option<u16> {
        match self {
            Self::PayloadTooLarge => Some(413),
            Self::UnprocessableContent => Some(422),
            Self::LocalCap { .. } => None,
        }
    }
}

/// A client configuration fault pauses that original client until rebinding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, thiserror::Error)]
pub enum EmbeddingConfigurationFault {
    #[error("embedding configuration fault: HTTP 400")]
    BadRequest,
    #[error("embedding configuration fault: HTTP 401")]
    Unauthorized,
    #[error("embedding configuration fault: HTTP 403")]
    Forbidden,
    #[error("embedding configuration fault: HTTP 404")]
    NotFound,
}

impl EmbeddingConfigurationFault {
    /// The configuration refusal's HTTP status, safe for health projection.
    #[must_use]
    pub const fn status(self) -> u16 {
        match self {
            Self::BadRequest => 400,
            Self::Unauthorized => 401,
            Self::Forbidden => 403,
            Self::NotFound => 404,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("{0}")]
    InputRefused(EmbeddingInputRefusal),
    #[error("{0}")]
    Configuration(EmbeddingConfigurationFault),
    #[error("embedding call failed: {message}")]
    Retryable {
        status: Option<u16>,
        retry_after: Option<Duration>,
        message: String,
    },
    #[error("internal: {0}")]
    Internal(String),
}

impl LlmError {
    /// An ordinary non-HTTP failure with no provider retry timing.
    #[must_use]
    pub fn retryable(message: impl Into<String>) -> Self {
        Self::Retryable {
            status: None,
            retry_after: None,
            message: message.into(),
        }
    }
}

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
/// The engine and maintenance boundaries apply one request deadline across
/// waiting for the shared client's dispatch slot and the provider call. The
/// shipped OpenAI-compatible adapter also applies its timeout at the HTTP layer.
/// Claims are renewed on a separate task every third of
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
/// Returns the client's error, or retryable [`LlmError::Retryable`] when the
/// deadline elapses.
pub async fn embed_with_timeout(
    client: &dyn EmbeddingClient,
    text: &str,
    request_timeout: Duration,
) -> Result<Vec<f32>, LlmError> {
    tokio::time::timeout(request_timeout, client.embed(text))
        .await
        .map_err(|_| {
            LlmError::retryable(format!(
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
/// Returns the client's error, or retryable [`LlmError::Retryable`] when the
/// deadline elapses.
pub async fn embed_many_with_timeout(
    client: &dyn EmbeddingClient,
    texts: &[String],
    request_timeout: Duration,
) -> Result<Vec<Vec<f32>>, LlmError> {
    tokio::time::timeout(request_timeout, client.embed_many(texts))
        .await
        .map_err(|_| {
            LlmError::retryable(format!(
                "request timed out after {} seconds",
                request_timeout.as_secs()
            ))
        })?
}

/// Smallest piece, in bytes, the chunked-embedding rescue can produce.
/// A rejected segment is split only if both halves reach this floor.
pub const CHUNKED_EMBED_MIN_BYTES: usize = 2048;

/// Lowest [`crate::models::EmbedCaps::max_input_chars`] [`embed_in_chunks`]
/// can satisfy. A cap below this floor can turn a rescuable input terminal.
///
/// Floor = largest piece the split can emit. A segment is cut only when
/// each half is still at least [`CHUNKED_EMBED_MIN_BYTES`], so a piece is at
/// most `2 * CHUNKED_EMBED_MIN_BYTES - 1` bytes. Characters never exceed bytes.
pub const MIN_EMBED_INPUT_CAP_CHARS: usize = 2 * CHUNKED_EMBED_MIN_BYTES - 1;

/// Complete chunk coverage or the final refusal at the minimum split size.
#[derive(Debug, PartialEq)]
pub enum ChunkedEmbedding {
    Embedded(Vec<Vec<f32>>),
    InputRefused(EmbeddingInputRefusal),
}

/// Bisect explicitly refused input into accepted pieces in text order.
/// Only [`LlmError::InputRefused`] enters rescue. Partial coverage is never
/// returned: a later non-input error discards earlier successful pieces.
///
/// # Errors
///
/// Returns the first provider failure that is not an input refusal.
pub async fn embed_in_chunks(
    client: &dyn EmbeddingClient,
    text: &str,
) -> Result<ChunkedEmbedding, LlmError> {
    embed_in_chunks_with(client, text, None, |client, segment| {
        Box::pin(client.embed(segment))
    })
    .await
}

/// [`embed_in_chunks`] with the deadline applied separately to every call.
///
/// # Errors
///
/// Returns the first non-input failure, including a request timeout.
pub async fn embed_in_chunks_with_timeout(
    client: &dyn EmbeddingClient,
    text: &str,
    request_timeout: Duration,
) -> Result<ChunkedEmbedding, LlmError> {
    embed_in_chunks_with(client, text, None, |client, segment| {
        Box::pin(embed_with_timeout(client, segment, request_timeout))
    })
    .await
}

/// Rescue a known input refusal without submitting the full input again.
///
/// A retryable failure cannot authorize input splitting.
///
/// ```compile_fail
/// # use proxima_core::llm::{EmbeddingClient, LlmError, embed_in_chunks_after_failure};
/// # fn rescue(client: &dyn EmbeddingClient) {
/// let _ = embed_in_chunks_after_failure(client, "text", LlmError::retryable("busy"));
/// # }
/// ```
///
/// # Errors
///
/// Returns the first later provider failure that is not an input refusal.
pub async fn embed_in_chunks_after_failure(
    client: &dyn EmbeddingClient,
    text: &str,
    refusal: EmbeddingInputRefusal,
) -> Result<ChunkedEmbedding, LlmError> {
    embed_in_chunks_with(client, text, Some(refusal), |client, segment| {
        Box::pin(client.embed(segment))
    })
    .await
}

/// Timeout-bounded form of [`embed_in_chunks_after_failure`].
///
/// # Errors
///
/// Returns the first later non-input failure, including a request timeout.
pub async fn embed_in_chunks_after_failure_with_timeout(
    client: &dyn EmbeddingClient,
    text: &str,
    refusal: EmbeddingInputRefusal,
    request_timeout: Duration,
) -> Result<ChunkedEmbedding, LlmError> {
    embed_in_chunks_with(client, text, Some(refusal), |client, segment| {
        Box::pin(embed_with_timeout(client, segment, request_timeout))
    })
    .await
}

/// A LIFO stack takes the left half first, preserving text order.
fn push_left_half_first<'a>(pending: &mut Vec<&'a str>, left: &'a str, right: &'a str) {
    pending.push(right);
    pending.push(left);
}

async fn embed_in_chunks_with<'a, F>(
    client: &'a dyn EmbeddingClient,
    text: &'a str,
    initial_refusal: Option<EmbeddingInputRefusal>,
    mut embed: F,
) -> Result<ChunkedEmbedding, LlmError>
where
    F: for<'b> FnMut(
        &'b dyn EmbeddingClient,
        &'b str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Vec<f32>, LlmError>> + Send + 'b>,
    >,
{
    let mut pending = Vec::new();
    let mut vectors = Vec::new();
    if let Some(refusal) = initial_refusal {
        let Some((left, right)) = split_segment(text) else {
            return Ok(ChunkedEmbedding::InputRefused(refusal));
        };
        push_left_half_first(&mut pending, left, right);
    } else {
        pending.push(text);
    }
    while let Some(segment) = pending.pop() {
        match embed(client, segment).await {
            Ok(vector) => vectors.push(vector),
            Err(LlmError::InputRefused(refusal)) => {
                let Some((left, right)) = split_segment(segment) else {
                    return Ok(ChunkedEmbedding::InputRefused(refusal));
                };
                push_left_half_first(&mut pending, left, right);
            }
            Err(error) => return Err(error),
        }
    }
    Ok(ChunkedEmbedding::Embedded(vectors))
}

fn split_segment(segment: &str) -> Option<(&str, &str)> {
    let mut cut = segment.len() / 2;
    while cut > 0 && !segment.is_char_boundary(cut) {
        cut -= 1;
    }
    if cut < CHUNKED_EMBED_MIN_BYTES {
        return None;
    }
    Some((&segment[..cut], &segment[cut..]))
}

#[cfg(test)]
mod tests {
    use super::{
        CHUNKED_EMBED_MIN_BYTES, ChunkedEmbedding, DEFAULT_EMBED_BATCH_SIZE,
        DEFAULT_EMBED_REQUEST_TIMEOUT, DEFAULT_EMBED_STALE_CLAIM_TIMEOUT,
        DEFAULT_EMBED_WORKER_INTERVAL, EmbeddingClient, EmbeddingConfigurationFault,
        EmbeddingInputRefusal, EmbeddingRuntimePolicy, EmbeddingRuntimePolicyError, LlmError,
        MIN_EMBED_INPUT_CAP_CHARS, PROXIMA_EMBED_BATCH_SIZE, PROXIMA_EMBED_REQUEST_TIMEOUT_SECONDS,
        PROXIMA_EMBED_STALE_CLAIM_TIMEOUT_SECONDS, PROXIMA_EMBED_WORKER_INTERVAL_SECONDS,
        embed_in_chunks, embed_in_chunks_after_failure, embed_in_chunks_after_failure_with_timeout,
        embed_in_chunks_with_timeout, embed_many_with_timeout, embed_with_timeout,
    };
    use crate::env_from_pairs as lookup;
    use async_trait::async_trait;
    use std::sync::Mutex;
    use std::time::Duration;

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
                matches!(result, Err(LlmError::Retryable { ref message, .. }) if message.contains("timed out")),
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
                return Err(LlmError::InputRefused(
                    EmbeddingInputRefusal::PayloadTooLarge,
                ));
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
            matches!(result, Err(LlmError::Retryable { ref message, .. }) if message.contains("timed out")),
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
                return Err(LlmError::InputRefused(EmbeddingInputRefusal::LocalCap {
                    actual_chars: chars,
                    max_chars: self.max_chars,
                }));
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

        let ChunkedEmbedding::Embedded(vectors) = embed_in_chunks(&client, &text)
            .await
            .expect("a cap refusal is not a transient error")
        else {
            panic!("an over-cap input is splittable, not invalid");
        };

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

    #[derive(Debug)]
    struct ScriptedEmbedding {
        replies: Mutex<std::collections::VecDeque<Result<Vec<f32>, LlmError>>>,
        offered: Mutex<Vec<String>>,
    }

    impl ScriptedEmbedding {
        fn new(replies: impl IntoIterator<Item = Result<Vec<f32>, LlmError>>) -> Self {
            Self {
                replies: Mutex::new(replies.into_iter().collect()),
                offered: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl EmbeddingClient for ScriptedEmbedding {
        async fn embed(&self, text: &str) -> Result<Vec<f32>, LlmError> {
            self.offered.lock().expect("offered").push(text.into());
            if text == "ok" {
                return Ok(vec![0.0; 4]);
            }
            self.replies
                .lock()
                .expect("replies")
                .pop_front()
                .expect("unexpected provider request")
        }
        fn model_id(&self) -> &'static str {
            "scripted"
        }
        fn dim(&self) -> usize {
            4
        }
    }

    #[tokio::test]
    async fn non_input_refusals_never_probe_or_split_even_when_a_probe_would_succeed() {
        for length in [10, MIN_EMBED_INPUT_CAP_CHARS * 5] {
            let errors = [
                LlmError::retryable("network unavailable"),
                LlmError::Internal("client fault".into()),
                LlmError::Configuration(EmbeddingConfigurationFault::BadRequest),
                LlmError::Configuration(EmbeddingConfigurationFault::Unauthorized),
                LlmError::Configuration(EmbeddingConfigurationFault::Forbidden),
                LlmError::Configuration(EmbeddingConfigurationFault::NotFound),
            ]
            .into_iter()
            .chain(
                [408, 429, 500, 502, 503, 504].map(|status| LlmError::Retryable {
                    status: Some(status),
                    retry_after: None,
                    message: "provider failure".into(),
                }),
            );
            for error in errors {
                let expected = error.to_string();
                let client = ScriptedEmbedding::new([Err(error)]);
                let text = "a".repeat(length);
                let result = embed_in_chunks(&client, &text)
                    .await
                    .expect_err("non-input failure");
                assert_eq!(
                    result.to_string(),
                    expected,
                    "preserve original non-input failure"
                );
                assert_eq!(
                    *client.offered.lock().expect("offered"),
                    [text],
                    "no probe or rescue"
                );
            }
        }
    }

    #[tokio::test]
    async fn later_non_input_failure_discards_partial_chunk_coverage() {
        for error in [
            LlmError::Retryable {
                status: Some(429),
                retry_after: Some(Duration::from_secs(90)),
                message: "busy".into(),
            },
            LlmError::Configuration(EmbeddingConfigurationFault::Unauthorized),
            LlmError::retryable("network unavailable"),
        ] {
            let expected = error.to_string();
            let client = ScriptedEmbedding::new([Ok(vec![1.0; 4]), Err(error)]);
            let text = "a".repeat(CHUNKED_EMBED_MIN_BYTES * 2);
            let result = embed_in_chunks_after_failure(
                &client,
                &text,
                EmbeddingInputRefusal::PayloadTooLarge,
            )
            .await
            .expect_err("partial chunks must not escape");
            assert_eq!(result.to_string(), expected);
            assert_eq!(
                client.offered.lock().expect("offered").len(),
                2,
                "no full-text replay or probe"
            );
        }
    }

    #[tokio::test]
    async fn chunk_rescue_preserves_utf8_order_and_final_refusing_status() {
        let text = format!(
            "{}{}",
            "é".repeat(CHUNKED_EMBED_MIN_BYTES / 2),
            "界".repeat(CHUNKED_EMBED_MIN_BYTES)
        );
        let client = ScriptedEmbedding::new([Ok(vec![1.0; 4]), Ok(vec![2.0; 4])]);
        let coverage =
            embed_in_chunks_after_failure(&client, &text, EmbeddingInputRefusal::PayloadTooLarge)
                .await
                .expect("UTF-8 rescue");
        assert_eq!(
            coverage,
            ChunkedEmbedding::Embedded(vec![vec![1.0; 4], vec![2.0; 4]])
        );
        assert_eq!(
            client.offered.lock().expect("offered").concat(),
            text,
            "left-to-right complete coverage"
        );

        let client = ScriptedEmbedding::new([Err(LlmError::InputRefused(
            EmbeddingInputRefusal::UnprocessableContent,
        ))]);
        let coverage = embed_in_chunks_after_failure(
            &client,
            &"a".repeat(CHUNKED_EMBED_MIN_BYTES * 2),
            EmbeddingInputRefusal::PayloadTooLarge,
        )
        .await
        .expect("input refusal");
        assert_eq!(
            coverage,
            ChunkedEmbedding::InputRefused(EmbeddingInputRefusal::UnprocessableContent)
        );
        assert_eq!(client.offered.lock().expect("offered").len(), 1);

        let client = ScriptedEmbedding::new([]);
        let coverage = embed_in_chunks_after_failure_with_timeout(
            &client,
            "short",
            EmbeddingInputRefusal::PayloadTooLarge,
            Duration::from_secs(1),
        )
        .await
        .expect("known refusal requires no provider call below split floor");
        assert_eq!(
            coverage,
            ChunkedEmbedding::InputRefused(EmbeddingInputRefusal::PayloadTooLarge)
        );
        assert!(client.offered.lock().expect("offered").is_empty());
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
                .expect("not a transient error"),
            ChunkedEmbedding::Embedded(vec![vec![0.0; 4]; 2]),
            "at the floor the widest piece is acceptable, so the rescue finishes",
        );

        let under_floor = CappedEmbedding::new(MIN_EMBED_INPUT_CAP_CHARS - 1);
        assert!(
            matches!(
                embed_in_chunks(&under_floor, &text)
                    .await
                    .expect("not a transient error"),
                ChunkedEmbedding::InputRefused(_)
            ),
            "one char lower, that piece is refused and is too small to split \
             again, so a rescuable input goes terminal — which is why a cap \
             under MIN_EMBED_INPUT_CAP_CHARS is refused at construction",
        );
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
                if matches!(rescued, ChunkedEmbedding::InputRefused(_)) {
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

    /// A cap is not the only typed input refusal. Invalid input
    /// that is invalid rather than long is refused all the way down, and the
    /// caller must see that as "no rescue" so the job goes terminal instead
    /// of cycling — the behaviour the cap must not change.
    #[tokio::test]
    async fn input_refused_at_every_length_is_not_rescued() {
        let client = CappedEmbedding::new(0);
        let outcome = embed_in_chunks(&client, &"a".repeat(MIN_EMBED_INPUT_CAP_CHARS * 4))
            .await
            .expect("not a transient error");
        assert!(
            matches!(
                outcome,
                ChunkedEmbedding::InputRefused(EmbeddingInputRefusal::LocalCap {
                    max_chars: 0,
                    ..
                })
            ),
            "nothing was acceptable at any length"
        );
    }
}
