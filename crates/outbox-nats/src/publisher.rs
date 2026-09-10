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

use crate::config::{DeliveryProfile, NatsPublisherConfig, subject_for};

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

/// A connected publisher bound to one stream and one outbox.
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
            .field("stream", &self.config.stream)
            .field("subject_prefix", &self.config.subject_prefix)
            .field("publisher_id", &self.config.publisher_id)
            .field("hook", &self.hook)
            .finish_non_exhaustive()
    }
}

impl JetStreamPublisher {
    /// Connect to the broker and make sure the stream exists in the
    /// configured profile.
    ///
    /// An existing stream is VERIFIED, never rewritten: silently updating
    /// someone else's stream from `Limits`/`New` to whatever this binary
    /// prefers is how an operator loses undelivered messages during a
    /// rolling deploy.
    ///
    /// # Errors
    ///
    /// [`PublisherError::Config`] for a lease or timeout the delivery
    /// contract cannot hold, [`PublisherError::Connect`] when the broker is
    /// unreachable or the credentials are refused,
    /// [`PublisherError::StreamMismatch`] when an existing stream
    /// contradicts the profile, and [`PublisherError::Broker`] for other
    /// `JetStream` failures.
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
        verify_payload_ceiling(&config, client.max_payload())?;
        let context = jetstream::new(client);
        ensure_stream(&context, &config).await?;
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

/// Refuse a configuration in which the outbox would hold records the SERVER
/// can never accept.
///
/// `max_payload` is a server-wide ceiling no stream configuration raises,
/// so a per-message ceiling above it describes a stream that would refuse
/// its own largest legal message. The other half of the chain — capture
/// ceiling + envelope headroom ≤ this per-message ceiling — is settled
/// before the config is built, by `with_capture_limits` and the facade's
/// `PROXIMA_NATS_MAX_MESSAGE_BYTES` reconciliation; and an EXISTING stream
/// with a smaller `max_msg_size` than this one is refused by
/// [`verify_compatible`]. Together those make "a record the broker can
/// never take" a configuration error rather than a delivery mystery.
fn verify_payload_ceiling(
    config: &NatsPublisherConfig,
    server_max_payload: usize,
) -> Result<(), PublisherError> {
    let configured = usize::try_from(config.max_message_bytes).unwrap_or(usize::MAX);
    if configured > server_max_payload {
        return Err(PublisherError::PayloadCeiling {
            stream: config.stream.clone(),
            configured,
            server_max: server_max_payload,
        });
    }
    Ok(())
}

/// Create the stream, or prove the one already there still honours the
/// profile.
async fn ensure_stream(
    context: &jetstream::Context,
    config: &NatsPublisherConfig,
) -> Result<(), PublisherError> {
    let desired = stream_config(config);
    match context.get_stream(&config.stream).await {
        Ok(mut stream) => {
            let info = stream
                .info()
                .await
                .map_err(|error| PublisherError::Broker(error.to_string()))?;
            verify_compatible(&config.stream, &info.config, &desired)
        }
        Err(error) if is_stream_not_found(&error) => {
            context
                .create_stream(desired)
                .await
                .map_err(|error| PublisherError::Broker(error.to_string()))?;
            Ok(())
        }
        Err(error) => Err(PublisherError::Broker(error.to_string())),
    }
}

/// The `local-file` profile as a stream configuration.
fn stream_config(config: &NatsPublisherConfig) -> jetstream::stream::Config {
    let DeliveryProfile::LocalFile = config.profile;
    jetstream::stream::Config {
        name: config.stream.clone(),
        subjects: vec![config.subject_filter()],
        // File, not Memory: a broker restart must not be a data loss
        // event for events a Fact write already committed to.
        storage: jetstream::stream::StorageType::File,
        num_replicas: 1,
        retention: jetstream::stream::RetentionPolicy::Limits,
        // Discard NEW rather than old: a full stream must refuse the
        // PubAck, leaving the record `pending` in Postgres. Discarding old
        // would silently drop events a consumer never saw and report
        // success to the publisher.
        discard: jetstream::stream::DiscardPolicy::New,
        // No age limit: unacknowledged work never expires on a clock.
        max_age: Duration::ZERO,
        max_bytes: config.max_stream_bytes,
        max_messages: -1,
        max_message_size: config.max_message_bytes,
        duplicate_window: config.duplicate_window,
        description: Some("Proxima captured Fact publications (docs/18)".to_owned()),
        ..jetstream::stream::Config::default()
    }
}

/// One field on which an existing stream contradicts the profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamFieldMismatch {
    pub field: &'static str,
    pub found: String,
    pub expected: String,
}

impl StreamFieldMismatch {
    fn new(field: &'static str, found: impl Into<String>, expected: impl Into<String>) -> Self {
        Self {
            field,
            found: found.into(),
            expected: expected.into(),
        }
    }
}

impl std::fmt::Display for StreamFieldMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} = {}, requires {}",
            self.field, self.found, self.expected
        )
    }
}

