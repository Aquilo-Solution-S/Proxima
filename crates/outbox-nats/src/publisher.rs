//! The shared `JetStream` publisher: claim → publish → acknowledge.
//!
//! Every step is recoverable because none of them destroys anything. A
//! claim is a lease, a publish is idempotent inside the broker's dedup
//! window under `Nats-Msg-Id`, and an acknowledgement is fenced on the
//! claim token. The failure the design is built around is the one that
//! cannot be observed from here — a `PubAck` that was sent and never
//! arrived — and its answer is the same as every other failure's:
//! republish the same bytes under the same id.

use std::sync::Arc;
use std::time::Duration;

use async_nats::jetstream;
use async_nats::jetstream::context::PublishErrorKind;
use bytes::Bytes;
use proxima_core::storage_ports::publication::{
    AckOutcome, BrokerReceipt, ClaimedPublication, PublicationOutboxPort, ReleaseOutcome,
};
use tokio_util::sync::CancellationToken;

use crate::config::{NatsPublisherConfig, subject_for};

/// Header carrying the broker's dedup key: the `CloudEvents` `id`.
pub const HEADER_MSG_ID: &str = "Nats-Msg-Id";
/// Header naming the envelope encoding.
pub const HEADER_CONTENT_TYPE: &str = "Content-Type";
/// Header naming the registered schema and its version, so a consumer can
/// route without parsing the body.
pub const HEADER_SCHEMA: &str = "Proxima-Schema";
/// The `CloudEvents` structured-mode content type.
pub const CONTENT_TYPE_CLOUDEVENTS: &str = "application/cloudevents+json";

/// What one drain pass did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DrainReport {
    /// Records this pass leased.
    pub claimed: usize,
    /// Records the broker acknowledged and the outbox recorded as
    /// published — including a duplicate `PubAck`, which means the broker
    /// already holds those bytes.
    pub published: usize,
    /// Claims handed back to `pending` without a delivery.
    pub released: usize,
    /// Records this pass could not deliver.
    ///
    /// A failed publish hands its own claim straight back (so it is counted
    /// in [`Self::released`] as well) and the pass continues with the rest:
    /// one envelope the broker refuses costs its own slot, not the queue.
    /// The two hook-driven fault paths leave the record leased instead, and
    /// there its lease expiry is what makes it deliverable again.
    pub failed: usize,
    /// Acknowledgements refused because the claim had already moved on.
    /// Never an error — a stale worker lost a race it could not see.
    pub stale: usize,
}

/// What a whole [`JetStreamPublisher::run`] loop did before cancellation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DrainSummary {
    pub passes: u64,
    pub published: u64,
    pub released: u64,
    pub failed: u64,
    pub errors: u64,
}

/// What the publisher does after the broker acknowledged one record and
/// before the outbox records it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookAction {
    /// Normal path: record the acknowledgement.
    Continue,
    /// Behave as though the `PubAck` never arrived: leave the record
    /// `claimed` and move on. Its lease expiry is what returns it.
    DropAck,
    /// Behave as though the process died here: abandon the pass at once,
    /// leaving this record and every unattempted claim leased.
    AbortDrain,
}

/// Seam between "the broker accepted it" and "the outbox knows".
///
/// It exists because that window is the one this design cannot test any
/// other way: a lost `PubAck` and a crash before the marker commits are the
/// two failures the whole delivery contract is built to survive, and both
/// live inside a single `await` a test cannot otherwise interrupt. It is a
/// real, always-compiled dispatch rather than a `cfg(test)` branch — a
/// fault path production never runs is a fault path production never
/// checked.
#[async_trait::async_trait]
pub trait PublishHook: Send + Sync + std::fmt::Debug {
    /// Called with the receipt the broker returned, before
    /// `mark_published`.
    async fn after_publish_ack(&self, event_id: &str, receipt: &BrokerReceipt) -> HookAction;
}

/// The shipped hook: acknowledge everything.
#[derive(Debug, Clone, Copy, Default)]
pub struct ContinueHook;

#[async_trait::async_trait]
impl PublishHook for ContinueHook {
    async fn after_publish_ack(&self, _event_id: &str, _receipt: &BrokerReceipt) -> HookAction {
        HookAction::Continue
    }
}

/// A connected publisher bound to one source subject and one outbox.
#[derive(Clone)]
pub struct JetStreamPublisher {
    context: jetstream::Context,
    outbox: Arc<dyn PublicationOutboxPort>,
    config: NatsPublisherConfig,
    hook: Arc<dyn PublishHook>,
}

