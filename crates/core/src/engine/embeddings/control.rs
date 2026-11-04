//! Shared client admission for embedding calls, plus observed queue routes.
//!
//! Admission never queues: a provider takes concurrent requests and queues or
//! refuses what exceeds its capacity. Admission only refuses calls a client
//! has been told to stop: a configuration fault stops every call, a transient
//! backoff stops background work while search queries still go out.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Duration;

use tokio::time::Instant;

use crate::OwnerId;
use crate::llm::{
    EmbeddingClient, EmbeddingConfigurationFault, EmbeddingInputRefusal, EmbeddingRoute,
    EmbeddingRuntimePolicy, EmbeddingSpace, LlmError,
};

const MAX_BACKOFF: Duration = Duration::from_mins(15);
/// The longest a provider's `Retry-After` hint holds a client back. A hint beyond a day (or one
/// that overflows a duration) would otherwise stall the route until the process restarts.
const MAX_RETRY_AFTER: Duration = Duration::from_hours(24);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn next_backoff(previous: Option<Duration>, base: Duration) -> Duration {
    previous
        .map_or(base, |delay| delay.saturating_mul(2))
        .min(MAX_BACKOFF.max(base))
}

#[derive(Debug, Clone, Copy)]
struct Wait {
    since: Instant,
    delay: Duration,
    backoff: Duration,
    status: Option<u16>,
}

impl Wait {
    fn remaining(self, now: Instant) -> Duration {
        self.delay
            .saturating_sub(now.saturating_duration_since(self.since))
    }
}

#[derive(Debug, Default)]
enum ClientState {
    #[default]
    Ready,
    Waiting(Wait),
    Paused(EmbeddingConfigurationFault),
}

impl ClientState {
    fn refusal(&self, admission: Admission, now: Instant) -> Option<LlmError> {
        match (self, admission) {
            (Self::Ready, _) | (Self::Waiting(_), Admission::Query) => None,
            (Self::Paused(fault), _) => Some(LlmError::Configuration(*fault)),
            (Self::Waiting(wait), Admission::Background) => {
                let remaining = wait.remaining(now);
                (!remaining.is_zero()).then(|| LlmError::Retryable {
                    status: wait.status,
                    retry_after: Some(remaining),
                    message: "embedding client is backed off".into(),
                })
            }
        }
    }

    fn settle<T>(&mut self, result: &Result<T, LlmError>, base: Duration, now: Instant) {
        let (status, retry_after) = match result {
            Ok(_)
            | Err(LlmError::InputRefused(
                EmbeddingInputRefusal::PayloadTooLarge
                | EmbeddingInputRefusal::UnprocessableContent,
            )) => {
                *self = Self::Ready;
                return;
            }
            Err(LlmError::InputRefused(EmbeddingInputRefusal::LocalCap { .. })) => return,
            Err(LlmError::Configuration(fault)) => {
                *self = Self::Paused(*fault);
                return;
            }
            Err(LlmError::Retryable {
                status,
                retry_after,
                ..
            }) => (*status, *retry_after),
            Err(LlmError::Internal(_)) => (None, None),
        };
        let previous = match self {
            Self::Waiting(wait) => Some(wait.backoff),
            Self::Ready | Self::Paused(_) => None,
        };
        let backoff = next_backoff(previous, base);
        *self = Self::Waiting(Wait {
            since: now,
            delay: backoff.max(retry_after.unwrap_or_default().min(MAX_RETRY_AFTER)),
            backoff,
            status,
        });
    }
}

/// What a call is, fixed by the operation the engine invokes: a search query
/// through [`EmbeddingClient::embed_query`], anything else is background work.
#[derive(Debug, Clone, Copy)]
enum Admission {
    /// Indexing and maintenance: owns the client's transient backoff.
    Background,
    /// A search query: one small request a caller waits for. It passes a
    /// backoff that background work published, and its own transient failure
    /// goes back to its caller without backing the client off.
    Query,
}

#[derive(Debug, Default)]
struct SharedState {
    value: ClientState,
    refusal_generation: u64,
}

impl SharedState {
    fn refusal(&self, admission: Admission, now: Instant) -> Option<LlmError> {
        self.value.refusal(admission, now)
    }

    /// Whether the client may accept more background work.
    fn blocked(&self, now: Instant) -> bool {
        self.refusal(Admission::Background, now).is_some()
    }