/// Refuse an existing stream whose durability contract is weaker than the
/// profile's, naming EVERY contradiction.
///
/// Each of these changes what a `PubAck` means to the outbox, which commits
/// `state = 'published'` on the strength of one:
///
/// - `storage`, `retention`, `discard`: the three that decide whether an
///   accepted message survives a restart, an ACK, or a full stream.
/// - `max_age`: with `Limits` retention, a non-zero age silently expires
///   messages the outbox already marked published — a consumer that was
///   down over the window never sees them, and nothing anywhere records
///   that they were lost.
/// - `num_replicas`: fewer replicas than the profile promises is a
///   different durability claim under node loss.
/// - `max_msg_size` and `duplicate_window`: SMALLER than desired is not
///   operator tuning. Under-sized refuses records this deployment's capture
///   ceiling admits, and a short dedup window turns the republication that
///   follows a lost `PubAck` into a duplicate on the stream.
///
/// Larger values of the last two, and every other stream setting, are
/// operator tuning and are left alone. All mismatches are reported together
/// because the operator's next action is one stream re-creation, not one
/// per redeploy.
fn verify_compatible(
    name: &str,
    existing: &jetstream::stream::Config,
    desired: &jetstream::stream::Config,
) -> Result<(), PublisherError> {
    let mut mismatches = Vec::new();
    if existing.storage != desired.storage {
        mismatches.push(StreamFieldMismatch::new(
            "storage",
            format!("{:?}", existing.storage),
            format!("{:?}", desired.storage),
        ));
    }
    if existing.retention != desired.retention {
        mismatches.push(StreamFieldMismatch::new(
            "retention",
            format!("{:?}", existing.retention),
            format!("{:?}", desired.retention),
        ));
    }
    if existing.discard != desired.discard {
        mismatches.push(StreamFieldMismatch::new(
            "discard",
            format!("{:?}", existing.discard),
            format!("{:?}", desired.discard),
        ));
    }
    if existing.max_age != desired.max_age {
        mismatches.push(StreamFieldMismatch::new(
            "max_age",
            format!("{:?}", existing.max_age),
            "no age limit, so unacknowledged work never expires on a clock",
        ));
    }
    if existing.num_replicas != desired.num_replicas {
        mismatches.push(StreamFieldMismatch::new(
            "num_replicas",
            existing.num_replicas.to_string(),
            desired.num_replicas.to_string(),
        ));
    }
    if !message_size_admits(existing.max_message_size, desired.max_message_size) {
        mismatches.push(StreamFieldMismatch::new(
            "max_msg_size",
            existing.max_message_size.to_string(),
            format!("at least {}", desired.max_message_size),
        ));
    }
    if existing.duplicate_window < desired.duplicate_window {
        mismatches.push(StreamFieldMismatch::new(
            "duplicate_window",
            format!("{:?}", existing.duplicate_window),
            format!("at least {:?}", desired.duplicate_window),
        ));
    }
    if !existing
        .subjects
        .iter()
        .any(|subject| desired.subjects.contains(subject))
    {
        mismatches.push(StreamFieldMismatch::new(
            "subjects",
            existing.subjects.join(","),
            desired.subjects.join(","),
        ));
    }
    let Some(first) = mismatches.first().cloned() else {
        return Ok(());
    };
    Err(PublisherError::StreamMismatch {
        stream: name.to_owned(),
        field: first.field,
        found: first.found,
        expected: first.expected,
        mismatches,
    })
}

/// Whether an existing `max_msg_size` accepts everything the desired one
/// does. `-1` is `JetStream`'s "no limit" and therefore accepts anything.
const fn message_size_admits(existing: i32, desired: i32) -> bool {
    existing < 0 || existing >= desired
}

/// Every contradiction on one line, in the order the profile checks them.
fn render_mismatches(mismatches: &[StreamFieldMismatch]) -> String {
    mismatches
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ")
}

