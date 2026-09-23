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
    /// the model's token limit — HTTP 413/422). Jobs hitting this must
    /// fail terminally instead of burning retry attempts forever.
    #[error("embedding permanently rejected: {0}")]
    EmbedPermanent(String),
    #[error("output validation failed: {0}")]
    OutputValidation(String),
    #[error("internal: {0}")]
    Internal(String),
}

/// A vector width the store can index.
///
/// Closed on purpose. pgvector indexes one width per HNSW index, so the
/// store keeps one partial index per width over a single `embeddings`
/// table. A width outside this set has no index to be searched through; it
/// is refused where a client is bound ([`BoundEmbeddingClient::bind`]),
/// not discovered on the first write. Widths above pgvector's
/// 2,000-dimension HNSW cap for `vector` are indexed as `halfvec`
/// ([`Self::is_halfvec_indexed`]); the stored vector keeps full precision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum EmbeddingDim {
    D384,
    D768,
    D1024,
    D1536,
    D2048,
    D3072,
}

impl EmbeddingDim {
    /// Every supported width, narrowest first.
    pub const ALL: [Self; 6] = [
        Self::D384,
        Self::D768,
        Self::D1024,
        Self::D1536,
        Self::D2048,
        Self::D3072,
    ];

    /// Number of components in a vector of this width.
    #[must_use]
    pub const fn width(self) -> usize {
        match self {
            Self::D384 => 384,
            Self::D768 => 768,
            Self::D1024 => 1024,
            Self::D1536 => 1536,
            Self::D2048 => 2048,
            Self::D3072 => 3072,
        }
    }

    /// Whether this width's index casts to `halfvec`: pgvector cannot build
    /// an HNSW index over `vector` wider than 2,000 dimensions.
    #[must_use]
    pub const fn is_halfvec_indexed(self) -> bool {
        matches!(self, Self::D2048 | Self::D3072)
    }

    /// The supported width with exactly `width` components, if any.
    #[must_use]
    pub const fn from_width(width: usize) -> Option<Self> {
        match width {
            384 => Some(Self::D384),
            768 => Some(Self::D768),
            1024 => Some(Self::D1024),
            1536 => Some(Self::D1536),
            2048 => Some(Self::D2048),
            3072 => Some(Self::D3072),
            _ => None,
        }
    }
}

impl TryFrom<usize> for EmbeddingDim {
    type Error = UnsupportedEmbeddingWidth;

    fn try_from(width: usize) -> Result<Self, Self::Error> {
        Self::from_width(width).ok_or(UnsupportedEmbeddingWidth { width })
    }
}

impl std::fmt::Display for EmbeddingDim {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.width())
    }
}

/// A client width no [`EmbeddingDim`] matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error(
    "embedding width {width} is not supported; the store indexes 384, 768, 1024, 1536, 2048 \
     and 3072 (a Matryoshka model can request one of these)"
)]
pub struct UnsupportedEmbeddingWidth {
    pub width: usize,
}

/// Where vectors live: the model that produced them and their width.
///
/// Vectors are comparable only within one space. The width is part of the
/// identity, so the same model re-embedded at another Matryoshka width is a
/// different space, not a collision.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EmbeddingSpace {
    model_id: String,
    dim: EmbeddingDim,
}

impl EmbeddingSpace {
    #[must_use]
    pub fn new(model_id: impl Into<String>, dim: EmbeddingDim) -> Self {
        Self {
            model_id: model_id.into(),
            dim,
        }
    }

    #[must_use]
    pub fn model_id(&self) -> &str {
        &self.model_id
    }

    #[must_use]
    pub const fn dim(&self) -> EmbeddingDim {
        self.dim
    }
}

impl std::fmt::Display for EmbeddingSpace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}@{}", self.model_id, self.dim)
    }
}

/// An embedding client bound to the [`EmbeddingSpace`] its vectors live in.
///
/// The engine installs only bound clients. Binding checks the client's
/// width once, so every downstream write and query names a width the store
/// indexes. Dereferences to the client.
#[derive(Debug, Clone)]
pub struct BoundEmbeddingClient {
    client: std::sync::Arc<dyn EmbeddingClient>,
    space: EmbeddingSpace,
    /// The client as the host bound it, kept through engine wrapping so two
    /// bindings of one host client still compare as the same endpoint.
    origin: std::sync::Arc<dyn EmbeddingClient>,
}