    fn settle<T>(
        &mut self,
        result: &Result<T, LlmError>,
        admission: Admission,
        generation: u64,
        base: Duration,
        now: Instant,
    ) {
        if matches!(admission, Admission::Query)
            && !matches!(result, Err(LlmError::Configuration(_)))
        {
            return;
        }
        // An in-flight success cannot clear a refusal published by a concurrent call.
        // Configuration pauses last until the original client binding is replaced.
        let refusal = matches!(
            result,
            Err(LlmError::Configuration(_) | LlmError::Retryable { .. } | LlmError::Internal(_))
        );
        if matches!(self.value, ClientState::Paused(_))
            || (!refusal && generation != self.refusal_generation)
        {
            return;
        }
        let previous_wait = match self.value {
            ClientState::Waiting(wait) if generation != self.refusal_generation => {
                wait.remaining(now)
            }
            _ => Duration::ZERO,
        };
        self.value.settle(result, base, now);
        if refusal {
            self.refusal_generation = self.refusal_generation.wrapping_add(1);
            if let ClientState::Waiting(wait) = &mut self.value {
                wait.delay = wait.delay.max(previous_wait);
            }
        }
    }
}

#[derive(Debug)]
struct ClientControl {
    trace_id: u64,
    state: Mutex<SharedState>,
}

impl Default for ClientControl {
    fn default() -> Self {
        static NEXT_CLIENT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        Self {
            trace_id: NEXT_CLIENT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            state: Mutex::default(),
        }
    }
}

impl ClientControl {
    async fn call<T>(
        &self,
        policy: EmbeddingRuntimePolicy,
        admission: Admission,
        call: impl std::future::Future<Output = Result<T, LlmError>>,
    ) -> Result<T, LlmError> {
        let generation = {
            let state = lock(&self.state);
            if let Some(error) = state.refusal(admission, Instant::now()) {
                return Err(error);
            }
            state.refusal_generation
        };
        let result = crate::stage_timing::timed(
            "embedding_provider",
            tokio::time::timeout(policy.request_timeout(), call),
        )
        .await
        .unwrap_or_else(|_| {
            Err(LlmError::retryable(format!(
                "request timed out after {} seconds",
                policy.request_timeout().as_secs()
            )))
        });
        lock(&self.state).settle(
            &result,
            admission,
            generation,
            policy.worker_interval(),
            Instant::now(),
        );
        result
    }
}

#[derive(Debug)]
struct ClientEntry {
    origin: Weak<dyn EmbeddingClient>,
    control: Arc<ClientControl>,
}

#[derive(Debug)]
struct Binding {
    space: EmbeddingSpace,
    control: Arc<ClientControl>,
}

#[derive(Debug, Default)]
struct Registry {
    clients: Vec<ClientEntry>,
    owners: HashMap<OwnerId, Vec<Binding>>,
    route_backoff: HashMap<OwnerId, Wait>,
}

/// Shared by manual drains, workers, queries, maintenance and health.
#[derive(Debug, Default)]
pub(crate) struct EmbeddingControl {
    registry: Mutex<Registry>,
    /// When a drain last took the stale-claim reclaim.
    last_reclaim: Mutex<Option<Instant>>,
}

impl EmbeddingControl {
    /// Whether this drain runs the stale-claim reclaim: the first drain, then
    /// one in every `every`, however many batches are claimed in between.
    pub(super) fn reclaim_due(&self, now: Instant, every: Duration) -> bool {
        let mut last = lock(&self.last_reclaim);
        if last.is_some_and(|at| now.saturating_duration_since(at) < every) {
            return false;
        }
        *last = Some(now);
        true
    }

    pub(super) fn observe(
        &self,
        owner: OwnerId,
        route: EmbeddingRoute,
        policy: EmbeddingRuntimePolicy,
    ) -> EmbeddingRoute {
        let mut registry = lock(&self.registry);
        registry
            .clients
            .retain(|entry| entry.origin.strong_count() > 0);
        registry.route_backoff.remove(&owner);
        let mut bindings = Vec::new();
        let route = route.wrap_clients(|bound, inner| {
            let origin = Arc::downgrade(bound.origin());
            let control = if let Some(entry) = registry
                .clients
                .iter()
                .find(|entry| entry.origin.ptr_eq(&origin))
            {
                Arc::clone(&entry.control)
            } else {
                let control = Arc::new(ClientControl::default());
                registry.clients.push(ClientEntry {
                    origin,
                    control: Arc::clone(&control),
                });
                control
            };
            let space = bound.space().clone();
            bindings.push(Binding {
                space: space.clone(),
                control: Arc::clone(&control),
            });
            Arc::new(ControlledClient {
                inner,
                space,
                control,
                policy,
            })
        });
        if bindings.is_empty() {
            registry.owners.remove(&owner);
        } else {
            registry.owners.insert(owner, bindings);
        }
        route
    }

    pub(super) fn owners_to_refresh(&self, now: Instant) -> Vec<OwnerId> {
        lock(&self.registry)
            .owners
            .iter()
            .filter_map(|(owner, bindings)| {
                bindings
                    .iter()
                    .any(|binding| lock(&binding.control.state).blocked(now))
                    .then_some(*owner)
            })
            .collect()
    }

