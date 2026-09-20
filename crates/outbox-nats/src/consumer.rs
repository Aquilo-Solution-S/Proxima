//! The reference durable pull consumer.
//!
//! It is a REFERENCE, not an orchestrator: it binds a durable consumer,
//! parses the envelope, hands it to a [`DurableIntake`], and acknowledges
//! on exactly one condition — the intake durably recorded an outcome.
//! Accepting and rejecting are both outcomes; only a sink FAILURE leaves
//! the message unacknowledged, because that is the only case where a
//! redelivery can still change anything.

use std::collections::BTreeMap;
use std::future::Future;
use std::num::NonZeroU32;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_nats::jetstream;
use bytes::Bytes;
use futures::StreamExt;
use tokio_util::sync::CancellationToken;

use crate::config::NatsConsumerConfig;

/// The `CloudEvents` 1.0 structured envelope, as parsed off the wire.
///
/// Deserialized rather than re-derived: the producer's bytes are the
/// contract. An attribute this struct does not name lands in
/// [`Self::extensions`] rather than being refused — a consumer pinned to
/// today's exact attribute set would break on the first extension a later
/// release adds. A sink verifying a producer signature still reads
/// [`ReceivedEvent::raw`], the delivered bytes, and not a re-serialization
/// of this view.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct CloudEventEnvelope {
    pub specversion: String,
    pub id: String,
    pub source: String,
    #[serde(rename = "type")]
    pub event_type: String,
    #[serde(default)]
    pub datacontenttype: Option<String>,
    #[serde(default)]
    pub dataschema: Option<String>,
    #[serde(default)]
    pub time: Option<String>,
    #[serde(default)]
    pub proximaowner: Option<String>,
    #[serde(default)]
    pub proximamodel: Option<String>,
    /// Every context attribute the fields above do not name, in name order.
    /// Host-bound extension attributes arrive here; so does anything a
    /// later producer release adds. Values are whatever the `CloudEvents`
    /// JSON format allowed the producer to write.
    #[serde(flatten)]
    pub extensions: BTreeMap<String, serde_json::Value>,
    pub data: serde_json::Value,
}

/// One delivery, with the broker facts a durable intake needs to be
/// idempotent.
#[derive(Debug, Clone)]
pub struct ReceivedEvent {
    /// The `CloudEvents` `id` — the dedup key, and the same value the
    /// producer put in `Nats-Msg-Id`.
    pub id: String,
    pub subject: String,
    pub stream_sequence: u64,
    /// How many times the broker has delivered this message, this delivery
    /// included. `> 1` means a previous delivery was not acknowledged.
    pub delivered_count: u64,
    /// The bytes as delivered. A consumer verifying a producer signature
    /// checks these, not a re-serialization of [`Self::envelope`].
    pub raw: Bytes,
    pub envelope: CloudEventEnvelope,
}

/// What an intake did with one event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Intake {
    Accepted,
    Rejected { reason: String },
}

/// A sink that could not decide. Distinct from
/// [`Intake::Rejected`] on purpose: rejection is a decision that was
/// durably recorded, this is a decision that was not made.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("durable intake failed: {message}")]
pub struct IntakeError {
    pub message: String,
    /// How long the broker should wait before redelivering. `None` uses
    /// the consumer's own `ack_wait`.
    pub retry_after: Option<Duration>,
}

impl IntakeError {
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retry_after: None,
        }
    }

    #[must_use]
    pub const fn retry_after(mut self, delay: Duration) -> Self {
        self.retry_after = Some(delay);
        self
    }
}

/// The one thing a consumer of this stream must implement.
///
/// The contract is the acknowledgement contract: `accept` MUST have made
/// its outcome durable before it returns, because the ACK that follows is
/// the consumer telling the broker it may forget the message. Both
/// outcomes count — a rejection nobody recorded is indistinguishable from
/// an event that never arrived.
///
/// Deduplicate on [`ReceivedEvent::id`]. Redelivery is not an error case,
/// it is the normal cost of at-least-once.
#[async_trait::async_trait]
pub trait DurableIntake: Send + Sync + std::fmt::Debug {
    /// Durably record an outcome for `event` and return which one.
    ///
    /// # Errors
    ///
    /// [`IntakeError`] when the outcome could NOT be made durable; the
    /// consumer then leaves the message unacknowledged for redelivery.
    async fn accept(&self, event: &ReceivedEvent) -> Result<Intake, IntakeError>;
}

/// What the consumer does after the intake returned and before the ACK.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AckAction {
    /// Normal path: send the acknowledgement.
    Continue,
    /// Behave as though the ACK was lost in flight: the intake ran, the
    /// broker never heard about it, and `ack_wait` will redeliver.
    DropAck,
}