impl BoundEmbeddingClient {
    /// Bind `client` to its space.
    ///
    /// # Errors
    ///
    /// [`UnsupportedEmbeddingWidth`] when `client.dim()` is not an
    /// [`EmbeddingDim`].
    pub fn bind(
        client: std::sync::Arc<dyn EmbeddingClient>,
    ) -> Result<Self, UnsupportedEmbeddingWidth> {
        let dim = EmbeddingDim::try_from(client.dim())?;
        let space = EmbeddingSpace::new(client.model_id(), dim);
        Ok(Self {
            origin: std::sync::Arc::clone(&client),
            client,
            space,
        })
    }

    #[must_use]
    pub const fn space(&self) -> &EmbeddingSpace {
        &self.space
    }

    #[must_use]
    pub const fn client(&self) -> &std::sync::Arc<dyn EmbeddingClient> {
        &self.client
    }

    /// The same space served through `client`, which must be a wrapper
    /// around this binding's client (the engine's request-timeout layer).
    pub(crate) fn rewrap(&self, client: std::sync::Arc<dyn EmbeddingClient>) -> Self {
        Self {
            client,
            space: self.space.clone(),
            origin: std::sync::Arc::clone(&self.origin),
        }
    }

    /// Whether both bindings serve the same host client, so one embedding
    /// of a text may stand for both.
    #[must_use]
    pub fn same_client(&self, other: &Self) -> bool {
        std::ptr::addr_eq(
            std::sync::Arc::as_ptr(&self.origin),
            std::sync::Arc::as_ptr(&other.origin),
        )
    }
}

/// Where one Owner's memories are embedded.
///
/// A route is the host's answer for the Owner whose memories are written or
/// searched — never the caller's. The engine embeds that Owner's texts and
/// queries only through the route's clients, so a route naming no client
/// means no jobs, no vectors and lexical-only search for the Owner.
///
/// A route has up to two clients. `current` embeds new memories inline and
/// serves search. `next`, set while the Owner moves to another model, is
/// queued for every new memory and filled by backfill; search stays on
/// `current` until the host flips the route to `current(next)`.
#[derive(Debug, Clone, Default)]
pub struct EmbeddingRoute {
    current: Option<BoundEmbeddingClient>,
    next: Option<BoundEmbeddingClient>,
}

impl EmbeddingRoute {
    /// No embeddings for this Owner.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            current: None,
            next: None,
        }
    }

    /// Embed and search this Owner's memories through `client`.
    #[must_use]
    pub const fn current(client: BoundEmbeddingClient) -> Self {
        Self {
            current: Some(client),
            next: None,
        }
    }

    /// Search through `current` while every memory is also embedded in
    /// `next`'s space. `current: None` starts an Owner that had no
    /// embeddings on `next` without serving semantic search yet.
    ///
    /// # Errors
    ///
    /// [`EmbeddingRouteError`] when both clients embed the same space: that
    /// is no move.
    pub fn moving(
        current: Option<BoundEmbeddingClient>,
        next: BoundEmbeddingClient,
    ) -> Result<Self, EmbeddingRouteError> {
        if current
            .as_ref()
            .is_some_and(|current| current.space() == next.space())
        {
            return Err(EmbeddingRouteError::new(format!(
                "a move needs a new embedding space; both clients embed {}",
                next.space()
            )));
        }
        Ok(Self {
            current,
            next: Some(next),
        })
    }

    /// The client that embeds new memories inline and search queries.
    #[must_use]
    pub const fn current_client(&self) -> Option<&BoundEmbeddingClient> {
        self.current.as_ref()
    }

    /// The client this Owner is moving to, if a move is under way.
    #[must_use]
    pub const fn next_client(&self) -> Option<&BoundEmbeddingClient> {
        self.next.as_ref()
    }

    /// Spaces a new memory of this Owner is queued for: `current`, then
    /// `next`.
    #[must_use]
    pub fn write_spaces(&self) -> Vec<EmbeddingSpace> {
        self.clients()
            .map(|client| client.space().clone())
            .collect()
    }

    /// The client that embeds `space` for this Owner, or `None` when the
    /// route no longer names it: a job queued for such a space is stale.
    #[must_use]
    pub fn client_for(&self, space: &EmbeddingSpace) -> Option<&BoundEmbeddingClient> {
        self.clients().find(|client| client.space() == space)
    }

    fn clients(&self) -> impl Iterator<Item = &BoundEmbeddingClient> {
        self.current.iter().chain(self.next.iter())
    }

    /// The same route with every client passed through `wrap`.
    pub(crate) fn map_clients(
        self,
        wrap: impl Fn(&BoundEmbeddingClient) -> BoundEmbeddingClient,
    ) -> Self {
        Self {
            current: self.current.as_ref().map(&wrap),
            next: self.next.as_ref().map(&wrap),
        }
    }
}