    pub(super) fn blocked_pairs(&self, now: Instant) -> Vec<(OwnerId, EmbeddingSpace)> {
        lock(&self.registry)
            .owners
            .iter()
            .flat_map(|(owner, bindings)| {
                bindings
                    .iter()
                    .filter(|binding| lock(&binding.control.state).blocked(now))
                    .map(move |binding| (*owner, binding.space.clone()))
            })
            .collect()
    }

    pub(super) fn blocked_owners(&self, now: Instant) -> Vec<OwnerId> {
        lock(&self.registry)
            .route_backoff
            .iter()
            .filter_map(|(owner, wait)| (!wait.remaining(now).is_zero()).then_some(*owner))
            .collect()
    }

    pub(super) fn route_failed(&self, owner: OwnerId, base: Duration, now: Instant) -> Duration {
        let mut registry = lock(&self.registry);
        if let Some(wait) = registry.route_backoff.get(&owner) {
            let remaining = wait.remaining(now);
            if !remaining.is_zero() {
                return remaining;
            }
        }
        let backoff = next_backoff(
            registry.route_backoff.get(&owner).map(|wait| wait.backoff),
            base,
        );
        registry.route_backoff.insert(
            owner,
            Wait {
                since: now,
                delay: backoff,
                backoff,
                status: None,
            },
        );
        backoff
    }