/// Manual because the outbox port is core's and carries no `Debug`
/// supertrait: a storage handle is not a value an operator reads, and
/// requiring `Debug` on the port to satisfy a derive here would push a
/// formatting concern into the storage contract.
impl std::fmt::Debug for JetStreamPublisher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JetStreamPublisher")
            .field("subject_prefix", &self.config.subject_prefix)
            .field("publisher_id", &self.config.publisher_id)
            .field("hook", &self.hook)
            .finish_non_exhaustive()
    }
}

impl JetStreamPublisher {
    /// Connect to the broker. Stream topology is provisioned by the
    /// deployment; this path only needs publish and reply-subscription
    /// permissions.
    ///
    /// # Errors
    ///
    /// [`PublisherError::Config`] for a lease or timeout the delivery
    /// contract cannot hold, [`PublisherError::Connect`] when the broker is
    /// unreachable or the credentials are refused,
    /// [`PublisherError::Broker`] for `JetStream` failures.
    pub async fn connect(
        config: NatsPublisherConfig,
        outbox: Arc<dyn PublicationOutboxPort>,
    ) -> Result<Self, PublisherError> {
        Self::connect_with_hook(config, outbox, Arc::new(ContinueHook)).await
    }

    /// [`Self::connect`] with an installed [`PublishHook`].
    ///
    /// # Errors
    ///
    /// As [`Self::connect`].
    pub async fn connect_with_hook(
        config: NatsPublisherConfig,
        outbox: Arc<dyn PublicationOutboxPort>,
        hook: Arc<dyn PublishHook>,
    ) -> Result<Self, PublisherError> {
        config.validate()?;
        let client = crate::connect_client(&config.url, &config.auth, config.publish_timeout)
            .await
            .map_err(|error| PublisherError::Connect(error.to_string()))?;
        let context = jetstream::new(client);
        Ok(Self {
            context,
            outbox,
            config,
            hook,
        })
    }

    /// The configuration this publisher was built with.
    #[must_use]
    pub const fn config(&self) -> &NatsPublisherConfig {
        &self.config
    }

    /// One claim → publish → acknowledge pass.
    ///
    /// # Errors
    ///
    /// [`PublisherError::Storage`] from the outbox,
    /// [`PublisherError::BrokerCapacity`] when the stream is full (records
    /// stay `pending`), and [`PublisherError::Publish`] for other broker
    /// failures. An error is returned only when NO record got through, so
    /// an error means the broker — not one envelope — is the problem.
    pub async fn drain_once(&self) -> Result<DrainReport, PublisherError> {
        self.drain_batch(&CancellationToken::new()).await
    }

    /// [`Self::drain_once`], abandoning the batch when `cancel` fires.
    ///
    /// Per-record failures are ISOLATED. One envelope the broker will never
    /// accept — over `max_msg_size`, or over the server's `max_payload` —
    /// used to abort the pass and be re-claimed at the head of the next
    /// one, so every later Fact stalled behind it until the outbox filled
    /// and listenable writes started failing. Now its claim goes straight
    /// back and the pass continues; the storage port's `attempts ASC`
    /// ordering demotes it behind fresher work on the next claim, so it
    /// costs one slot per pass.
    async fn drain_batch(&self, cancel: &CancellationToken) -> Result<DrainReport, PublisherError> {
        let claimed = self
            .outbox
            .claim(
                &self.config.publisher_id,
                self.config.batch,
                self.config.lease,
            )
            .await
            .map_err(|error| PublisherError::Storage(error.to_string()))?;
        let mut report = DrainReport {
            claimed: claimed.len(),
            ..DrainReport::default()
        };
        let mut first_error: Option<PublisherError> = None;
        let mut pending = claimed.into_iter();
        while let Some(record) = pending.next() {
            match self.deliver(&record, &mut report, cancel).await {
                Ok(Flow::Continue) => {}
                Ok(Flow::Abort) => return Ok(report),
                Ok(Flow::Cancelled) => {
                    // Shutdown, not failure: hand back everything this pass
                    // still holds so the next process does not wait out a
                    // lease for work nobody is doing.
                    self.release(&record, &mut report).await;
                    for rest in pending {
                        self.release(&rest, &mut report).await;
                    }
                    return Ok(report);
                }
                Err(error) => {
                    report.failed += 1;
                    self.release(&record, &mut report).await;
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                }
            }
        }
        match first_error {
            // Nothing got through at all: report it, so `run` backs off
            // rather than spinning on a dead connection or a full stream.
            Some(error) if report.published == 0 => Err(error),
            _ => Ok(report),
        }
    }