/// A route the host cannot resolve for an Owner.
///
/// Misconfiguration, not an outage: fetch credentials inside the client's
/// `embed`, where the drain retries. The engine never falls back to another
/// Owner's client or a default; it refuses the write, releases the Owner's
/// jobs, or drops the Owner's semantic arm.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct EmbeddingRouteError {
    message: String,
}

impl EmbeddingRouteError {
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl From<UnsupportedEmbeddingWidth> for EmbeddingRouteError {
    fn from(err: UnsupportedEmbeddingWidth) -> Self {
        Self::new(err.to_string())
    }
}

/// Host policy: which embedding endpoint serves each Owner.
///
/// Called for the Owner of the data on every write, drain batch and search,
/// so it must be a cheap lookup of host configuration.
#[async_trait]
pub trait EmbeddingRouter: Send + Sync + std::fmt::Debug {
    /// The route for memories `owner` owns.
    ///
    /// # Errors
    ///
    /// [`EmbeddingRouteError`] when the host cannot say, for this Owner.
    async fn route(&self, owner: &crate::Owner) -> Result<EmbeddingRoute, EmbeddingRouteError>;
}

/// One client for every Owner: the single-endpoint host.
#[derive(Debug, Clone)]
pub struct SingleClientRouter {
    client: BoundEmbeddingClient,
}

impl SingleClientRouter {
    #[must_use]
    pub const fn new(client: BoundEmbeddingClient) -> Self {
        Self { client }
    }

    #[must_use]
    pub const fn client(&self) -> &BoundEmbeddingClient {
        &self.client
    }
}

#[async_trait]
impl EmbeddingRouter for SingleClientRouter {
    async fn route(&self, _owner: &crate::Owner) -> Result<EmbeddingRoute, EmbeddingRouteError> {
        Ok(EmbeddingRoute::current(self.client.clone()))
    }
}

impl AsRef<dyn EmbeddingClient> for BoundEmbeddingClient {
    fn as_ref(&self) -> &(dyn EmbeddingClient + 'static) {
        self.client.as_ref()
    }
}

impl std::ops::Deref for BoundEmbeddingClient {
    type Target = dyn EmbeddingClient;