/// Seam between "the intake committed" and "the broker was told".
///
/// Same reason as the publisher's [`crate::PublishHook`]: a lost ACK is a
/// window inside one `await`, and it is the window the whole idempotency
/// requirement exists for. Always compiled, defaulted to a no-op.
#[async_trait::async_trait]
pub trait AckHook: Send + Sync + std::fmt::Debug {
    async fn before_ack(&self, event: &ReceivedEvent, outcome: &Intake) -> AckAction;
}

/// The shipped hook: acknowledge everything the intake decided.
#[derive(Debug, Clone, Copy, Default)]
pub struct AckAlwaysHook;

#[async_trait::async_trait]
impl AckHook for AckAlwaysHook {
    async fn before_ack(&self, _event: &ReceivedEvent, _outcome: &Intake) -> AckAction {
        AckAction::Continue
    }
}

/// What one `process_once` pass did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ConsumeReport {
    pub fetched: usize,
    pub accepted: usize,
    pub rejected: usize,
    /// Messages left unacknowledged for redelivery: the intake failed.
    pub deferred: usize,
    /// Messages that were not `CloudEvents` at all. Durably rejected
    /// through the intake and then acknowledged — a poison message that
    /// blocked the consumer forever would be a worse outcome than one that
    /// was recorded and set aside.
    pub malformed: usize,
    /// Acknowledgements this pass deliberately did not send (hook).
    pub unacked: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsumerTaskState {
    Starting,
    Running,
    Stopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsumerConnectionState {
    NotObserved,
    Pending,
    Connected,
    Disconnected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsumerPassState {
    NotObserved,
    Clean,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConsumerHealth {
    pub task: ConsumerTaskState,
    pub connection: ConsumerConnectionState,
    pub pass: ConsumerPassState,
}

impl ConsumerHealth {
    #[must_use]
    pub const fn is_ready(self) -> bool {
        matches!(self.task, ConsumerTaskState::Running)
            && matches!(self.connection, ConsumerConnectionState::Connected)
            && matches!(self.pass, ConsumerPassState::Clean)
    }
}

#[derive(Clone)]
pub struct ConsumerHealthReader {
    inner: Arc<Mutex<ConsumerHealthInner>>,
}

impl std::fmt::Debug for ConsumerHealthReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("ConsumerHealthReader")
            .field(&self.snapshot())
            .finish()
    }
}

struct ConsumerHealthInner {
    task: ConsumerTaskState,
    connection: ConsumerConnectionState,
    pass: ConsumerPassState,
    client: Option<async_nats::Client>,
}

impl Default for ConsumerHealthInner {
    fn default() -> Self {
        Self {
            task: ConsumerTaskState::Starting,
            connection: ConsumerConnectionState::NotObserved,
            pass: ConsumerPassState::NotObserved,
            client: None,
        }
    }
}

impl ConsumerHealthReader {
    #[must_use]
    pub fn snapshot(&self) -> ConsumerHealth {
        let inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let connection = inner.client.as_ref().map_or(inner.connection, |client| {
            match client.connection_state() {
                async_nats::connection::State::Connected => ConsumerConnectionState::Connected,
                async_nats::connection::State::Pending => ConsumerConnectionState::Pending,
                async_nats::connection::State::Disconnected => {
                    ConsumerConnectionState::Disconnected
                }
            }
        });
        ConsumerHealth {
            task: inner.task,
            connection,
            pass: inner.pass,
        }
    }
}

struct ConsumerTaskGuard {
    inner: Arc<Mutex<ConsumerHealthInner>>,
}

impl ConsumerTaskGuard {
    fn update(&self, update: impl FnOnce(&mut ConsumerHealthInner)) {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        update(&mut inner);
    }

    fn set_task(&self, state: ConsumerTaskState) {
        self.update(|inner| inner.task = state);
    }

    fn attach(&self, client: async_nats::Client) {
        self.update(|inner| {
            inner.connection = ConsumerConnectionState::Connected;
            inner.client = Some(client);
        });
    }

    fn observe_pass(&self, result: &Result<ConsumeReport, ConsumerError>) {
        let state = match result {
            Ok(report) if report.deferred == 0 && report.unacked == 0 => ConsumerPassState::Clean,
            Ok(_) | Err(_) => ConsumerPassState::Failed,
        };
        self.update(|inner| inner.pass = state);
    }
}

impl Drop for ConsumerTaskGuard {
    fn drop(&mut self) {
        self.update(|inner| {
            inner.task = ConsumerTaskState::Stopped;
            inner.connection = ConsumerConnectionState::NotObserved;
            inner.client = None;
        });
    }
}

/// A bound durable pull consumer over the publication stream.
#[derive(Debug, Clone)]
pub struct ReferenceConsumer {
    consumer: jetstream::consumer::Consumer<jetstream::consumer::pull::Config>,
    intake: Arc<dyn DurableIntake>,
    hook: Arc<dyn AckHook>,
    config: NatsConsumerConfig,
    client: async_nats::Client,
}

impl ReferenceConsumer {
    /// Connect and bind an existing durable consumer provisioned by the
    /// deployment. This path never creates or updates broker topology.
    ///
    /// # Errors
    ///
    /// [`ConsumerError::Connect`] when the broker is unreachable and
    /// [`ConsumerError::Broker`] when the stream or consumer cannot be
    /// bound.
    pub async fn connect(
        config: NatsConsumerConfig,
        intake: Arc<dyn DurableIntake>,
    ) -> Result<Self, ConsumerError> {
        Self::connect_with_hook(config, intake, Arc::new(AckAlwaysHook)).await
    }

    /// [`Self::connect`] with an installed [`AckHook`].
    ///
    /// # Errors
    ///
    /// As [`Self::connect`].
    pub async fn connect_with_hook(
        config: NatsConsumerConfig,
        intake: Arc<dyn DurableIntake>,
        hook: Arc<dyn AckHook>,
    ) -> Result<Self, ConsumerError> {
        let client = crate::connect_client(
            &config.url,
            &config.auth,
            config.inbox_prefix.as_ref(),
            config.request_timeout,
        )
        .await
        .map_err(|error| ConsumerError::Connect(error.to_string()))?;
        let context = jetstream::new(client.clone());
        let consumer: jetstream::consumer::Consumer<jetstream::consumer::pull::Config> = context
            .get_consumer_from_stream(&config.durable_name, &config.stream)
            .await
            .map_err(|error| ConsumerError::Broker(error.to_string()))?;
        Ok(Self {
            consumer,
            intake,
            hook,
            config,
            client,
        })
    }

    /// The configuration this consumer was built with.
    #[must_use]
    pub const fn config(&self) -> &NatsConsumerConfig {
        &self.config
    }

    /// Split the consumer into a read-only health reader and its owned run
    /// future. The future uses the same fetch/ACK loop as [`Self::run`].
    /// Dropping it, including before its first poll, marks its health
    /// terminal and releases the reader's observation of the NATS client.
    #[must_use = "poll the future and retain the returned health reader"]
    pub fn into_observed_parts(
        self,
        cancel: CancellationToken,
    ) -> (
        ConsumerHealthReader,
        impl Future<Output = ()> + Send + 'static,
    ) {
        let (reader, guard) = Self::health_observation();
        let client = self.client.clone();
        let future = async move {
            let guard = guard;
            guard.set_task(ConsumerTaskState::Running);
            guard.attach(client);
            self.run_observed(cancel, &guard).await;
        };
        (reader, future)
    }

    fn health_observation() -> (ConsumerHealthReader, ConsumerTaskGuard) {
        let inner = Arc::new(Mutex::new(ConsumerHealthInner::default()));
        (
            ConsumerHealthReader {
                inner: inner.clone(),
            },
            ConsumerTaskGuard { inner },
        )
    }

    /// Fetch up to `max` messages, waiting at most `wait` for the first.
    ///
    /// # Errors
    ///
    /// [`ConsumerError::Broker`] when the fetch itself fails. A message
    /// that fails to parse is NOT an error: it is a durable rejection.
    pub async fn process_once(
        &self,
        max: NonZeroU32,
        wait: Duration,
    ) -> Result<ConsumeReport, ConsumerError> {
        let mut batch = self
            .consumer
            .fetch()
            .max_messages(usize::try_from(max.get()).unwrap_or(usize::MAX))
            .expires(wait)
            .messages()
            .await
            .map_err(|error| ConsumerError::Broker(error.to_string()))?;
        let mut report = ConsumeReport::default();
        while let Some(message) = batch.next().await {
            let message = message.map_err(|error| ConsumerError::Broker(error.to_string()))?;
            report.fetched += 1;
            let info = message
                .info()
                .map_err(|error| ConsumerError::Broker(error.to_string()))?;
            let subject = message.subject.to_string();
            let raw = message.payload.clone();
            let (envelope, malformed) = match serde_json::from_slice::<CloudEventEnvelope>(&raw) {
                Ok(envelope) => (envelope, false),
                Err(error) => (malformed_envelope(&subject, &raw, &error), true),
            };
            if malformed {
                report.malformed += 1;
            }
            let event = ReceivedEvent {
                id: envelope.id.clone(),
                subject,
                stream_sequence: info.stream_sequence,
                delivered_count: info.delivered.unsigned_abs(),
                raw,
                envelope,
            };
            // A malformed message goes through the intake like any other:
            // an unparseable delivery that was silently ACKed would be one
            // nobody can account for afterwards.
            match self.intake.accept(&event).await {
                Ok(outcome) => {
                    match &outcome {
                        Intake::Accepted => report.accepted += 1,
                        Intake::Rejected { .. } => report.rejected += 1,
                    }
                    match self.hook.before_ack(&event, &outcome).await {
                        AckAction::Continue => {
                            message
                                .ack()
                                .await
                                .map_err(|error| ConsumerError::Ack(error.to_string()))?;
                        }
                        AckAction::DropAck => {
                            report.unacked += 1;
                            tracing::warn!(
                                failure_category = ?ConsumerFailureCategory::Ack,
                                unacked = 1,
                                "acknowledgement dropped after a durable intake; \
                                 ack_wait will redeliver"
                            );
                        }
                    }
                }
                Err(error) => {
                    report.deferred += 1;
                    tracing::warn!(
                        failure_category = ?ConsumerFailureCategory::Intake,
                        deferred = 1,
                        "durable intake failed; leaving the message unacknowledged"
                    );
                    // NAK rather than silence, so the redelivery is
                    // immediate (or after the sink's own backoff) instead
                    // of costing a full `ack_wait`.
                    message
                        .ack_with(jetstream::AckKind::Nak(error.retry_after))
                        .await
                        .map_err(|error| ConsumerError::Ack(error.to_string()))?;
                }
            }
        }
        Ok(report)
    }

    /// Consume until cancelled; broker errors are logged and retried.
    ///
    /// # Panics
    ///
    /// Never: the only unwrap is a non-zero batch-size literal.
    pub async fn run(self, cancel: CancellationToken) {
        let (_, future) = self.into_observed_parts(cancel);
        future.await;
    }

    async fn run_observed(&self, cancel: CancellationToken, guard: &ConsumerTaskGuard) {
        let batch = NonZeroU32::new(64).expect("64 is not zero");
        loop {
            if cancel.is_cancelled() {
                return;
            }
            match self
                .process_once_observed(batch, Duration::from_secs(1), guard)
                .await
            {
                Ok(report) if report.fetched > 0 => continue,
                Ok(_) | Err(_) => {}
            }
            tokio::select! {
                () = cancel.cancelled() => return,
                () = tokio::time::sleep(Duration::from_millis(200)) => {}
            }
        }
    }

    async fn process_once_observed(
        &self,
        max: NonZeroU32,
        wait: Duration,
        guard: &ConsumerTaskGuard,
    ) -> Result<ConsumeReport, ConsumerError> {
        let result = self.process_once(max, wait).await;
        guard.observe_pass(&result);
        match &result {
            Ok(report) if report.deferred > 0 || report.unacked > 0 => {
                tracing::warn!(
                    failure_category = ?ConsumerFailureCategory::PartialPass,
                    deferred = report.deferred,
                    unacked = report.unacked,
                    fetched = report.fetched,
                    accepted = report.accepted,
                    rejected = report.rejected,
                    "reference consumer pass completed with deferred outcomes"
                );
            }
            Err(error) => log_consumer_failure(error),
            Ok(_) => {}
        }
        result
    }
}

/// The stand-in envelope for bytes that are not `CloudEvents`.
///
/// It keeps an id — derived from the subject and the bytes — so the intake
/// still has a stable dedup key for a message it cannot understand.
fn malformed_envelope(subject: &str, raw: &Bytes, error: &serde_json::Error) -> CloudEventEnvelope {
    let mut hasher = blake3::Hasher::new();
    hasher.update(
        &u64::try_from(subject.len())
            .expect("subject length must fit in the digest framing")
            .to_be_bytes(),
    );
    hasher.update(subject.as_bytes());
    hasher.update(raw);
    let digest = hasher.finalize();
    CloudEventEnvelope {
        specversion: String::new(),
        id: format!("malformed:{}", &digest.to_hex()[..32]),
        source: subject.to_owned(),
        event_type: "proxima.malformed".to_owned(),
        datacontenttype: None,
        dataschema: None,
        time: None,
        proximaowner: None,
        proximamodel: None,
        extensions: BTreeMap::new(),
        data: serde_json::Value::String(error.to_string()),
    }
}

/// Why a consume pass stopped.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConsumerError {
    #[error("connecting to the NATS broker failed: {0}")]
    Connect(String),
    #[error("JetStream refused the request: {0}")]
    Broker(String),
    #[error("acknowledging a message failed: {0}")]
    Ack(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConsumerFailureCategory {
    Connect,
    Broker,
    Ack,
    Intake,
    PartialPass,
}

impl ConsumerError {
    const fn category(&self) -> ConsumerFailureCategory {
        match self {
            Self::Connect(_) => ConsumerFailureCategory::Connect,
            Self::Broker(_) => ConsumerFailureCategory::Broker,
            Self::Ack(_) => ConsumerFailureCategory::Ack,
        }
    }
}

fn log_consumer_failure(error: &ConsumerError) {
    tracing::warn!(
        failure_category = ?error.category(),
        "reference consumer pass failed; retrying"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tracing::field::{Field, Visit};
    use tracing::{Event, Subscriber};
    use tracing_subscriber::Layer;
    use tracing_subscriber::layer::Context;
    use tracing_subscriber::prelude::*;

    const ERROR_MARKER: &str = "SYNTHETIC_CONSUMER_SECRET";
    const ID_MARKER: &str = "SYNTHETIC_CONSUMER_ID_MARKER";
    const PAYLOAD_MARKER: &str = "SYNTHETIC_CONSUMER_PAYLOAD_MARKER";

    #[derive(Clone)]
    struct EventBuffer(Arc<Mutex<Vec<String>>>);

    impl<S> Layer<S> for EventBuffer
    where
        S: Subscriber,
    {
        fn on_event(&self, event: &Event<'_>, _context: Context<'_, S>) {
            let mut fields = EventFields::default();
            event.record(&mut fields);
            self.0.lock().expect("event buffer lock").push(fields.0);
        }
    }

    #[derive(Default)]
    struct EventFields(String);

    impl Visit for EventFields {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            use std::fmt::Write as _;
            write!(&mut self.0, "{}={value:?} ", field.name()).expect("string write");
        }
    }

    async fn with_dispatch<F: Future>(dispatch: tracing::Dispatch, future: F) -> F::Output {
        let mut future = Box::pin(future);
        std::future::poll_fn(|cx| {
            tracing::dispatcher::with_default(&dispatch, || future.as_mut().poll(cx))
        })
        .await
    }

    fn test_nats_url(test: &str) -> Option<String> {
        match std::env::var("PROXIMA_TEST_NATS_URL") {
            Ok(url) if !url.trim().is_empty() => Some(url),
            _ => {
                assert_ne!(
                    std::env::var("CI").as_deref(),
                    Ok("true"),
                    "PROXIMA_TEST_NATS_URL required under CI=true (test {test})"
                );
                eprintln!("skipping {test}: PROXIMA_TEST_NATS_URL is unset");
                None
            }
        }
    }

    async fn test_admin_client(url: &str) -> async_nats::Client {
        let options = async_nats::ConnectOptions::new();
        let options = match (
            std::env::var("PROXIMA_TEST_NATS_ADMIN_USER"),
            std::env::var("PROXIMA_TEST_NATS_ADMIN_PASSWORD"),
        ) {
            (Ok(user), Ok(password)) => options.user_and_password(user, password),
            (Err(_), Err(_)) => options,
            _ => panic!("admin NATS credentials must be set together"),
        };
        options
            .connect(url)
            .await
            .expect("admin NATS connection opens")
    }

    #[derive(Debug)]
    struct ScenarioIntake {
        fail_marker_once: AtomicBool,
    }

    #[async_trait::async_trait]
    impl DurableIntake for ScenarioIntake {
        async fn accept(&self, event: &ReceivedEvent) -> Result<Intake, IntakeError> {
            if event.id == ID_MARKER && self.fail_marker_once.swap(false, Ordering::AcqRel) {
                return Err(IntakeError::new(ERROR_MARKER).retry_after(Duration::from_millis(20)));
            }
            if event.id == "rejected-event" || event.envelope.event_type == "proxima.malformed" {
                return Ok(Intake::Rejected {
                    reason: "synthetic durable rejection".to_owned(),
                });
            }
            Ok(Intake::Accepted)
        }
    }

    #[derive(Debug, Default)]
    struct DropFirstAck(AtomicBool);

    #[async_trait::async_trait]
    impl AckHook for DropFirstAck {
        async fn before_ack(&self, _event: &ReceivedEvent, _outcome: &Intake) -> AckAction {
            if self.0.swap(true, Ordering::AcqRel) {
                AckAction::Continue
            } else {
                AckAction::DropAck
            }
        }
    }

    #[test]
    fn a_captured_envelope_round_trips_through_the_consumer_view() {
        let bytes = br#"{"specversion":"1.0","id":"F:abc","source":"urn:proxima:test",
            "type":"probe/listenable-v1","datacontenttype":"application/json",
            "dataschema":"proxima://schema/probe/listenable-v1/1","time":"2026-01-01T00:00:00Z",
            "proximaowner":"personal:1","proximamodel":"m","data":{"note":"hi"}}"#;
        let envelope: CloudEventEnvelope = serde_json::from_slice(bytes).expect("parses");
        assert_eq!(envelope.id, "F:abc");
        assert_eq!(envelope.event_type, "probe/listenable-v1");
        assert_eq!(envelope.proximamodel.as_deref(), Some("m"));
        assert_eq!(envelope.data["note"], serde_json::json!("hi"));
        assert!(envelope.extensions.is_empty());
    }

    #[test]
    fn host_bound_extension_attributes_survive_the_parse() {
        let bytes = br#"{"specversion":"1.0","id":"F:abc","source":"urn:proxima:test",
            "type":"probe/listenable-v1","proximaowner":"personal:1","proximamodel":"m",
            "stepid":"step-7","runid":"run-3","attempt":2,"replay":false,"data":{"note":"hi"}}"#;
        let envelope: CloudEventEnvelope = serde_json::from_slice(bytes).expect("parses");
        assert_eq!(
            envelope
                .extensions
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec!["attempt", "replay", "runid", "stepid"]
        );
        assert_eq!(envelope.extensions["runid"], serde_json::json!("run-3"));
        assert_eq!(envelope.extensions["attempt"], serde_json::json!(2));
        assert_eq!(envelope.extensions["replay"], serde_json::json!(false));
        // The substrate's own attributes stay on their named fields rather
        // than doubling up in the map.
        assert!(!envelope.extensions.contains_key("proximamodel"));
        assert!(!envelope.extensions.contains_key("data"));
    }

    #[test]
    fn an_absent_model_attribute_is_not_a_parse_failure() {
        let bytes = br#"{"specversion":"1.0","id":"F:abc","source":"urn:x:y","type":"t",
            "data":{}}"#;
        let envelope: CloudEventEnvelope = serde_json::from_slice(bytes).expect("parses");
        assert_eq!(envelope.proximamodel, None);
        assert_eq!(envelope.proximaowner, None);
    }

    #[test]
    fn a_malformed_message_still_gets_a_stable_dedup_key() {
        let raw = Bytes::from_static(b"not json at all");
        let error = serde_json::from_slice::<CloudEventEnvelope>(&raw).expect_err("not json");
        let first = malformed_envelope("proxima.fact.x", &raw, &error);
        let error = serde_json::from_slice::<CloudEventEnvelope>(&raw).expect_err("not json");
        let second = malformed_envelope("proxima.fact.x", &raw, &error);
        assert_eq!(first.id, second.id);
        assert!(first.id.starts_with("malformed:"));
    }

    #[test]
    fn malformed_dedup_keys_include_the_subject() {
        let raw = Bytes::from_static(b"not json at all");
        let error = serde_json::from_slice::<CloudEventEnvelope>(&raw).expect_err("not json");
        let first = malformed_envelope("proxima.fact.x", &raw, &error);
        let error = serde_json::from_slice::<CloudEventEnvelope>(&raw).expect_err("not json");
        let second = malformed_envelope("proxima.fact.y", &raw, &error);
        assert_ne!(first.id, second.id);
    }

    #[test]
    fn malformed_dedup_keys_frame_subject_before_bytes() {
        let first_raw = Bytes::from_static(b"bc");
        let second_raw = Bytes::from_static(b"c");
        let first_error =
            serde_json::from_slice::<CloudEventEnvelope>(&first_raw).expect_err("not json");
        let second_error =
            serde_json::from_slice::<CloudEventEnvelope>(&second_raw).expect_err("not json");
        let first = malformed_envelope("a", &first_raw, &first_error);
        let second = malformed_envelope("ab", &second_raw, &second_error);
        assert_ne!(first.id, second.id);
    }

    #[test]
    fn broker_failure_log_keeps_only_its_fixed_category() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::registry().with(EventBuffer(events.clone()));
        let dispatch = tracing::Dispatch::new(subscriber);
        tracing::dispatcher::with_default(&dispatch, || {
            log_consumer_failure(&ConsumerError::Broker(ERROR_MARKER.to_owned()));
        });
        let output = events.lock().expect("event buffer lock").join("\n");
        assert!(
            output.contains("Broker"),
            "expected fixed category in {output}"
        );
        assert!(output.contains("reference consumer pass failed"));
        assert!(
            !output.contains(ERROR_MARKER),
            "raw broker error leaked: {output}"
        );
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn observed_pass_classifies_partial_and_acknowledged_outcomes() {
        use futures::FutureExt;

        let Some(url) = test_nats_url("observed_pass_classifies_partial_and_acknowledged_outcomes")
        else {
            return;
        };
        let admin = test_admin_client(&url).await;
        let context = jetstream::new(admin);
        let token = uuid::Uuid::now_v7().simple().to_string();
        let stream_name = format!("CONSUMER_HEALTH_{token}").to_ascii_uppercase();
        let subject_prefix = format!("consumerhealth.{token}");
        let durable_name = format!("consumer-health-{token}");
        context
            .create_stream(jetstream::stream::Config {
                name: stream_name.clone(),
                subjects: vec![format!("{subject_prefix}.>")],
                storage: jetstream::stream::StorageType::File,
                retention: jetstream::stream::RetentionPolicy::Limits,
                discard: jetstream::stream::DiscardPolicy::New,
                max_age: Duration::ZERO,
                max_bytes: 1024 * 1024,
                max_message_size: -1,
                duplicate_window: Duration::from_secs(30),
                ..jetstream::stream::Config::default()
            })
            .await
            .expect("the unique consumer health stream is created");

        let events = Arc::new(Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::registry().with(EventBuffer(events.clone()));
        let dispatch = tracing::Dispatch::new(subscriber);
        let outcome = std::panic::AssertUnwindSafe(with_dispatch(dispatch, async {
            let stream = context
                .get_stream(&stream_name)
                .await
                .expect("consumer health stream is readable");
            stream
                .create_consumer(jetstream::consumer::pull::Config {
                    durable_name: Some(durable_name.clone()),
                    ack_policy: jetstream::consumer::AckPolicy::Explicit,
                    ack_wait: Duration::from_secs(1),
                    max_deliver: -1,
                    deliver_policy: jetstream::consumer::DeliverPolicy::All,
                    max_ack_pending: 16,
                    filter_subject: format!("{subject_prefix}.>"),
                    max_batch: 3,
                    ..jetstream::consumer::pull::Config::default()
                })
                .await
                .expect("the durable health consumer is created");

            let mut config = NatsConsumerConfig::new(url.clone());
            if let (Ok(user), Ok(password)) = (
                std::env::var("PROXIMA_TEST_NATS_ADMIN_USER"),
                std::env::var("PROXIMA_TEST_NATS_ADMIN_PASSWORD"),
            ) {
                config.auth = crate::NatsAuth::UserPassword { user, password };
            }
            config.stream.clone_from(&stream_name);
            config.durable_name.clone_from(&durable_name);
            let intake = Arc::new(ScenarioIntake {
                fail_marker_once: AtomicBool::new(true),
            });
            let consumer_config = config.clone();
            let consumer =
                ReferenceConsumer::connect_with_hook(config, intake, Arc::new(AckAlwaysHook))
                    .await
                    .expect("the health consumer binds the durable");
            let (reader, guard) = ReferenceConsumer::health_observation();
            guard.set_task(ConsumerTaskState::Running);
            guard.attach(consumer.client.clone());
            assert_eq!(reader.snapshot().pass, ConsumerPassState::NotObserved);

            publish_test_event(
                &context,
                &subject_prefix,
                "accepted-before-partial",
                "ordinary payload",
            )
            .await;
            publish_test_event(&context, &subject_prefix, ID_MARKER, PAYLOAD_MARKER).await;
            let partial = consumer
                .process_once_observed(
                    NonZeroU32::new(2).expect("nonzero batch"),
                    Duration::from_secs(2),
                    &guard,
                )
                .await
                .expect("partial intake outcomes are a report");
            assert_eq!(partial.fetched, 2, "{partial:?}");
            assert_eq!(partial.accepted, 1, "{partial:?}");
            assert_eq!(partial.deferred, 1, "{partial:?}");
            assert_eq!(reader.snapshot().pass, ConsumerPassState::Failed);

            publish_test_event(
                &context,
                &subject_prefix,
                "rejected-event",
                "durable rejection",
            )
            .await;
            context
                .publish(
                    format!("{subject_prefix}.malformed"),
                    bytes::Bytes::from_static(b"not a CloudEvents envelope"),
                )
                .await
                .expect("malformed test message is published")
                .await
                .expect("malformed test message receives PubAck");
            let clean = consumer
                .process_once_observed(
                    NonZeroU32::new(3).expect("nonzero batch"),
                    Duration::from_secs(2),
                    &guard,
                )
                .await
                .expect("the retried and rejected events are durably handled");
            assert_eq!(clean.deferred, 0, "{clean:?}");
            assert_eq!(clean.unacked, 0, "{clean:?}");
            assert_eq!(clean.rejected, 2, "{clean:?}");
            assert_eq!(clean.malformed, 1, "{clean:?}");
            assert_eq!(reader.snapshot().pass, ConsumerPassState::Clean);

            drop(guard);
            assert_eq!(reader.snapshot().task, ConsumerTaskState::Stopped);
            assert_eq!(
                reader.snapshot().connection,
                ConsumerConnectionState::NotObserved
            );
            assert!(!format!("{reader:?}").contains(ERROR_MARKER));
            assert!(!format!("{reader:?}").contains(ID_MARKER));
            drop(consumer);

            let ack_consumer = ReferenceConsumer::connect_with_hook(
                consumer_config,
                Arc::new(ScenarioIntake {
                    fail_marker_once: AtomicBool::new(false),
                }),
                Arc::new(DropFirstAck::default()),
            )
            .await
            .expect("the ACK health consumer binds the durable");
            let (ack_reader, ack_guard) = ReferenceConsumer::health_observation();
            ack_guard.set_task(ConsumerTaskState::Running);
            ack_guard.attach(ack_consumer.client.clone());
            publish_test_event(&context, &subject_prefix, "drop-ack-event", "drop ack").await;
            let dropped_ack = ack_consumer
                .process_once_observed(
                    NonZeroU32::new(1).expect("nonzero batch"),
                    Duration::from_secs(2),
                    &ack_guard,
                )
                .await
                .expect("a deliberately dropped ACK is a report");
            assert_eq!(dropped_ack.unacked, 1, "{dropped_ack:?}");
            assert_eq!(ack_reader.snapshot().pass, ConsumerPassState::Failed);
            tokio::time::sleep(Duration::from_millis(1200)).await;
            let redelivery = ack_consumer
                .process_once_observed(
                    NonZeroU32::new(1).expect("nonzero batch"),
                    Duration::from_secs(2),
                    &ack_guard,
                )
                .await
                .expect("the redelivery is accepted and acknowledged");
            assert_eq!(redelivery.accepted, 1, "{redelivery:?}");
            assert_eq!(redelivery.unacked, 0, "{redelivery:?}");
            assert_eq!(ack_reader.snapshot().pass, ConsumerPassState::Clean);

            let broker_error = ack_consumer
                .process_once_observed(
                    NonZeroU32::new(4).expect("nonzero batch"),
                    Duration::from_secs(2),
                    &ack_guard,
                )
                .await;
            assert!(matches!(broker_error, Err(ConsumerError::Broker(_))));
            assert_eq!(ack_reader.snapshot().pass, ConsumerPassState::Failed);

            let stream = context
                .get_stream(&stream_name)
                .await
                .expect("health stream remains available");
            stream
                .update_consumer(jetstream::consumer::pull::Config {
                    durable_name: Some(durable_name.clone()),
                    ack_policy: jetstream::consumer::AckPolicy::Explicit,
                    ack_wait: Duration::from_secs(1),
                    max_deliver: -1,
                    deliver_policy: jetstream::consumer::DeliverPolicy::All,
                    max_ack_pending: 16,
                    filter_subject: format!("{subject_prefix}.>"),
                    max_batch: 0,
                    ..jetstream::consumer::pull::Config::default()
                })
                .await
                .expect("the server-side batch limit is restored");
            let recovered = ack_consumer
                .process_once_observed(
                    NonZeroU32::new(4).expect("nonzero batch"),
                    Duration::from_millis(100),
                    &ack_guard,
                )
                .await
                .expect("the same production pass recovers after the limit is removed");
            // The broker rejection does not deliver a batch. The one earlier
            // deliberately unacked delivery can nevertheless be redelivered
            // on this recovery pull; if it is, it must complete and be ACKed.
            assert!(recovered.fetched <= 1, "{recovered:?}");
            assert_eq!(recovered.accepted, recovered.fetched, "{recovered:?}");
            assert_eq!(recovered.rejected, 0, "{recovered:?}");
            assert_eq!(recovered.deferred, 0, "{recovered:?}");
            assert_eq!(recovered.malformed, 0, "{recovered:?}");
            assert_eq!(recovered.unacked, 0, "{recovered:?}");
            assert_eq!(ack_reader.snapshot().pass, ConsumerPassState::Clean);

            drop(ack_guard);
            assert_eq!(ack_reader.snapshot().task, ConsumerTaskState::Stopped);
            assert_eq!(
                ack_reader.snapshot().connection,
                ConsumerConnectionState::NotObserved
            );
            drop(ack_consumer);
        }))
        .catch_unwind()
        .await;
        let delete_result = context.delete_stream(&stream_name).await;
        if let Err(payload) = outcome {
            let _ = delete_result;
            std::panic::resume_unwind(payload);
        }
        delete_result.expect("the test-owned stream is deleted");

        let output = events.lock().expect("event buffer lock").join("\n");
        assert!(
            output.contains("Intake"),
            "intake category missing from {output}"
        );
        assert!(
            output.contains("PartialPass"),
            "partial category missing from {output}"
        );
        assert!(output.contains("Ack"), "ack category missing from {output}");
        assert!(
            output.contains("Broker"),
            "broker category missing from {output}"
        );
        for marker in [ERROR_MARKER, ID_MARKER, PAYLOAD_MARKER] {
            assert!(
                !output.contains(marker),
                "consumer log leaked {marker}: {output}"
            );
        }
    }

    async fn publish_test_event(
        context: &jetstream::Context,
        subject_prefix: &str,
        id: &str,
        note: &str,
    ) {
        let payload = serde_json::json!({
            "specversion": "1.0",
            "id": id,
            "source": "urn:proxima:consumer-health-test",
            "type": "test/consumer-health-v1",
            "data": { "note": note }
        });
        context
            .publish(
                format!("{subject_prefix}.event"),
                serde_json::to_vec(&payload)
                    .expect("the synthetic test envelope serializes")
                    .into(),
            )
            .await
            .expect("test event is published")
            .await
            .expect("test event receives PubAck");
    }
}