    pub(super) fn configuration_faults(&self) -> Vec<EmbeddingConfigurationFault> {
        lock(&self.registry)
            .owners
            .values()
            .flatten()
            .filter_map(|binding| match lock(&binding.control.state).value {
                ClientState::Paused(fault) => Some(fault),
                ClientState::Ready | ClientState::Waiting(_) => None,
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }
}

#[derive(Debug)]
struct ControlledClient {
    inner: Arc<dyn EmbeddingClient>,
    space: EmbeddingSpace,
    control: Arc<ClientControl>,
    policy: EmbeddingRuntimePolicy,
}

impl ControlledClient {
    fn validate(&self, vector: Vec<f32>) -> Result<Vec<f32>, LlmError> {
        if vector.len() != self.space.dim().width() {
            return Err(LlmError::retryable(format!(
                "embedding width mismatch: expected {}, got {}",
                self.space.dim(),
                vector.len()
            )));
        }
        if vector.iter().any(|value| !value.is_finite()) {
            return Err(LlmError::retryable(
                "embedding contains a non-finite component",
            ));
        }
        Ok(vector)
    }

    async fn embed_one(
        &self,
        admission: Admission,
        text: &str,
        kind: &'static str,
    ) -> Result<Vec<f32>, LlmError> {
        use tracing::Instrument as _;
        self.control
            .call(self.policy, admission, async {
                let vector = match admission {
                    Admission::Background => self.inner.embed(text).await?,
                    Admission::Query => self.inner.embed_query(text).await?,
                };
                self.validate(vector)
            })
            .instrument(
                tracing::debug_span!(target: "proxiima::stage_timing", "embedding_call",
                call = %uuid::Uuid::now_v7(), client = self.control.trace_id, kind),
            )
            .await
    }
}

#[async_trait::async_trait]
impl EmbeddingClient for ControlledClient {
    async fn embed(&self, text: &str) -> Result<Vec<f32>, LlmError> {
        self.embed_one(Admission::Background, text, "single").await
    }

    async fn embed_query(&self, text: &str) -> Result<Vec<f32>, LlmError> {
        self.embed_one(Admission::Query, text, "query").await
    }

    async fn embed_many(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, LlmError> {
        use tracing::Instrument as _;
        self.control.call(self.policy, Admission::Background, async {
            let vectors = self.inner.embed_many(texts).await?;
            if vectors.len() != texts.len() {
                return Err(LlmError::retryable(format!("embedding batch cardinality mismatch: sent {} texts but received {} vectors", texts.len(), vectors.len())));
            }
            vectors.into_iter().map(|vector| self.validate(vector)).collect()
        }).instrument(tracing::debug_span!(target: "proxiima::stage_timing", "embedding_call",
            call = %uuid::Uuid::now_v7(), client = self.control.trace_id, kind = "batch")).await
    }

    fn model_id(&self) -> &str {
        self.space.model_id()
    }

    fn dim(&self) -> usize {
        self.space.dim().width()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::llm::BoundEmbeddingClient;

    const PATIENCE: Duration = Duration::from_secs(30);

    #[derive(Debug, Clone, Copy)]
    enum Reply {
        Accept,
        Configuration(EmbeddingConfigurationFault),
        Busy(Option<Duration>),
        Input,
        LocalCap,
        WrongWidth,
        WrongCount,
        NonFinite,
    }

    #[derive(Debug)]
    struct RecordingClient {
        model: &'static str,
        reply: Mutex<Reply>,
        calls: AtomicUsize,
    }

    impl RecordingClient {
        fn new(model: &'static str, reply: Reply) -> Arc<Self> {
            Arc::new(Self {
                model,
                reply: Mutex::new(reply),
                calls: AtomicUsize::new(0),
            })
        }

        fn respond(&self) -> Result<Vec<f32>, LlmError> {
            match *lock(&self.reply) {
                Reply::Accept | Reply::WrongCount => Ok(vec![1.0; 384]),
                Reply::Configuration(fault) => Err(LlmError::Configuration(fault)),
                Reply::Busy(retry_after) => Err(LlmError::Retryable {
                    status: Some(429),
                    retry_after,
                    message: "busy".into(),
                }),
                Reply::Input => Err(LlmError::InputRefused(
                    EmbeddingInputRefusal::UnprocessableContent,
                )),
                Reply::LocalCap => Err(LlmError::InputRefused(EmbeddingInputRefusal::LocalCap {
                    actual_chars: 2_048,
                    max_chars: 1_024,
                })),
                Reply::WrongWidth => Ok(vec![1.0; 383]),
                Reply::NonFinite => Ok(vec![f32::NAN; 384]),
            }
        }
    }

    #[async_trait::async_trait]
    impl EmbeddingClient for RecordingClient {
        async fn embed(&self, _text: &str) -> Result<Vec<f32>, LlmError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.respond()
        }
        async fn embed_many(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, LlmError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let vector = self.respond()?;
            let count = if matches!(*lock(&self.reply), Reply::WrongCount) {
                0
            } else {
                texts.len()
            };
            Ok(vec![vector; count])
        }
        fn model_id(&self) -> &'static str {
            self.model
        }
        fn dim(&self) -> usize {
            384
        }
    }

    fn bound(client: &Arc<RecordingClient>) -> BoundEmbeddingClient {
        BoundEmbeddingClient::bind(Arc::clone(client) as Arc<dyn EmbeddingClient>)
            .expect("supported width")
    }

    #[test]
    fn the_stale_claim_reclaim_runs_once_per_interval() {
        let control = EmbeddingControl::default();
        let every = Duration::from_mins(5);
        let start = Instant::now();
        assert!(
            control.reclaim_due(start, every),
            "the first drain reclaims"
        );
        for batch in 1..=100_u64 {
            let now = start + Duration::from_secs(batch);
            assert!(
                !control.reclaim_due(now, every),
                "batch {batch} of a backlog reclaims nothing"
            );
        }
        assert!(
            control.reclaim_due(start + every, every),
            "the interval is up"
        );
        assert!(!control.reclaim_due(start + every, every), "and taken once");
    }

    #[tokio::test]
    async fn configuration_pause_is_shared_and_new_original_client_retires_only_its_binding() {
        for fault in [
            EmbeddingConfigurationFault::BadRequest,
            EmbeddingConfigurationFault::Unauthorized,
            EmbeddingConfigurationFault::Forbidden,
            EmbeddingConfigurationFault::NotFound,
        ] {
            let control = EmbeddingControl::default();
            let old = RecordingClient::new("shared", Reply::Configuration(fault));
            let owner_a = OwnerId::new(uuid::Uuid::now_v7());
            let owner_b = OwnerId::new(uuid::Uuid::now_v7());
            let policy = EmbeddingRuntimePolicy::default();
            let a = control.observe(owner_a, EmbeddingRoute::current(bound(&old)), policy);
            let b = control.observe(owner_b, EmbeddingRoute::current(bound(&old)), policy);
            assert!(
                matches!(a.current_client().expect("current").embed("one").await, Err(LlmError::Configuration(actual)) if actual == fault)
            );
            assert!(
                matches!(b.current_client().expect("current").embed("two").await, Err(LlmError::Configuration(actual)) if actual == fault)
            );
            assert_eq!(
                old.calls.load(Ordering::SeqCst),
                1,
                "shared original pauses all bindings"
            );
            assert_eq!(control.configuration_faults(), [fault]);

            *lock(&old.reply) = Reply::Accept;
            let same = control.observe(owner_a, EmbeddingRoute::current(bound(&old)), policy);
            assert!(matches!(
                same.current_client()
                    .expect("current")
                    .embed("same Arc")
                    .await,
                Err(LlmError::Configuration(_))
            ));
            assert_eq!(
                old.calls.load(Ordering::SeqCst),
                1,
                "same-Arc mutation cannot reset a pause"
            );

            let new = RecordingClient::new("shared", Reply::Accept);
            let replacement =
                control.observe(owner_a, EmbeddingRoute::current(bound(&new)), policy);
            replacement
                .current_client()
                .expect("current")
                .embed("new Arc")
                .await
                .expect("same-space new client is ready");
            assert_eq!(new.calls.load(Ordering::SeqCst), 1);
            assert_eq!(
                control.configuration_faults(),
                [fault],
                "other owner still references old original"
            );
            control.observe(owner_b, EmbeddingRoute::none(), policy);
            assert!(
                control.configuration_faults().is_empty(),
                "no active observed old binding"
            );
            assert!(control.blocked_pairs(Instant::now()).is_empty());

            let returned = control.observe(owner_b, EmbeddingRoute::current(bound(&old)), policy);
            assert!(matches!(
                returned
                    .current_client()
                    .expect("current")
                    .embed("old returns")
                    .await,
                Err(LlmError::Configuration(_))
            ));
            assert_eq!(
                old.calls.load(Ordering::SeqCst),
                1,
                "weak pause memo never probes a surviving original"
            );
            assert_eq!(control.configuration_faults(), [fault]);
        }
    }

    #[tokio::test]
    async fn retired_pause_memo_does_not_keep_the_original_client_alive() {
        let control = EmbeddingControl::default();
        let old = RecordingClient::new(
            "old",
            Reply::Configuration(EmbeddingConfigurationFault::Unauthorized),
        );
        let weak = Arc::downgrade(&old);
        let owner = OwnerId::new(uuid::Uuid::now_v7());
        let route = control.observe(
            owner,
            EmbeddingRoute::current(bound(&old)),
            EmbeddingRuntimePolicy::default(),
        );
        route
            .current_client()
            .expect("current")
            .embed("pause")
            .await
            .expect_err("configuration");
        control.observe(
            owner,
            EmbeddingRoute::none(),
            EmbeddingRuntimePolicy::default(),
        );
        drop(route);
        drop(old);
        assert!(
            weak.upgrade().is_none(),
            "memo holds only weak original identity"
        );
        control.observe(
            owner,
            EmbeddingRoute::current(bound(&RecordingClient::new("new", Reply::Accept))),
            EmbeddingRuntimePolicy::default(),
        );
        assert_eq!(
            lock(&control.registry).clients.len(),
            1,
            "expired original memo is pruned"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn retry_after_outlives_local_cap_without_suppressed_calls_extending_it() {
        let control = EmbeddingControl::default();
        let client = RecordingClient::new("busy", Reply::Busy(Some(Duration::from_hours(1))));
        let owner = OwnerId::new(uuid::Uuid::now_v7());
        let route = control.observe(
            owner,
            EmbeddingRoute::current(bound(&client)),
            EmbeddingRuntimePolicy::default(),
        );
        let client_bound = route.current_client().expect("current");
        assert!(
            matches!(client_bound.embed("first").await, Err(LlmError::Retryable { status: Some(429), retry_after: Some(delay), .. }) if delay == Duration::from_hours(1))
        );
        *lock(&client.reply) = Reply::Accept;
        tokio::time::advance(Duration::from_secs(3599)).await;
        assert!(
            matches!(client_bound.embed("too early").await, Err(LlmError::Retryable { retry_after: Some(delay), .. }) if delay == Duration::from_secs(1))
        );
        assert_eq!(client.calls.load(Ordering::SeqCst), 1);
        tokio::time::advance(Duration::from_secs(1)).await;
        client_bound.embed("at bound").await.expect("recovered");
        assert_eq!(
            client.calls.load(Ordering::SeqCst),
            2,
            "suppressed call never moved retry bound"
        );
        assert!(control.blocked_pairs(Instant::now()).is_empty());

        let huge = RecordingClient::new("huge", Reply::Busy(Some(Duration::MAX)));
        let route = control.observe(
            owner,
            EmbeddingRoute::current(bound(&huge)),
            EmbeddingRuntimePolicy::default(),
        );
        route
            .current_client()
            .expect("current")
            .embed("first")
            .await
            .expect_err("huge retry delay");
        tokio::time::advance(MAX_RETRY_AFTER.saturating_sub(Duration::from_secs(1))).await;
        assert!(
            matches!(route.current_client().expect("current").embed("still waiting").await, Err(LlmError::Retryable { retry_after: Some(delay), .. }) if delay == Duration::from_secs(1))
        );
        assert_eq!(
            huge.calls.load(Ordering::SeqCst),
            1,
            "no deadline overflow or early retry"
        );
        tokio::time::advance(Duration::from_secs(1)).await;
        route
            .current_client()
            .expect("current")
            .embed("at the cap")
            .await
            .expect_err("the provider still answers busy");
        assert_eq!(
            huge.calls.load(Ordering::SeqCst),
            2,
            "a hint beyond the cap holds the client back for the cap only"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_local_input_cap_preserves_accumulated_provider_backoff() {
        let control = EmbeddingControl::default();
        let client = RecordingClient::new("local-cap", Reply::Busy(None));
        let policy = EmbeddingRuntimePolicy::default();
        let route = control.observe(
            OwnerId::new(uuid::Uuid::now_v7()),
            EmbeddingRoute::current(bound(&client)),
            policy,
        );
        let bound = route.current_client().expect("current");
        bound
            .embed("first provider failure")
            .await
            .expect_err("busy");
        tokio::time::advance(policy.worker_interval()).await;

        *lock(&client.reply) = Reply::LocalCap;
        assert!(matches!(
            bound.embed("locally over the cap").await,
            Err(LlmError::InputRefused(
                EmbeddingInputRefusal::LocalCap { .. }
            ))
        ));
        *lock(&client.reply) = Reply::Busy(None);
        bound
            .embed("second provider failure")
            .await
            .expect_err("busy");
        let expected = policy.worker_interval() * 2;
        assert!(
            matches!(bound.embed("backed off").await,
                Err(LlmError::Retryable { retry_after: Some(delay), .. }) if delay == expected),
            "a local refusal did not contact the provider or heal its backoff"
        );
        assert_eq!(client.calls.load(Ordering::SeqCst), 3);
        *lock(&client.reply) = Reply::Accept;
        tokio::time::advance(expected).await;
        bound
            .embed("provider recovered")
            .await
            .expect("retry at bound");
        assert_eq!(client.calls.load(Ordering::SeqCst), 4);
    }

    #[tokio::test]
    async fn paused_current_does_not_exclude_same_owner_next_or_another_original_in_same_space() {
        let control = EmbeddingControl::default();
        let owner = OwnerId::new(uuid::Uuid::now_v7());
        let other = OwnerId::new(uuid::Uuid::now_v7());
        let current = RecordingClient::new(
            "current",
            Reply::Configuration(EmbeddingConfigurationFault::Forbidden),
        );
        let next = RecordingClient::new("next", Reply::Accept);
        let route = control.observe(
            owner,
            EmbeddingRoute::moving(Some(bound(&current)), bound(&next)).expect("different spaces"),
            EmbeddingRuntimePolicy::default(),
        );
        let other_route = control.observe(
            other,
            EmbeddingRoute::current(bound(&RecordingClient::new("current", Reply::Accept))),
            EmbeddingRuntimePolicy::default(),
        );
        route
            .current_client()
            .expect("current")
            .embed("pause")
            .await
            .expect_err("configuration");
        route
            .next_client()
            .expect("next")
            .embed("healthy next")
            .await
            .expect("next is independent");
        other_route
            .current_client()
            .expect("current")
            .embed("separate original")
            .await
            .expect("same space is independent");
        assert_eq!(
            control.blocked_pairs(Instant::now()),
            [(owner, bound(&current).space().clone())]
        );
        assert!(
            control.blocked_owners(Instant::now()).is_empty(),
            "client faults never skip the whole owner"
        );
        assert_eq!(
            control.configuration_faults(),
            [EmbeddingConfigurationFault::Forbidden],
            "unrelated successes do not clear pause"
        );
    }

    #[tokio::test]
    async fn malformed_success_is_retryable_and_input_refusal_does_not_pause() {
        for reply in [Reply::WrongWidth, Reply::WrongCount, Reply::NonFinite] {
            let control = EmbeddingControl::default();
            let client = RecordingClient::new("malformed", reply);
            let owner = OwnerId::new(uuid::Uuid::now_v7());
            let route = control.observe(
                owner,
                EmbeddingRoute::current(bound(&client)),
                EmbeddingRuntimePolicy::default(),
            );
            let client_bound = route.current_client().expect("current");
            assert!(matches!(
                client_bound.embed_many(&["a".into(), "b".into()]).await,
                Err(LlmError::Retryable { status: None, .. })
            ));
            assert!(matches!(
                client_bound.embed("suppressed").await,
                Err(LlmError::Retryable { .. })
            ));
            assert_eq!(client.calls.load(Ordering::SeqCst), 1);
            assert!(control.configuration_faults().is_empty());
            assert_eq!(control.blocked_pairs(Instant::now()).len(), 1);
        }
        let control = EmbeddingControl::default();
        let client = RecordingClient::new("input", Reply::Input);
        let route = control.observe(
            OwnerId::new(uuid::Uuid::now_v7()),
            EmbeddingRoute::current(bound(&client)),
            EmbeddingRuntimePolicy::default(),
        );
        assert!(matches!(
            route
                .current_client()
                .expect("current")
                .embed("refused")
                .await,
            Err(LlmError::InputRefused(_))
        ));
        assert!(control.configuration_faults().is_empty());
        assert!(
            control.blocked_pairs(Instant::now()).is_empty(),
            "input rescue can proceed immediately"
        );
    }

    #[tokio::test]
    async fn observing_a_wrapped_binding_replaces_the_engine_layer() {
        let control = EmbeddingControl::default();
        let owner = OwnerId::new(uuid::Uuid::now_v7());
        let client = RecordingClient::new("reobserved", Reply::Accept);
        let route = control.observe(
            owner,
            EmbeddingRoute::current(bound(&client)),
            EmbeddingRuntimePolicy::default(),
        );
        let route = control.observe(owner, route, EmbeddingRuntimePolicy::default());
        tokio::time::timeout(
            PATIENCE,
            route.current_client().expect("current").embed("one"),
        )
        .await
        .expect("re-observation must not nest the same gate")
        .expect("valid response");
        assert_eq!(client.calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn concurrent_refusals_keep_the_longest_wait_and_a_configuration_pause() {
        let start = Instant::now();
        let base = Duration::from_secs(5);
        for late in [
            LlmError::Retryable {
                status: Some(429),
                retry_after: Some(Duration::from_secs(1)),
                message: "busy".into(),
            },
            LlmError::Internal("unavailable".into()),
        ] {
            let mut state = SharedState::default();
            state.settle(
                &Err::<(), _>(LlmError::Retryable {
                    status: Some(429),
                    retry_after: Some(Duration::from_mins(1)),
                    message: "busy".into(),
                }),
                Admission::Background,
                0,
                base,
                start,
            );
            state.settle(
                &Err::<(), _>(late),
                Admission::Background,
                0,
                base,
                start + Duration::from_secs(1),
            );
            assert!(
                matches!(state.refusal(Admission::Background, start + Duration::from_secs(59)), Some(LlmError::Retryable { retry_after: Some(remaining), .. }) if remaining == Duration::from_secs(1))
            );
            assert!(
                state
                    .refusal(Admission::Background, start + Duration::from_mins(1))
                    .is_none()
            );
        }
        let mut state = SharedState::default();
        state.settle(
            &Err::<(), _>(LlmError::Configuration(
                EmbeddingConfigurationFault::Unauthorized,
            )),
            Admission::Background,
            0,
            base,
            start,
        );
        state.settle(
            &Err::<(), _>(LlmError::retryable("late refusal")),
            Admission::Background,
            0,
            base,
            start,
        );
        assert!(matches!(
            state.refusal(Admission::Background, start + MAX_RETRY_AFTER),
            Some(LlmError::Configuration(
                EmbeddingConfigurationFault::Unauthorized
            ))
        ));
    }

    fn poll_once<F: std::future::Future>(
        future: std::pin::Pin<&mut F>,
    ) -> std::task::Poll<F::Output> {
        future.poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
    }

    fn busy(retry_after: Duration) -> LlmError {
        LlmError::Retryable {
            status: Some(429),
            retry_after: Some(retry_after),
            message: "busy".into(),
        }
    }

    #[derive(Debug)]
    struct HeldWritesClient {
        entered: AtomicUsize,
        release: tokio::sync::Semaphore,
        queries: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl EmbeddingClient for HeldWritesClient {
        async fn embed(&self, _text: &str) -> Result<Vec<f32>, LlmError> {
            self.entered.fetch_add(1, Ordering::SeqCst);
            self.release.acquire().await.unwrap().forget();
            Ok(vec![1.0; 384])
        }
        async fn embed_query(&self, _text: &str) -> Result<Vec<f32>, LlmError> {
            self.queries.fetch_add(1, Ordering::SeqCst);
            Ok(vec![1.0; 384])
        }
        fn model_id(&self) -> &'static str {
            "held-writes"
        }
        fn dim(&self) -> usize {
            384
        }
    }

    #[tokio::test]
    async fn calls_reach_the_provider_concurrently_and_a_query_waits_for_none() {
        const WRITES: usize = 6;
        let client = Arc::new(HeldWritesClient {
            entered: AtomicUsize::new(0),
            release: tokio::sync::Semaphore::new(0),
            queries: AtomicUsize::new(0),
        });
        let control = EmbeddingControl::default();
        let route = control.observe(
            OwnerId::new(uuid::Uuid::now_v7()),
            EmbeddingRoute::current(
                BoundEmbeddingClient::bind(Arc::clone(&client) as Arc<dyn EmbeddingClient>)
                    .unwrap(),
            ),
            EmbeddingRuntimePolicy::default(),
        );
        let bound = route.current_client().unwrap();
        let mut writes: Vec<_> = (0..WRITES)
            .map(|_| Box::pin(bound.embed("write")))
            .collect();
        for write in &mut writes {
            assert!(poll_once(write.as_mut()).is_pending());
        }
        assert_eq!(
            client.entered.load(Ordering::SeqCst),
            WRITES,
            "every write is at the provider; none waits in the engine"
        );
        tokio::time::timeout(
            PATIENCE,
            bound.embed_query("query", crate::llm::QueryTask::DEFAULT),
        )
        .await
        .expect("the query is not queued behind held writes")
        .expect("query vector");
        assert_eq!(
            client.queries.load(Ordering::SeqCst),
            1,
            "a search goes out as a query"
        );
        client.release.add_permits(WRITES);
        for write in writes {
            tokio::time::timeout(PATIENCE, write)
                .await
                .unwrap()
                .unwrap();
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_query_passes_a_background_backoff_without_moving_it_and_stops_at_a_pause() {
        let control = ClientControl::default();
        let policy = EmbeddingRuntimePolicy::default();
        let calls = AtomicUsize::new(0);
        let provider = |reply: Result<(), LlmError>| {
            let calls = &calls;
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                reply
            }
        };
        let backed_off = |result: Result<(), LlmError>, expected: Duration| matches!(result, Err(LlmError::Retryable { retry_after: Some(remaining), .. }) if remaining == expected);
        control
            .call(
                policy,
                Admission::Background,
                provider(Err(busy(Duration::from_mins(1)))),
            )
            .await
            .expect_err("busy");

        control
            .call(policy, Admission::Query, provider(Ok(())))
            .await
            .expect("a query passes the backoff");
        assert!(
            backed_off(
                control
                    .call(policy, Admission::Background, provider(Ok(())))
                    .await,
                Duration::from_mins(1)
            ),
            "a query's success does not heal the background backoff"
        );
        control
            .call(
                policy,
                Admission::Query,
                provider(Err(busy(Duration::from_hours(1)))),
            )
            .await
            .expect_err("the query's own refusal goes to its caller");
        assert!(
            backed_off(
                control
                    .call(policy, Admission::Background, provider(Ok(())))
                    .await,
                Duration::from_mins(1)
            ),
            "a query's refusal does not extend the background backoff"
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            3,
            "refused calls never reach the provider"
        );

        control
            .call(
                policy,
                Admission::Query,
                provider(Err(LlmError::Configuration(
                    EmbeddingConfigurationFault::Unauthorized,
                ))),
            )
            .await
            .expect_err("configuration");
        for admission in [Admission::Background, Admission::Query] {
            assert!(matches!(
                control.call(policy, admission, provider(Ok(()))).await,
                Err(LlmError::Configuration(
                    EmbeddingConfigurationFault::Unauthorized
                ))
            ));
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            4,
            "a query's configuration fault pauses every call"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_late_success_cannot_clear_a_concurrent_refusal() {
        for refusal in [
            busy(Duration::from_mins(1)),
            LlmError::Configuration(EmbeddingConfigurationFault::Unauthorized),
        ] {
            let pause = matches!(refusal, LlmError::Configuration(_));
            let control = ClientControl::default();
            let policy = EmbeddingRuntimePolicy::default();
            let (release, held) = tokio::sync::oneshot::channel();
            let mut in_flight = Box::pin(control.call(policy, Admission::Background, async {
                held.await.unwrap();
                Ok(())
            }));
            assert!(poll_once(in_flight.as_mut()).is_pending());
            control
                .call(policy, Admission::Background, async {
                    Err::<(), _>(refusal)
                })
                .await
                .expect_err("refused");
            release.send(()).unwrap();
            in_flight.await.expect("the earlier call succeeds");
            let result = control
                .call(policy, Admission::Background, async { Ok(()) })
                .await;
            if pause {
                assert!(matches!(result, Err(LlmError::Configuration(_))));
            } else {
                assert!(
                    matches!(result, Err(LlmError::Retryable { retry_after: Some(remaining), .. }) if remaining == Duration::from_mins(1))
                );
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_provider_call_past_the_request_deadline_backs_the_client_off() {
        let control = ClientControl::default();
        let policy = EmbeddingRuntimePolicy::default();
        let started = Instant::now();
        let error = control
            .call(
                policy,
                Admission::Background,
                std::future::pending::<Result<(), LlmError>>(),
            )
            .await
            .expect_err("deadline");
        assert!(matches!(
            error,
            LlmError::Retryable {
                status: None,
                retry_after: None,
                ..
            }
        ));
        assert_eq!(
            Instant::now().saturating_duration_since(started),
            policy.request_timeout()
        );
        let state = lock(&control.state);
        let ClientState::Waiting(wait) = state.value else {
            panic!("a provider timeout must publish backoff: {state:?}");
        };
        assert_eq!(wait.delay, policy.worker_interval());
    }
}