fn is_stream_not_found(error: &jetstream::context::GetStreamError) -> bool {
    matches!(
        error.kind(),
        jetstream::context::GetStreamErrorKind::JetStream(inner)
            if inner.error_code() == jetstream::ErrorCode::STREAM_NOT_FOUND
    )
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
        "stream {stream} already exists in a shape this deployment's delivery profile \
         refuses ({}); refusing to rewrite a stream that may hold undelivered messages",
        render_mismatches(.mismatches)
    )]
    StreamMismatch {
        stream: String,
        /// The first contradiction, so a caller can match on one field name.
        field: &'static str,
        found: String,
        expected: String,
        /// Every contradiction, that first one included.
        mismatches: Vec<StreamFieldMismatch>,
    },
    #[error(
        "the per-message ceiling for stream {stream} is {configured} bytes, over the \
         broker's {server_max}-byte max_payload; a record captured at that size could \
         never be published, so the configuration is refused before anything is captured"
    )]
    PayloadCeiling {
        stream: String,
        configured: usize,
        server_max: usize,
    },
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

    fn config() -> NatsPublisherConfig {
        NatsPublisherConfig::new("nats://127.0.0.1:4222").expect("publisher id")
    }

    #[test]
    fn the_local_file_profile_is_the_durable_one() {
        let stream = stream_config(&config());
        assert_eq!(stream.storage, jetstream::stream::StorageType::File);
        assert_eq!(stream.retention, jetstream::stream::RetentionPolicy::Limits);
        assert_eq!(stream.discard, jetstream::stream::DiscardPolicy::New);
        assert_eq!(stream.max_age, Duration::ZERO);
        assert_eq!(stream.num_replicas, 1);
        assert_eq!(stream.subjects, vec!["proxima.fact.>".to_owned()]);
        assert!(stream.max_bytes > 0, "capacity must be explicit");
    }

    #[test]
    fn an_existing_stream_that_discards_old_messages_is_refused() {
        let desired = stream_config(&config());
        let mut existing = desired.clone();
        existing.discard = jetstream::stream::DiscardPolicy::Old;
        let err = verify_compatible("S", &existing, &desired).expect_err("downgrade");
        assert!(
            matches!(
                err,
                PublisherError::StreamMismatch {
                    field: "discard",
                    ..
                }
            ),
            "{err}"
        );

        let mut existing = desired.clone();
        existing.storage = jetstream::stream::StorageType::Memory;
        assert!(matches!(
            verify_compatible("S", &existing, &desired),
            Err(PublisherError::StreamMismatch {
                field: "storage",
                ..
            })
        ));

        let mut existing = desired.clone();
        existing.subjects = vec!["other.>".to_owned()];
        assert!(matches!(
            verify_compatible("S", &existing, &desired),
            Err(PublisherError::StreamMismatch {
                field: "subjects",
                ..
            })
        ));

        // Operator tuning is left alone: a bigger stream is still the
        // same durability contract.
        let mut existing = desired.clone();
        existing.max_bytes = desired.max_bytes * 4;
        assert!(verify_compatible("S", &existing, &desired).is_ok());
    }

    /// The mismatch on this list nobody would notice: the stream still says
    /// `File`/`Limits`/`New`, and quietly drops committed events on a clock.
    #[test]
    fn an_existing_stream_with_a_max_age_expires_events_the_outbox_committed() {
        let desired = stream_config(&config());
        let mut existing = desired.clone();
        existing.max_age = Duration::from_hours(24);
        let err = verify_compatible("S", &existing, &desired).expect_err("an age limit");
        assert!(
            matches!(
                err,
                PublisherError::StreamMismatch {
                    field: "max_age",
                    ..
                }
            ),
            "{err}"
        );
        assert!(
            err.to_string().contains("max_age"),
            "the operator must be told which field: {err}"
        );
    }

    #[test]
    fn a_weaker_replica_count_size_or_dedup_window_is_refused() {
        let desired = stream_config(&config());
        let refused_field = |existing: &jetstream::stream::Config| -> &'static str {
            match verify_compatible("S", existing, &desired) {
                Err(PublisherError::StreamMismatch { field, .. }) => field,
                other => panic!("a weaker stream must be refused, got {other:?}"),
            }
        };

        let mut existing = desired.clone();
        existing.num_replicas = 0;
        assert_eq!(refused_field(&existing), "num_replicas");

        let mut existing = desired.clone();
        existing.max_message_size = 128;
        assert_eq!(refused_field(&existing), "max_msg_size");

        let mut existing = desired.clone();
        existing.duplicate_window = Duration::from_secs(1);
        assert_eq!(refused_field(&existing), "duplicate_window");

        // Bigger than asked for is operator tuning, and `-1` is
        // JetStream's "no limit" — neither weakens the contract.
        let mut existing = desired.clone();
        existing.max_message_size = -1;
        existing.duplicate_window = desired.duplicate_window * 2;
        assert!(verify_compatible("S", &existing, &desired).is_ok());
    }

    #[test]
    fn every_contradiction_is_reported_at_once() {
        let desired = stream_config(&config());
        let mut existing = desired.clone();
        existing.max_age = Duration::from_mins(1);
        existing.storage = jetstream::stream::StorageType::Memory;
        existing.duplicate_window = Duration::ZERO;
        let err = verify_compatible("S", &existing, &desired).expect_err("three contradictions");
        let PublisherError::StreamMismatch { mismatches, .. } = &err else {
            panic!("expected a stream mismatch, got {err}");
        };
        let fields: Vec<&str> = mismatches.iter().map(|m| m.field).collect();
        assert_eq!(fields, ["storage", "max_age", "duplicate_window"], "{err}");
        let rendered = err.to_string();
        for field in fields {
            assert!(
                rendered.contains(field),
                "one refusal must name every field an operator has to fix: {rendered}"
            );
        }
    }

    #[test]
    fn a_stream_ceiling_over_the_servers_max_payload_is_refused_at_connect() {
        let mut config = config();
        config.max_message_bytes = 2 * 1024 * 1024;
        let err = verify_payload_ceiling(&config, 1024 * 1024).expect_err("over the server");
        assert!(
            matches!(err, PublisherError::PayloadCeiling { .. }),
            "{err}"
        );
        assert!(verify_payload_ceiling(&config, 4 * 1024 * 1024).is_ok());
    }

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