    /// Drain until cancelled, backing off while the broker is unhappy.
    ///
    /// Never panics and never returns an error: a publisher that stopped
    /// on a broker outage would turn a recoverable delivery pause into a
    /// silent permanent one, and capture keeps running either way.
    pub async fn run(self, cancel: CancellationToken) -> DrainSummary {
        let mut summary = DrainSummary::default();
        let mut backoff = self.config.poll_interval;
        loop {
            if cancel.is_cancelled() {
                return summary;
            }
            summary.passes += 1;
            let idle = match self.drain_batch(&cancel).await {
                Ok(report) => {
                    backoff = self.config.poll_interval;
                    summary.published += report.published as u64;
                    summary.released += report.released as u64;
                    summary.failed += report.failed as u64;
                    if report.published > 0 {
                        tracing::debug!(
                            published = report.published,
                            claimed = report.claimed,
                            "published captured facts"
                        );
                    }
                    // A pass that delivered a full batch is a pass with
                    // more waiting; only an empty claim earns the sleep.
                    report.claimed == 0
                }
                Err(error) => {
                    summary.errors += 1;
                    tracing::warn!(error = %error, "publication drain failed");
                    backoff = next_backoff(backoff, self.config.poll_interval);
                    true
                }
            };
            if idle {
                tokio::select! {
                    () = cancel.cancelled() => return summary,
                    () = tokio::time::sleep(backoff) => {}
                }
            }
        }
    }

    /// Publish one claimed record and record what the broker said.
    ///
    /// Cancellation is observed BETWEEN records and inside the publish
    /// itself, so a shutdown costs one storage round trip rather than
    /// `batch × publish_timeout`.
    async fn deliver(
        &self,
        record: &ClaimedPublication,
        report: &mut DrainReport,
        cancel: &CancellationToken,
    ) -> Result<Flow, PublisherError> {
        if cancel.is_cancelled() {
            return Ok(Flow::Cancelled);
        }
        let subject = subject_for(
            &self.config.subject_prefix,
            record.owner_kind.as_str(),
            record.owner_id,
            &record.event_type,
        );
        let mut headers = async_nats::HeaderMap::new();
        headers.insert(HEADER_MSG_ID, record.event_id.as_str());
        headers.insert(HEADER_CONTENT_TYPE, CONTENT_TYPE_CLOUDEVENTS);
        headers.insert(
            HEADER_SCHEMA,
            format!("{}/{}", record.schema_id, record.schema_version).as_str(),
        );
        // The captured bytes, verbatim. Not the Fact reloaded, not the
        // payload re-rendered by today's flavor, not today's installation
        // identity: the digest beside them in the outbox is the witness
        // that this is what was committed.
        let payload = Bytes::copy_from_slice(&record.envelope);
        let ack = tokio::select! {
            () = cancel.cancelled() => return Ok(Flow::Cancelled),
            ack = self.context.publish_with_headers(subject, headers, payload) => ack,
        };
        let ack = match ack {
            Ok(ack) => ack,
            Err(error) => return Err(refuse(record, &error)),
        };
        let ack = tokio::select! {
            () = cancel.cancelled() => return Ok(Flow::Cancelled),
            ack = tokio::time::timeout(self.config.publish_timeout, ack) => ack,
        };
        let ack = match ack {
            Ok(Ok(ack)) => ack,
            Ok(Err(error)) => return Err(refuse(record, &error)),
            Err(_) => {
                return Err(PublisherError::Publish(format!(
                    "no PubAck for {} within {:?}",
                    record.event_id, self.config.publish_timeout
                )));
            }
        };
        // A duplicate PubAck is a success: the broker already holds these
        // bytes under this id, which is exactly what publishing them was
        // for.
        let receipt = BrokerReceipt {
            stream: ack.stream.clone(),
            sequence: ack.sequence,
        };
        match self
            .hook
            .after_publish_ack(&record.event_id, &receipt)
            .await
        {
            HookAction::Continue => {}
            HookAction::DropAck => {
                report.failed += 1;
                tracing::warn!(
                    event_id = %record.event_id,
                    "publish acknowledgement dropped before it was recorded; \
                     the lease expiry will republish these bytes"
                );
                return Ok(Flow::Continue);
            }
            HookAction::AbortDrain => {
                report.failed += 1;
                return Ok(Flow::Abort);
            }
        }
        match self
            .outbox
            .mark_published(record.id, record.claim, &receipt)
            .await
            .map_err(|error| PublisherError::Storage(error.to_string()))?
        {
            AckOutcome::Published | AckOutcome::AlreadyPublished => report.published += 1,
            AckOutcome::StaleClaim => {
                report.stale += 1;
                tracing::info!(
                    event_id = %record.event_id,
                    sequence = receipt.sequence,
                    "publish acknowledgement was fenced out by a newer claim"
                );
            }
        }
        Ok(Flow::Continue)
    }

