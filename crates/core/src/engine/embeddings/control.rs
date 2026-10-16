//! Shared client admission for embedding calls, plus observed queue routes.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Duration;

use tokio::time::Instant;

use crate::OwnerId;
use crate::llm::{
    EmbeddingClient, EmbeddingConfigurationFault, EmbeddingRoute, EmbeddingRuntimePolicy,
    EmbeddingSpace, LlmError,
};

const MAX_BACKOFF: Duration = Duration::from_mins(15);

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
    fn refusal(&self, now: Instant) -> Option<LlmError> {
        match self {
            Self::Ready => None,
            Self::Paused(fault) => Some(LlmError::Configuration(*fault)),
            Self::Waiting(wait) => {
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
            Ok(_) | Err(LlmError::InputRefused(_)) => {
                *self = Self::Ready;
                return;
            }
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
            delay: backoff.max(retry_after.unwrap_or_default()),
            backoff,
            status,
        });
    }
}

#[derive(Debug, Default)]
struct ClientControl {
    dispatch: tokio::sync::Mutex<()>,
    state: Mutex<ClientState>,
}

impl ClientControl {
    async fn call<T>(
        &self,
        policy: EmbeddingRuntimePolicy,
        call: impl std::future::Future<Output = Result<T, LlmError>>,
    ) -> Result<T, LlmError> {
        let deadline = Instant::now() + policy.request_timeout();
        let timed_out = || {
            LlmError::retryable(format!(
                "request timed out after {} seconds",
                policy.request_timeout().as_secs()
            ))
        };
        let _dispatch = tokio::time::timeout_at(deadline, self.dispatch.lock())
            .await
            .map_err(|_| timed_out())?;
        if let Some(error) = lock(&self.state).refusal(Instant::now()) {
            return Err(error);
        }
        let result = tokio::time::timeout_at(deadline, call)
            .await
            .unwrap_or_else(|_| Err(timed_out()));
        lock(&self.state).settle(&result, policy.worker_interval(), Instant::now());
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
}

impl EmbeddingControl {
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
                    .any(|binding| lock(&binding.control.state).refusal(now).is_some())
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
                    .filter(|binding| lock(&binding.control.state).refusal(now).is_some())
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
            .filter_map(|binding| match *lock(&binding.control.state) {
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
}

#[async_trait::async_trait]
impl EmbeddingClient for ControlledClient {
    async fn embed(&self, text: &str) -> Result<Vec<f32>, LlmError> {
        self.control
            .call(self.policy, async {
                let vector = self.inner.embed(text).await?;
                self.validate(vector)
            })
            .await
    }

    async fn embed_many(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, LlmError> {
        self.control.call(self.policy, async {
            let vectors = self.inner.embed_many(texts).await?;
            if vectors.len() != texts.len() {
                return Err(LlmError::retryable(format!("embedding batch cardinality mismatch: sent {} texts but received {} vectors", texts.len(), vectors.len())));
            }
            vectors.into_iter().map(|vector| self.validate(vector)).collect()
        }).await
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
    use crate::llm::{BoundEmbeddingClient, EmbeddingInputRefusal};

    const PATIENCE: Duration = Duration::from_secs(30);

    #[derive(Debug, Clone, Copy)]
    enum Reply {
        Accept,
        Configuration(EmbeddingConfigurationFault),
        Busy(Option<Duration>),
        Input,
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
        tokio::time::advance(Duration::from_hours(24)).await;
        let remaining = Duration::MAX
            .checked_sub(Duration::from_hours(24))
            .expect("one day is below the maximum duration");
        assert!(
            matches!(route.current_client().expect("current").embed("still waiting").await, Err(LlmError::Retryable { retry_after: Some(delay), .. }) if delay == remaining)
        );
        assert_eq!(
            huge.calls.load(Ordering::SeqCst),
            1,
            "no deadline overflow or early retry"
        );
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

    #[derive(Debug, Default)]
    struct HeldConfigurationClient {
        entered: tokio::sync::Notify,
        release: tokio::sync::Notify,
        calls: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl EmbeddingClient for HeldConfigurationClient {
        async fn embed(&self, _text: &str) -> Result<Vec<f32>, LlmError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.entered.notify_one();
            self.release.notified().await;
            Err(LlmError::Configuration(
                EmbeddingConfigurationFault::Unauthorized,
            ))
        }
        fn model_id(&self) -> &'static str {
            "held"
        }
        fn dim(&self) -> usize {
            384
        }
    }

    #[tokio::test]
    async fn already_waiting_call_cannot_follow_a_published_pause() {
        let client = Arc::new(HeldConfigurationClient::default());
        let original = BoundEmbeddingClient::bind(Arc::clone(&client) as Arc<dyn EmbeddingClient>)
            .expect("supported");
        let control = EmbeddingControl::default();
        let a = control.observe(
            OwnerId::new(uuid::Uuid::now_v7()),
            EmbeddingRoute::current(original.clone()),
            EmbeddingRuntimePolicy::default(),
        );
        let b = control.observe(
            OwnerId::new(uuid::Uuid::now_v7()),
            EmbeddingRoute::current(original),
            EmbeddingRuntimePolicy::default(),
        );
        let first =
            tokio::spawn(async move { a.current_client().expect("current").embed("first").await });
        tokio::time::timeout(PATIENCE, client.entered.notified())
            .await
            .expect("first entered");
        let (started, receiver) = tokio::sync::oneshot::channel();
        let second = tokio::spawn(async move {
            started.send(()).expect("start marker");
            b.current_client()
                .expect("current")
                .embed_many(&["second".into()])
                .await
        });
        receiver.await.expect("second started");
        client.release.notify_one();
        assert!(matches!(
            tokio::time::timeout(PATIENCE, first)
                .await
                .expect("first completed")
                .expect("first task"),
            Err(LlmError::Configuration(_))
        ));
        assert!(matches!(
            tokio::time::timeout(PATIENCE, second)
                .await
                .expect("second completed")
                .expect("second task"),
            Err(LlmError::Configuration(_))
        ));
        assert_eq!(
            client.calls.load(Ordering::SeqCst),
            1,
            "gate publishes pause before admitting the next call"
        );
    }

    fn admission_timeout_policy() -> EmbeddingRuntimePolicy {
        EmbeddingRuntimePolicy::new(
            Duration::from_secs(30),
            1,
            Duration::from_secs(5),
            Duration::from_mins(1),
        )
        .expect("bounded test policy")
    }

    #[tokio::test(start_paused = true)]
    async fn queued_deadline_does_not_poll_provider_or_settle_in_flight_state() {
        let control = Arc::new(ClientControl::default());
        let dispatch = control.dispatch.lock().await;
        let calls = Arc::new(AtomicUsize::new(0));
        let policy = admission_timeout_policy();
        let (started, waiting) = tokio::sync::oneshot::channel();
        let queued_control = Arc::clone(&control);
        let queued_calls = Arc::clone(&calls);
        let entered = Instant::now();
        let queued = tokio::spawn(async move {
            started.send(()).expect("queued request marker");
            queued_control
                .call(policy, async {
                    queued_calls.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                })
                .await
        });
        tokio::time::timeout(PATIENCE, waiting)
            .await
            .expect("request entered")
            .expect("start marker");
        tokio::time::advance(Duration::from_secs(29)).await;
        assert!(!queued.is_finished(), "queue keeps its remaining budget");
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(matches!(*lock(&control.state), ClientState::Ready));
        tokio::time::advance(Duration::from_secs(1)).await;
        let error = tokio::time::timeout(PATIENCE, queued)
            .await
            .expect("queue timeout finishes")
            .expect("queued task")
            .expect_err("admission deadline");
        assert!(matches!(
            error,
            LlmError::Retryable {
                status: None,
                retry_after: None,
                ..
            }
        ));
        assert_eq!(
            Instant::now().saturating_duration_since(entered),
            policy.request_timeout()
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "expired work never reaches the provider"
        );
        assert!(
            matches!(*lock(&control.state), ClientState::Ready),
            "queue expiry cannot overwrite the in-flight request's state"
        );
        drop(dispatch);
        control
            .call(policy, async {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
            .await
            .expect("queue-only expiry did not back off the client");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn provider_receives_only_the_request_budget_remaining_after_admission() {
        let control = Arc::new(ClientControl::default());
        let dispatch = control.dispatch.lock().await;
        let calls = Arc::new(AtomicUsize::new(0));
        let policy = admission_timeout_policy();
        let (started, waiting) = tokio::sync::oneshot::channel();
        let (provider_entered, provider_started) = tokio::sync::oneshot::channel();
        let queued_control = Arc::clone(&control);
        let queued_calls = Arc::clone(&calls);
        let entered = Instant::now();
        let request = tokio::spawn(async move {
            started.send(()).expect("queued request marker");
            queued_control
                .call(policy, async {
                    queued_calls.fetch_add(1, Ordering::SeqCst);
                    provider_entered
                        .send(())
                        .expect("provider admission marker");
                    std::future::pending::<Result<(), LlmError>>().await
                })
                .await
        });
        tokio::time::timeout(PATIENCE, waiting)
            .await
            .expect("request entered")
            .expect("start marker");
        tokio::time::advance(Duration::from_secs(20)).await;
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "provider is not polled while queued"
        );
        assert!(matches!(*lock(&control.state), ClientState::Ready));
        drop(dispatch);
        tokio::time::timeout(PATIENCE, provider_started)
            .await
            .expect("provider admitted before deadline")
            .expect("provider marker");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        tokio::time::advance(Duration::from_secs(9)).await;
        assert!(!request.is_finished(), "provider still has one second");
        tokio::time::advance(Duration::from_secs(1)).await;
        let error = tokio::time::timeout(PATIENCE, request)
            .await
            .expect("shared deadline finishes")
            .expect("request task")
            .expect_err("provider deadline");
        assert!(matches!(
            error,
            LlmError::Retryable {
                status: None,
                retry_after: None,
                ..
            }
        ));
        assert_eq!(
            Instant::now().saturating_duration_since(entered),
            policy.request_timeout(),
            "admission must not restart the provider's timeout"
        );
        let state = lock(&control.state);
        let ClientState::Waiting(wait) = *state else {
            panic!("an admitted provider timeout must publish backoff: {state:?}");
        };
        assert_eq!(wait.delay, policy.worker_interval());
        assert_eq!(wait.since, Instant::now());
    }
}