    fn deref(&self) -> &Self::Target {
        self.client.as_ref()
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
/// A permanent rejection, or an ambiguous rejection followed by a successful
/// liveness probe, starts embedding once pieces are short enough. An
/// ambiguous rejection whose probe fails remains retryable. Halves below
/// [`CHUNKED_EMBED_MIN_BYTES`] abort — partial coverage would mask poison
/// input.
///
/// Lives here, not on `Engine`, so every drainer rescues the same way.
///
/// `Ok(Some(vectors))` in text order, `Ok(None)` if a live provider refuses
/// every length, `Err` on a failed liveness probe or another transient
/// provider error.
///
/// # Errors
///
/// First non-content-attributed provider error.
pub async fn embed_in_chunks(
    client: &dyn EmbeddingClient,
    text: &str,
) -> Result<Option<Vec<Vec<f32>>>, LlmError> {
    embed_in_chunks_with(
        client,
        text,
        None,
        |client, segment| Box::pin(client.embed(segment)),
        |client| Box::pin(client.embed(EMBED_LIVENESS_PROBE)),
    )
    .await
}

/// [`embed_in_chunks`] with a deadline around every provider request.
///
/// One rescue can legitimately issue several calls; each individual call,
/// rather than the whole rescue, receives the configured request budget.
///
/// # Errors
///
/// Returns the first non-content-attributed client error, including
/// retryable [`LlmError::Embed`] when an individual request deadline elapses
/// or its liveness probe fails.
pub async fn embed_in_chunks_with_timeout(
    client: &dyn EmbeddingClient,
    text: &str,
    request_timeout: Duration,
) -> Result<Option<Vec<Vec<f32>>>, LlmError> {
    embed_in_chunks_with(
        client,
        text,
        None,
        |client, segment| Box::pin(embed_with_timeout(client, segment, request_timeout)),
        |client| {
            Box::pin(embed_with_timeout(
                client,
                EMBED_LIVENESS_PROBE,
                request_timeout,
            ))
        },
    )
    .await
}

/// Rescue a failed embedding request by attributing an ambiguous provider
/// failure to the submitted content only when a trivial liveness probe still
/// succeeds. A failed probe returns the original retryable error; it never
/// turns a provider outage into a terminal job failure.
///
/// # Errors
///
/// Returns the original error when the liveness probe fails, or the first
/// later provider error that is not content-attributed.
pub async fn embed_in_chunks_after_failure(
    client: &dyn EmbeddingClient,
    text: &str,
    error: LlmError,
) -> Result<Option<Vec<Vec<f32>>>, LlmError> {
    embed_in_chunks_with(
        client,
        text,
        Some(error),
        |client, segment| Box::pin(client.embed(segment)),
        |client| Box::pin(client.embed(EMBED_LIVENESS_PROBE)),
    )
    .await
}

/// Timeout-bounded form of [`embed_in_chunks_after_failure`]. Every segment
/// and every liveness probe receives the same configured request budget.
///
/// # Errors
///
/// Returns the original error when the timeout-bounded liveness probe fails,
/// or the first later provider error that is not content-attributed.
pub async fn embed_in_chunks_after_failure_with_timeout(
    client: &dyn EmbeddingClient,
    text: &str,
    error: LlmError,
    request_timeout: Duration,
) -> Result<Option<Vec<Vec<f32>>>, LlmError> {
    embed_in_chunks_with(
        client,
        text,
        Some(error),
        |client, segment| Box::pin(embed_with_timeout(client, segment, request_timeout)),
        |client| {
            Box::pin(embed_with_timeout(
                client,
                EMBED_LIVENESS_PROBE,
                request_timeout,
            ))
        },
    )
    .await
}

/// Queue both halves of a split onto a LIFO stack so the left one is taken
/// next, which is what keeps chunk vectors in text order.
fn push_left_half_first<'a>(pending: &mut Vec<&'a str>, left: &'a str, right: &'a str) {
    pending.push(right);
    pending.push(left);
}

async fn embed_in_chunks_with<'a, F, P>(
    client: &'a dyn EmbeddingClient,
    text: &'a str,
    initial_error: Option<LlmError>,
    mut embed: F,
    mut probe: P,
) -> Result<Option<Vec<Vec<f32>>>, LlmError>
where
    F: for<'b> FnMut(
        &'b dyn EmbeddingClient,
        &'b str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Vec<f32>, LlmError>> + Send + 'b>,
    >,
    P: for<'b> FnMut(
        &'b dyn EmbeddingClient,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Vec<f32>, LlmError>> + Send + 'b>,
    >,
{
    // Depth-first, left-to-right bisection keeps chunk vectors in text
    // order without recursion (async fns don't recurse).
    let mut pending: Vec<&str> = Vec::with_capacity(1);
    let mut vectors: Vec<Vec<f32>> = Vec::new();

    if let Some(error) = initial_error {
        if !failure_is_content_attributed(client, &error, &mut probe).await {
            return Err(error);
        }
        let Some((left, right)) = split_segment(text) else {
            return Ok(None);
        };
        push_left_half_first(&mut pending, left, right);
    } else {
        pending.push(text);
    }

    while let Some(segment) = pending.pop() {
        match embed(client, segment).await {
            Ok(vector) => vectors.push(vector),
            Err(error) if failure_is_content_attributed(client, &error, &mut probe).await => {
                let Some((left, right)) = split_segment(segment) else {
                    return Ok(None);
                };
                push_left_half_first(&mut pending, left, right);
            }
            Err(error) => return Err(error),
        }
    }
    Ok(Some(vectors))
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

async fn failure_is_content_attributed<P>(
    client: &dyn EmbeddingClient,
    error: &LlmError,
    probe: &mut P,
) -> bool
where
    P: for<'a> FnMut(
        &'a dyn EmbeddingClient,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Vec<f32>, LlmError>> + Send + 'a>,
    >,
{
    if matches!(error, LlmError::EmbedPermanent(_)) {
        return true;
    }
    probe(client).await.is_ok()
}

#[cfg(test)]
mod tests {
    use super::{
        CHUNKED_EMBED_MIN_BYTES, DEFAULT_EMBED_BATCH_SIZE, DEFAULT_EMBED_REQUEST_TIMEOUT,
        DEFAULT_EMBED_STALE_CLAIM_TIMEOUT, DEFAULT_EMBED_WORKER_INTERVAL, EmbeddingClient,
        EmbeddingRuntimePolicy, EmbeddingRuntimePolicyError, LlmError, MIN_EMBED_INPUT_CAP_CHARS,
        PROXIMA_EMBED_BATCH_SIZE, PROXIMA_EMBED_REQUEST_TIMEOUT_SECONDS,
        PROXIMA_EMBED_STALE_CLAIM_TIMEOUT_SECONDS, PROXIMA_EMBED_WORKER_INTERVAL_SECONDS,
        embed_in_chunks, embed_in_chunks_after_failure, embed_in_chunks_after_failure_with_timeout,
        embed_in_chunks_with_timeout, embed_many_with_timeout, embed_with_timeout,
    };
    use async_trait::async_trait;
    use std::sync::Mutex;
    use std::time::Duration;

    #[test]
    fn every_width_round_trips_and_only_supported_widths_bind() {
        for dim in super::EmbeddingDim::ALL {
            assert_eq!(super::EmbeddingDim::try_from(dim.width()), Ok(dim));
            assert_eq!(dim.is_halfvec_indexed(), dim.width() > 2000);
        }
        for width in [0, 4, 383, 1023, 1025, 4096] {
            assert_eq!(
                super::EmbeddingDim::try_from(width),
                Err(super::UnsupportedEmbeddingWidth { width })
            );
        }
    }

    #[test]
    fn binding_refuses_an_unsupported_width_and_records_the_space() {
        #[derive(Debug)]
        struct Width(usize);

        #[async_trait]
        impl EmbeddingClient for Width {
            async fn embed(&self, _text: &str) -> Result<Vec<f32>, LlmError> {
                Ok(vec![0.0; self.0])
            }

            fn model_id(&self) -> &'static str {
                "m"
            }

            fn dim(&self) -> usize {
                self.0
            }
        }

        let bound = super::BoundEmbeddingClient::bind(std::sync::Arc::new(Width(768)))
            .expect("768 is a lane");
        assert_eq!(
            bound.space(),
            &super::EmbeddingSpace::new("m", super::EmbeddingDim::D768)
        );
        assert_eq!(
            super::BoundEmbeddingClient::bind(std::sync::Arc::new(Width(1000))).unwrap_err(),
            super::UnsupportedEmbeddingWidth { width: 1000 }
        );
    }

    #[derive(Debug)]
    struct Named(&'static str, usize);

    #[async_trait]
    impl EmbeddingClient for Named {
        async fn embed(&self, _text: &str) -> Result<Vec<f32>, LlmError> {
            Ok(vec![0.0; self.1])
        }

        fn model_id(&self) -> &str {
            self.0
        }

        fn dim(&self) -> usize {
            self.1
        }
    }

    fn named(model: &'static str, dim: usize) -> super::BoundEmbeddingClient {
        super::BoundEmbeddingClient::bind(std::sync::Arc::new(Named(model, dim))).expect("a lane")
    }

    #[test]
    fn a_moving_route_writes_both_spaces_and_searches_current() {
        let old = named("old", 768);
        let new = named("new", 1024);
        let route = super::EmbeddingRoute::moving(Some(old.clone()), new.clone()).expect("a move");
        assert_eq!(
            route.write_spaces(),
            vec![old.space().clone(), new.space().clone()]
        );
        assert!(route.current_client().is_some_and(|c| c.same_client(&old)));
        assert!(route.next_client().is_some_and(|c| c.same_client(&new)));
        assert!(
            route
                .client_for(new.space())
                .is_some_and(|c| c.same_client(&new))
        );
        assert!(
            route
                .client_for(old.space())
                .is_some_and(|c| c.same_client(&old))
        );

        let first = super::EmbeddingRoute::moving(None, new.clone()).expect("a first route");
        assert!(first.current_client().is_none(), "nothing to search yet");
        assert_eq!(first.write_spaces(), vec![new.space().clone()]);
    }

    #[test]
    fn a_move_needs_a_new_space() {
        // Same model at a new width is a move; the same space twice is not.
        let rewidth = super::EmbeddingRoute::moving(Some(named("m", 1024)), named("m", 768));
        assert!(rewidth.is_ok());
        let err = super::EmbeddingRoute::moving(Some(named("m", 1024)), named("m", 1024))
            .expect_err("no move");
        assert!(err.to_string().contains("new embedding space"), "{err}");
    }

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

    #[derive(Debug)]
    struct AmbiguousCappedEmbedding {
        max_chars: usize,
        provider_down: bool,
        offered: Mutex<Vec<usize>>,
    }

    #[async_trait]
    impl EmbeddingClient for AmbiguousCappedEmbedding {
        async fn embed(&self, text: &str) -> Result<Vec<f32>, LlmError> {
            if self.provider_down {
                return Err(LlmError::Embed("provider unavailable".into()));
            }
            let chars = text.chars().count();
            self.offered
                .lock()
                .expect("no test holds this across a panic")
                .push(chars);
            if chars > self.max_chars {
                return Err(LlmError::Embed("400 EOF".into()));
            }
            Ok(vec![0.0; 4])
        }

        fn model_id(&self) -> &'static str {
            "ambiguous-capped"
        }

        fn dim(&self) -> usize {
            4
        }
    }

    #[tokio::test]
    async fn live_ambiguous_rejection_is_rescued_into_chunks() {
        let client = AmbiguousCappedEmbedding {
            max_chars: MIN_EMBED_INPUT_CAP_CHARS,
            provider_down: false,
            offered: Mutex::new(Vec::new()),
        };
        let text = "a".repeat(MIN_EMBED_INPUT_CAP_CHARS * 5);
        let initial = client.embed(&text).await.expect_err("over-limit input");
        let vectors = embed_in_chunks_after_failure(&client, &text, initial)
            .await
            .expect("live provider rejection should be content-attributed")
            .expect("over-limit input should be splittable");

        assert!(vectors.len() > 1, "long input must be chunked");
        assert!(
            client
                .offered
                .lock()
                .expect("no test holds this across a panic")
                .iter()
                .any(|chars| *chars <= MIN_EMBED_INPUT_CAP_CHARS),
            "rescue must submit provider-acceptable pieces"
        );
    }

    #[tokio::test]
    async fn failed_liveness_probe_keeps_ambiguous_rejection_retryable() {
        let client = AmbiguousCappedEmbedding {
            max_chars: MIN_EMBED_INPUT_CAP_CHARS,
            provider_down: true,
            offered: Mutex::new(Vec::new()),
        };
        let text = "a".repeat(MIN_EMBED_INPUT_CAP_CHARS * 5);
        let initial = client.embed(&text).await.expect_err("provider is down");
        let result = embed_in_chunks_after_failure_with_timeout(
            &client,
            &text,
            initial,
            Duration::from_secs(1),
        )
        .await;

        assert!(
            matches!(result, Err(LlmError::Embed(ref message)) if message == "provider unavailable"),
            "a failed probe must preserve a retryable error: {result:?}"
        );
        assert!(
            client
                .offered
                .lock()
                .expect("no test holds this across a panic")
                .is_empty(),
            "the provider-down mock must not reach chunk rescue"
        );
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