    /// Hand one claim back, best effort. A failed release is not an error:
    /// the lease expires on its own, which is the whole reason releases are
    /// an optimization rather than a correctness requirement.
    async fn release(&self, record: &ClaimedPublication, report: &mut DrainReport) {
        match self.outbox.release(record.id, record.claim).await {
            Ok(ReleaseOutcome::Released) => report.released += 1,
            Ok(ReleaseOutcome::AlreadyPublished) => report.published += 1,
            Ok(ReleaseOutcome::StaleClaim) => report.stale += 1,
            Err(error) => {
                report.failed += 1;
                tracing::warn!(
                    event_id = %record.event_id,
                    error = %error,
                    "releasing a claim failed; the lease expiry still returns it"
                );
            }
        }
    }
}

enum Flow {
    Continue,
    /// Stop the pass, leaving this record and every unattempted claim
    /// leased — the injected "the process died here".
    Abort,
    /// Stop the pass and hand every claim back — shutdown.
    Cancelled,
}

/// Classify one publish failure, and say so at `error` level when the
/// broker refused the envelope for its SIZE.
///
/// A size refusal is the one publish failure no retry can fix: not a
/// backoff, not a reconnection, not draining the stream. It is an operator
/// action item — raise `max_msg_size`, or lower the capture ceiling — so it
/// is logged with the id of the record that will otherwise sit in the
/// outbox forever.
fn refuse(record: &ClaimedPublication, error: &jetstream::context::PublishError) -> PublisherError {
    let classified = classify_publish_error(error);
    if is_size_refusal(error) {
        tracing::error!(
            event_id = %record.event_id,
            schema = %record.schema_id,
            envelope_bytes = record.envelope.len(),
            error = %classified,
            "the broker refused this envelope for its size; no retry can deliver it \
             and it stays pending until the stream's max_msg_size or the capture \
             ceiling changes"
        );
    }
    classified
}

/// Whether the broker refused these bytes for being too large, as opposed
/// to refusing them for having nowhere to put them.
fn is_size_refusal(error: &jetstream::context::PublishError) -> bool {
    matches!(error.kind(), PublishErrorKind::MaxPayloadExceeded)
        || error.source_error_code() == Some(jetstream::ErrorCode::STREAM_MESSAGE_EXCEEDS_MAXIMUM)
        || error.to_string().contains("message size exceeds maximum")
}

/// Split "the stream is full" out of every other publish failure.
///
/// Backpressure is not an outage. A full `discard: New` stream is the
/// broker refusing to lose someone else's undelivered message, and the
/// right answer is to leave the record `pending` and tell the operator
/// which limit was hit — not to retry the same byte into the same wall
/// every poll interval and log it as a transport error.
fn classify_publish_error(error: &jetstream::context::PublishError) -> PublisherError {
    let capacity_code = error
        .source_error_code()
        .is_some_and(is_capacity_error_code);
    let text = error.to_string();
    let capacity_text = [
        "maximum bytes exceeded",
        "maximum messages exceeded",
        "insufficient resources",
        "resource limits exceeded",
    ]
    .iter()
    .any(|needle| text.contains(needle));
    if capacity_code
        || capacity_text
        || matches!(error.kind(), PublishErrorKind::MaxPayloadExceeded)
    {
        return PublisherError::BrokerCapacity(text);
    }
    PublisherError::Publish(text)
}

/// `JetStream` API error codes that mean "no room", not "no broker".
fn is_capacity_error_code(code: jetstream::ErrorCode) -> bool {
    [
        jetstream::ErrorCode::STORAGE_RESOURCES_EXCEEDED,
        jetstream::ErrorCode::MEMORY_RESOURCES_EXCEEDED,
        jetstream::ErrorCode::ACCOUNT_RESOURCES_EXCEEDED,
        jetstream::ErrorCode::INSUFFICIENT_RESOURCES,
        jetstream::ErrorCode::STREAM_MESSAGE_EXCEEDS_MAXIMUM,
        jetstream::ErrorCode::STREAM_LIMITS,
        jetstream::ErrorCode::STREAM_STORE_FAILED,
    ]
    .contains(&code)
}

