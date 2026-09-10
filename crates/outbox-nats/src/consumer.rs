//! The reference durable pull consumer.
//!
//! It is a REFERENCE, not an orchestrator: it binds a durable consumer,
//! parses the envelope, hands it to a [`DurableIntake`], and acknowledges
//! on exactly one condition — the intake durably recorded an outcome.
//! Accepting and rejecting are both outcomes; only a sink FAILURE leaves
//! the message unacknowledged, because that is the only case where a
//! redelivery can still change anything.

use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

use async_nats::jetstream;
use async_nats::jetstream::consumer::{AckPolicy, DeliverPolicy};
use bytes::Bytes;
use futures::StreamExt;
use tokio_util::sync::CancellationToken;

use crate::config::NatsConsumerConfig;

/// The `CloudEvents` 1.0 structured envelope, as parsed off the wire.
///
/// Deserialized rather than re-derived: the producer's bytes are the
/// contract, so unknown attributes are kept in `extensions` instead of
/// being an error — a consumer pinned to today's exact attribute set would
/// break on the first extension a later release adds.
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

/// A bound durable pull consumer over the publication stream.
#[derive(Debug, Clone)]
pub struct ReferenceConsumer {
    consumer: jetstream::consumer::Consumer<jetstream::consumer::pull::Config>,
    intake: Arc<dyn DurableIntake>,
    hook: Arc<dyn AckHook>,
    config: NatsConsumerConfig,
}

impl ReferenceConsumer {
    /// Connect and bind the durable consumer, creating it if absent.
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
        let client = crate::connect_client(&config.url, &config.auth, config.ack_wait)
            .await
            .map_err(|error| ConsumerError::Connect(error.to_string()))?;
        let context = jetstream::new(client);
        let stream = context
            .get_stream(&config.stream)
            .await
            .map_err(|error| ConsumerError::Broker(error.to_string()))?;
        let consumer = stream
            .get_or_create_consumer(
                &config.durable_name,
                jetstream::consumer::pull::Config {
                    durable_name: Some(config.durable_name.clone()),
                    // Explicit: the ACK is the durability handshake this
                    // consumer exists to demonstrate.
                    ack_policy: AckPolicy::Explicit,
                    ack_wait: config.ack_wait,
                    // Unlimited redelivery. A ceiling would silently drop
                    // an event whose sink was down longer than N attempts,
                    // which is precisely the loss the outbox prevents
                    // upstream.
                    max_deliver: -1,
                    deliver_policy: DeliverPolicy::All,
                    max_ack_pending: config.max_ack_pending,
                    filter_subject: config.subject_filter(),
                    ..jetstream::consumer::pull::Config::default()
                },
            )
            .await
            .map_err(|error| ConsumerError::Broker(error.to_string()))?;
        Ok(Self {
            consumer,
            intake,
            hook,
            config,
        })
    }

    /// The configuration this consumer was built with.
    #[must_use]
    pub const fn config(&self) -> &NatsConsumerConfig {
        &self.config
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
                                event_id = %event.id,
                                "acknowledgement dropped after a durable intake; \
                                 ack_wait will redeliver"
                            );
                        }
                    }
                }
                Err(error) => {
                    report.deferred += 1;
                    tracing::warn!(
                        event_id = %event.id,
                        error = %error,
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
        let batch = NonZeroU32::new(64).expect("64 is not zero");
        loop {
            if cancel.is_cancelled() {
                return;
            }
            match self.process_once(batch, Duration::from_secs(1)).await {
                Ok(report) if report.fetched > 0 => continue,
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(error = %error, "reference consumer pass failed");
                }
            }
            tokio::select! {
                () = cancel.cancelled() => return,
                () = tokio::time::sleep(Duration::from_millis(200)) => {}
            }
        }
    }
}

/// The stand-in envelope for bytes that are not `CloudEvents`.
///
/// It keeps an id — derived from the subject and the bytes — so the intake
/// still has a stable dedup key for a message it cannot understand.
fn malformed_envelope(subject: &str, raw: &Bytes, error: &serde_json::Error) -> CloudEventEnvelope {
    let digest = blake3::hash(raw);
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