/// Read the `JetStream` API error code out of a publish failure, if the
/// server sent one.
trait SourceErrorCode {
    fn source_error_code(&self) -> Option<jetstream::ErrorCode>;
}

impl SourceErrorCode for jetstream::context::PublishError {
    fn source_error_code(&self) -> Option<jetstream::ErrorCode> {
        let mut source = std::error::Error::source(self);
        while let Some(error) = source {
            if let Some(api) = error.downcast_ref::<jetstream::Error>() {
                return Some(api.error_code());
            }
            source = error.source();
        }
        None
    }
}

/// Doubling backoff, capped so a long outage still polls often enough to
/// notice recovery within a few seconds.
///
/// `min` then `max`, never `clamp`: `Ord::clamp` PANICS when `min > max`,
/// and a poll interval above the ceiling is a legal configuration
/// (`PROXIMA_NATS_POLL_MS=60000`). Under `clamp` the first broker error
/// would have killed the publisher task and left the outbox undrained for
/// the life of the process — the one failure mode [`JetStreamPublisher::run`]
/// exists to rule out. A floor above the ceiling wins: an operator who asked
/// to poll every minute gets a minute.
fn next_backoff(current: Duration, floor: Duration) -> Duration {
    const CEILING: Duration = Duration::from_secs(30);
    let doubled = current.saturating_mul(2);
    doubled.min(CEILING).max(floor)
}

/// Why a publish pass stopped.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PublisherError {
    #[error(transparent)]
    Config(#[from] crate::config::ConfigError),
    #[error("connecting to the NATS broker failed: {0}")]
    Connect(String),
    #[error("JetStream refused the request: {0}")]
    Broker(String),
    #[error(
        "the stream is at capacity and refused the publish ({0}); the record stays pending \
         until capacity is raised or the backlog drains"
    )]
    BrokerCapacity(String),
    #[error("publishing failed: {0}")]
    Publish(String),
    #[error("outbox storage failed: {0}")]
    Storage(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::NatsPublisherConfig;

    #[test]
    fn the_backoff_doubles_and_stops_growing() {
        let floor = Duration::from_millis(500);
        let mut current = floor;
        for _ in 0..20 {
            current = next_backoff(current, floor);
        }
        assert_eq!(current, Duration::from_secs(30));
        assert_eq!(next_backoff(floor, floor), Duration::from_secs(1));
    }

    /// `PROXIMA_NATS_POLL_MS=60000` is a legal configuration, and under
    /// `Ord::clamp(floor, CEILING)` the first broker error panicked the
    /// publisher task out of existence.
    #[test]
    fn a_poll_interval_above_the_ceiling_backs_off_instead_of_panicking() {
        let floor = Duration::from_mins(1);
        let mut current = floor;
        for _ in 0..8 {
            current = next_backoff(current, floor);
            assert_eq!(current, floor);
        }
        let config = NatsPublisherConfig::from_lookup(|key: &str| match key {
            "PROXIMA_NATS_URL" => Some("nats://127.0.0.1:4222".to_owned()),
            "PROXIMA_NATS_POLL_MS" => Some("60000".to_owned()),
            _ => None,
        })
        .expect("a minute is a legal poll interval")
        .expect("the presence key is set");
        assert_eq!(
            next_backoff(config.poll_interval, config.poll_interval),
            Duration::from_mins(1)
        );
    }

    #[test]
    fn a_full_stream_is_backpressure_and_a_broken_pipe_is_not() {
        let full = jetstream::context::PublishError::new(PublishErrorKind::MaxPayloadExceeded);
        assert!(matches!(
            classify_publish_error(&full),
            PublisherError::BrokerCapacity(_)
        ));
        let broken = jetstream::context::PublishError::new(PublishErrorKind::BrokenPipe);
        assert!(matches!(
            classify_publish_error(&broken),
            PublisherError::Publish(_)
        ));

        // And within backpressure, the size refusal is the one no retry
        // fixes — the distinction that decides whether the record is
        // logged at `error` with its event id.
        assert!(is_size_refusal(&full));
        assert!(!is_size_refusal(&broken));
        let live_broker_text = "message size exceeds maximum allowed (code 400, error code 10054)";
        assert!(
            is_size_refusal(&jetstream::context::PublishError::with_source(
                PublishErrorKind::Other,
                live_broker_text
            )),
            "the JetStream refusal a stream max_msg_size produces must be recognised"
        );
    }
}
