//! Bounded removal of retained `JetStream` copies whose captured Fact origin
//! has been revoked by database erasure.
//!
//! The cleaner is host infrastructure: every origin verdict is committed by
//! storage before the broker request begins, and a broker failure never
//! changes database erasure's result.

use std::num::NonZeroU32;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_nats::jetstream::{self, stream::Stream};
use futures::FutureExt;
use proxima_core::MemoryId;
use proxima_core::mcp::{PrefixedUuidClass, format_prefixed_uuid, parse_prefixed_uuid};
use proxima_core::owner::{OwnerRef, parse_external_key};
use proxima_core::storage_ports::publication::{
    OriginScope, PublicationOriginEligibility, PublicationOriginEligibilityPort,
};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::config::{InboxPrefix, NatsAuth, redacted_url};
use crate::consumer::CloudEventEnvelope;
use crate::{
    CONTENT_TYPE_CLOUDEVENTS, HEADER_CONTENT_TYPE, HEADER_MSG_ID, HEADER_ORIGIN_SCOPE,
    connect_client,
};

/// The deployment-owned fresh, exclusive-Proxima-publisher stream.
pub const COPY_CLEANER_STREAM: &str = "PROXIMA_FACTS";
/// Canonical prefix used by the publisher whose retained copies are scanned.
pub const COPY_CLEANER_SUBJECT_PREFIX: &str = "proxima.fact";
/// Reply namespace reserved for the cleaner credential.
pub const COPY_CLEANER_INBOX_PREFIX: &str = "PROXIMA_PURGE_INBOX";

const DEFAULT_ITEMS_PER_SLICE: u32 = 128;
const DEFAULT_SLICE_BUDGET: Duration = Duration::from_secs(2);
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_SCAN_INTERVAL: Duration = Duration::from_secs(30);
const DEFAULT_SHUTDOWN_GRACE: Duration = Duration::from_secs(2);
const RECONNECT_DELAY: Duration = Duration::from_secs(1);

/// Explicit host configuration for the dedicated retained-copy cleaner.
///
/// Stream and source prefix are fixed by the Proxima deployment contract;
/// the cleaner has no API for selecting another stream or subject family.
#[derive(Clone)]
pub struct JetStreamCopyCleanerConfig {
    url: String,
    auth: NatsAuth,
    inbox_prefix: InboxPrefix,
    items_per_slice: NonZeroU32,
    slice_budget: Duration,
    request_timeout: Duration,
    scan_interval: Duration,
    shutdown_grace: Duration,
}

impl JetStreamCopyCleanerConfig {
    /// Configure a cleaner for the canonical Proxima stream.
    ///
    /// # Panics
    /// The built-in inbox prefix is a fixed valid token; this can panic only
    /// if that source constant is changed to an invalid value.
    #[must_use]
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            auth: NatsAuth::None,
            inbox_prefix: InboxPrefix::new(COPY_CLEANER_INBOX_PREFIX)
                .expect("the built-in cleaner inbox prefix is valid"),
            items_per_slice: NonZeroU32::new(DEFAULT_ITEMS_PER_SLICE)
                .expect("the cleaner item limit is positive"),
            slice_budget: DEFAULT_SLICE_BUDGET,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            scan_interval: DEFAULT_SCAN_INTERVAL,
            shutdown_grace: DEFAULT_SHUTDOWN_GRACE,
        }
    }

    /// Set the cleaner's dedicated broker credential.
    #[must_use]
    pub fn auth(mut self, auth: NatsAuth) -> Self {
        self.auth = auth;
        self
    }

    /// Set a validated reply namespace owned by this cleaner role.
    #[must_use]
    pub fn inbox_prefix(mut self, prefix: InboxPrefix) -> Self {
        self.inbox_prefix = prefix;
        self
    }

    /// Override the per-slice item limit, soft time budget, and wait bounds.
    /// The time budget is checked between messages; a started message check
    /// can finish after it, with each request bounded by `request_timeout`.
    ///
    /// # Errors
    /// Returns [`CleanerConfigError::ZeroDuration`] if any duration is zero.
    pub fn bounds(
        mut self,
        items_per_slice: NonZeroU32,
        slice_budget: Duration,
        request_timeout: Duration,
        scan_interval: Duration,
        shutdown_grace: Duration,
    ) -> Result<Self, CleanerConfigError> {
        if slice_budget.is_zero()
            || request_timeout.is_zero()
            || scan_interval.is_zero()
            || shutdown_grace.is_zero()
        {
            return Err(CleanerConfigError::ZeroDuration);
        }
        self.items_per_slice = items_per_slice;
        self.slice_budget = slice_budget;
        self.request_timeout = request_timeout;
        self.scan_interval = scan_interval;
        self.shutdown_grace = shutdown_grace;
        Ok(self)
    }
}

impl std::fmt::Debug for JetStreamCopyCleanerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JetStreamCopyCleanerConfig")
            .field("url", &redacted_url(&self.url))
            .field("auth", &self.auth)
            .field("inbox_prefix", &self.inbox_prefix)
            .field("items_per_slice", &self.items_per_slice)
            .field("slice_budget", &self.slice_budget)
            .field("request_timeout", &self.request_timeout)
            .field("scan_interval", &self.scan_interval)
            .field("shutdown_grace", &self.shutdown_grace)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CleanerConfigError {
    #[error("cleaner durations must all be greater than zero")]
    ZeroDuration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyCleanerTaskState {
    Starting,
    Running,
    Stopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyCleanerConnectionState {
    NotObserved,
    Pending,
    Connected,
    Disconnected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyCleanerScanState {
    NotObserved,
    Clean,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyCleanerFailure {
    Connect,
    StreamInfo,
    StreamRead,
    OriginCheck,
    Delete,
    RequestTimeout,
    /// A message on this stream was published by a DIFFERENT Proxima
    /// installation.
    ///
    /// A whole-cycle failure rather than a per-message skip, because it is
    /// not a statement about one message: the stream is not this
    /// installation's to clean, and every absent origin row on it would
    /// read as a revocation. Scanning further can only compound that.
    ForeignOriginScope,
}

/// Fixed operational state and aggregate counters; no payload or Fact
/// identity is exposed through the cleaner health view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CopyCleanerHealth {
    pub task: CopyCleanerTaskState,
    pub connection: CopyCleanerConnectionState,
    pub scan: CopyCleanerScanState,
    pub last_failure: Option<CopyCleanerFailure>,
    pub cycles_completed: u64,
    pub messages_examined: u64,
    pub messages_deleted: u64,
    pub messages_unknown: u64,
    pub failures: u64,
}

impl CopyCleanerHealth {
    #[must_use]
    pub const fn is_ready(self) -> bool {
        matches!(self.task, CopyCleanerTaskState::Running)
            && matches!(self.connection, CopyCleanerConnectionState::Connected)
            && matches!(self.scan, CopyCleanerScanState::Clean)
    }
}

#[derive(Clone)]
pub struct CopyCleanerHealthReader {
    inner: Arc<Mutex<CopyCleanerHealthInner>>,
}

impl std::fmt::Debug for CopyCleanerHealthReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("CopyCleanerHealthReader")
            .field(&self.snapshot())
            .finish()
    }
}

impl CopyCleanerHealthReader {
    #[must_use]
    pub fn snapshot(&self) -> CopyCleanerHealth {
        let inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let connection = inner.client.as_ref().map_or(inner.connection, |client| {
            match client.connection_state() {
                async_nats::connection::State::Pending => CopyCleanerConnectionState::Pending,
                async_nats::connection::State::Connected => CopyCleanerConnectionState::Connected,
                async_nats::connection::State::Disconnected => {
                    CopyCleanerConnectionState::Disconnected
                }
            }
        });
        CopyCleanerHealth {
            task: inner.task,
            connection,
            scan: inner.scan,
            last_failure: inner.last_failure,
            cycles_completed: inner.cycles_completed,
            messages_examined: inner.messages_examined,
            messages_deleted: inner.messages_deleted,
            messages_unknown: inner.messages_unknown,
            failures: inner.failures,
        }
    }
}

struct CopyCleanerHealthInner {
    task: CopyCleanerTaskState,
    connection: CopyCleanerConnectionState,
    scan: CopyCleanerScanState,
    last_failure: Option<CopyCleanerFailure>,
    cycles_completed: u64,
    messages_examined: u64,
    messages_deleted: u64,
    messages_unknown: u64,
    failures: u64,
    client: Option<async_nats::Client>,
}

impl Default for CopyCleanerHealthInner {
    fn default() -> Self {
        Self {
            task: CopyCleanerTaskState::Starting,
            connection: CopyCleanerConnectionState::NotObserved,
            scan: CopyCleanerScanState::NotObserved,
            last_failure: None,
            cycles_completed: 0,
            messages_examined: 0,
            messages_deleted: 0,
            messages_unknown: 0,
            failures: 0,
            client: None,
        }
    }
}

struct CopyCleanerTaskGuard {
    inner: Arc<Mutex<CopyCleanerHealthInner>>,
}

impl CopyCleanerTaskGuard {
    fn update(&self, update: impl FnOnce(&mut CopyCleanerHealthInner)) {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        update(&mut inner);
    }

    fn attach(&self, client: async_nats::Client) {
        self.update(|inner| {
            inner.connection = CopyCleanerConnectionState::Connected;
            inner.client = Some(client);
        });
    }

    fn disconnected(&self) {
        self.update(|inner| {
            inner.connection = CopyCleanerConnectionState::Disconnected;
            inner.client = None;
        });
    }

    fn fail(&self, failure: CopyCleanerFailure) {
        self.update(|inner| {
            inner.scan = CopyCleanerScanState::Failed;
            inner.last_failure = Some(failure);
            inner.failures = inner.failures.saturating_add(1);
        });
    }

    fn observe_slice(&self, report: &CopyCleanerSliceReport) {
        self.update(|inner| {
            inner.messages_examined = inner
                .messages_examined
                .saturating_add(u64::from(report.examined));
            inner.messages_deleted = inner
                .messages_deleted
                .saturating_add(u64::from(report.deleted));
            inner.messages_unknown = inner
                .messages_unknown
                .saturating_add(u64::from(report.unknown));
            if report.unknown > 0 {
                inner.scan = CopyCleanerScanState::Failed;
                inner.last_failure = Some(CopyCleanerFailure::StreamRead);
                inner.failures = inner.failures.saturating_add(u64::from(report.unknown));
            }
            if report.cycle_complete {
                inner.cycles_completed = inner.cycles_completed.saturating_add(1);
                if report.cycle_clean {
                    inner.scan = CopyCleanerScanState::Clean;
                    inner.last_failure = None;
                } else {
                    inner.scan = CopyCleanerScanState::Failed;
                }
            }
        });
    }
}

impl Drop for CopyCleanerTaskGuard {
    fn drop(&mut self) {
        self.update(|inner| {
            inner.task = CopyCleanerTaskState::Stopped;
            inner.connection = CopyCleanerConnectionState::NotObserved;
            inner.client = None;
        });
    }
}

/// Result from one production scan slice.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CopyCleanerSliceReport {
    pub examined: u32,
    pub deleted: u32,
    pub unknown: u32,
    pub cycle_complete: bool,
    pub cycle_clean: bool,
}

/// A connected cleaner session. The public scan slice is also the helper
/// used by the owned loop, so tests can observe a finite real broker request.
pub struct JetStreamCopyCleaner {
    config: JetStreamCopyCleanerConfig,
    eligibility: Arc<dyn PublicationOriginEligibilityPort>,
    origin_scope: OriginScope,
    client: async_nats::Client,
    stream: Stream,
    cycle_end: Option<u64>,
    next_sequence: Option<u64>,
    cycle_faulted: bool,
}

impl std::fmt::Debug for JetStreamCopyCleaner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JetStreamCopyCleaner")
            .field("config", &self.config)
            .field("cycle_end", &self.cycle_end)
            .field("next_sequence", &self.next_sequence)
            .field("cycle_faulted", &self.cycle_faulted)
            .finish_non_exhaustive()
    }
}

impl JetStreamCopyCleaner {
    /// Connect using the cleaner credential and bind to the canonical stream.
    ///
    /// `origin_scope` must be read from the SAME database that backs
    /// `eligibility`. It is a parameter rather than a config field for
    /// exactly that reason: an operator-supplied value could be made to
    /// agree with a foreign stream, and agreement is the whole check.
    ///
    /// # Errors
    /// Returns a fixed category and never includes broker text or credentials.
    pub async fn connect(
        config: JetStreamCopyCleanerConfig,
        eligibility: Arc<dyn PublicationOriginEligibilityPort>,
        origin_scope: OriginScope,
    ) -> Result<Self, CopyCleanerConnectError> {
        let client = connect_client(
            &config.url,
            &config.auth,
            Some(&config.inbox_prefix),
            config.request_timeout,
        )
        .await
        .map_err(|_| CopyCleanerConnectError::Connect)?;
        let context = jetstream::new(client.clone());
        let stream = context
            .get_stream(COPY_CLEANER_STREAM)
            .await
            .map_err(|_| CopyCleanerConnectError::Stream)?;
        Ok(Self {
            config,
            eligibility,
            origin_scope,
            client,
            stream,
            cycle_end: None,
            next_sequence: None,
            cycle_faulted: false,
        })
    }

    /// Perform one bounded slice of the same scanner used by [`Self::run`].
    ///
    /// The item limit is strict and the time budget is checked between
    /// messages, so a started GET, committed origin check, and DELETE can
    /// finish after the soft time budget. Each request is bounded by the
    /// configured request timeout. Successful ineligible verdicts are
    /// committed by storage before any message DELETE. Errors retain the
    /// current cursor for a later retry.
    /// Unknown canonicality failures are counted, retained, and skipped so
    /// later messages in the snapshot still receive their check.
    ///
    /// # Errors
    /// Returns one fixed failure category. Broker/database diagnostics and
    /// message data are intentionally withheld from callers and logs.
    pub async fn scan_slice(&mut self) -> Result<CopyCleanerSliceReport, CopyCleanerFailure> {
        let started = tokio::time::Instant::now();
        let mut report = CopyCleanerSliceReport::default();

        if self.cycle_end.is_none() {
            let info = self
                .stream_info()
                .await
                .map_err(|failure| self.mark_failed(failure))?;
            let end = info.state.last_sequence;
            let first = info.state.first_sequence;
            if end == 0 || first > end {
                report.cycle_complete = true;
                report.cycle_clean = true;
                return Ok(report);
            }
            self.cycle_end = Some(end);
            self.next_sequence = Some(first);
            self.cycle_faulted = false;
        }

        while report.examined < self.config.items_per_slice.get()
            && started.elapsed() < self.config.slice_budget
        {
            let Some(end) = self.cycle_end else {
                break;
            };
            let Some(next) = self.next_sequence else {
                report.cycle_clean = self.finish_cycle();
                report.cycle_complete = true;
                return Ok(report);
            };
            if next > end {
                report.cycle_clean = self.finish_cycle();
                report.cycle_complete = true;
                return Ok(report);
            }

            let message = match self.next_message(next).await {
                Ok(Some(message)) if message.sequence > end => {
                    report.cycle_clean = self.finish_cycle();
                    report.cycle_complete = true;
                    return Ok(report);
                }
                Ok(Some(message)) if message.sequence < next => {
                    return Err(self.mark_failed(CopyCleanerFailure::StreamRead));
                }
                Ok(Some(message)) => message,
                Ok(None) => {
                    report.cycle_clean = self.finish_cycle();
                    report.cycle_complete = true;
                    return Ok(report);
                }
                Err(failure) => {
                    return Err(self.mark_failed(failure));
                }
            };

            report.examined = report.examined.saturating_add(1);
            let identity = match message_identity(
                message.subject.as_str(),
                &message.headers,
                &message.payload,
                self.origin_scope,
            ) {
                Ok(identity) => identity,
                // Published by another installation. Not this cleaner's
                // stream, so stop the cycle rather than skip the message:
                // every origin row it would consult belongs to a different
                // database, and "no row" there means nothing.
                Err(MessageVerdict::ForeignScope) => {
                    return Err(self.mark_failed(CopyCleanerFailure::ForeignOriginScope));
                }
                // Unattributable: noncanonical, or published before this
                // installation began stamping. Both are retained and
                // counted, and both fault the cycle so the health view
                // never reports Clean over copies nothing vouched for.
                Err(MessageVerdict::Unknown) => {
                    report.unknown = report.unknown.saturating_add(1);
                    self.cycle_faulted = true;
                    tracing::warn!(
                        failure_category = "unknown_message",
                        unknown_count = report.unknown,
                        "JetStream copy cleaner retained a noncanonical message"
                    );
                    self.advance(message.sequence);
                    continue;
                }
            };

            let eligibility = match self.check_eligibility(identity.owner, identity.fact).await {
                Ok(eligibility) => eligibility,
                Err(failure) => return Err(self.mark_failed(failure)),
            };

            if eligibility == PublicationOriginEligibility::Ineligible {
                let deleted = match self.delete_message(message.sequence).await {
                    Ok(deleted) => deleted,
                    Err(failure) => return Err(self.mark_failed(failure)),
                };
                if !deleted {
                    return Err(self.mark_failed(CopyCleanerFailure::Delete));
                }
                report.deleted = report.deleted.saturating_add(1);
            }
            self.advance(message.sequence);
        }

        Ok(report)
    }

    /// Run this connected cleaner until cancellation. Cancellation waits for
    /// the current finite slice up to `shutdown_grace`; the supervised host
    /// spawn adds the initial-connect retry and health reader.
    pub async fn run(mut self, cancel: CancellationToken) {
        loop {
            if cancel.is_cancelled() {
                return;
            }
            let shutdown_grace = self.config.shutdown_grace;
            let scan_interval = self.config.scan_interval;
            let slice = self.scan_slice();
            tokio::pin!(slice);
            tokio::select! {
                () = cancel.cancelled() => {
                    let _ = tokio::time::timeout(shutdown_grace, &mut slice).await;
                    return;
                }
                result = &mut slice => {
                    if result.is_err() || result.is_ok_and(|report| report.cycle_complete) {
                        tokio::select! {
                            () = cancel.cancelled() => return,
                            () = tokio::time::sleep(scan_interval) => {}
                        }
                    }
                }
            }
        }
    }

    fn advance(&mut self, sequence: u64) {
        self.next_sequence = sequence.checked_add(1);
    }

    fn finish_cycle(&mut self) -> bool {
        let clean = !self.cycle_faulted;
        self.cycle_end = None;
        self.next_sequence = None;
        self.cycle_faulted = false;
        clean
    }

    fn mark_failed(&mut self, failure: CopyCleanerFailure) -> CopyCleanerFailure {
        self.cycle_faulted = true;
        tracing::warn!(
            failure_category = ?failure,
            "JetStream copy cleaner scan failed; current position retained"
        );
        failure
    }

    async fn stream_info(&self) -> Result<async_nats::jetstream::stream::Info, CopyCleanerFailure> {
        match tokio::time::timeout(self.config.request_timeout, self.stream.get_info()).await {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(_)) => Err(CopyCleanerFailure::StreamInfo),
            Err(_) => Err(CopyCleanerFailure::RequestTimeout),
        }
    }

    async fn next_message(
        &self,
        sequence: u64,
    ) -> Result<Option<async_nats::jetstream::message::StreamMessage>, CopyCleanerFailure> {
        match tokio::time::timeout(
            self.config.request_timeout,
            self.stream.get_first_raw_message_by_subject(">", sequence),
        )
        .await
        {
            Ok(Ok(message)) => Ok(Some(message)),
            Ok(Err(error))
                if matches!(
                    error.kind(),
                    async_nats::jetstream::stream::RawMessageErrorKind::NoMessageFound
                ) =>
            {
                Ok(None)
            }
            Ok(Err(_)) => Err(CopyCleanerFailure::StreamRead),
            Err(_) => Err(CopyCleanerFailure::RequestTimeout),
        }
    }

    async fn check_eligibility(
        &self,
        owner: OwnerRef,
        fact: MemoryId,
    ) -> Result<PublicationOriginEligibility, CopyCleanerFailure> {
        match tokio::time::timeout(
            self.config.request_timeout,
            self.eligibility.check_committed(owner, fact),
        )
        .await
        {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(_)) => Err(CopyCleanerFailure::OriginCheck),
            Err(_) => Err(CopyCleanerFailure::RequestTimeout),
        }
    }

    async fn delete_message(&self, sequence: u64) -> Result<bool, CopyCleanerFailure> {
        match tokio::time::timeout(
            self.config.request_timeout,
            self.stream.delete_message(sequence),
        )
        .await
        {
            Ok(Ok(deleted)) => Ok(deleted),
            Ok(Err(_)) => Err(CopyCleanerFailure::Delete),
            Err(_) => Err(CopyCleanerFailure::RequestTimeout),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CopyCleanerConnectError {
    #[error("cleaner broker connection failed")]
    Connect,
    #[error("canonical cleaner stream unavailable")]
    Stream,
}

struct MessageIdentity {
    fact: MemoryId,
    owner: OwnerRef,
}

/// Why a message yielded no usable identity.
///
/// The two are not degrees of the same thing. [`Self::Unknown`] is a
/// statement about ONE message and the next one may be fine;
/// [`Self::ForeignScope`] is a statement about the STREAM, and the next
/// message cannot be fine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MessageVerdict {
    Unknown,
    ForeignScope,
}

/// Prove a message is canonical Proxima output AND this installation's.
///
/// Everything below the scope check is structural: it establishes that
/// some Proxima produced these bytes, which every Proxima's output
/// satisfies. Only the stamp says WHICH one, and that is the half the
/// deletion rule actually rests on — "no origin row" is a revocation only
/// over messages this database published.
///
/// An unstamped message is [`MessageVerdict::Unknown`], not foreign. A
/// stream carrying events from a release that published before stamping
/// existed is the ordinary upgrade case, and it must not halt cleaning of
/// the stamped messages beside them.
fn message_identity(
    subject: &str,
    headers: &async_nats::HeaderMap,
    payload: &[u8],
    expected_scope: OriginScope,
) -> Result<MessageIdentity, MessageVerdict> {
    use MessageVerdict::Unknown;

    let mut buffer = uuid::Uuid::encode_buffer();
    let expected = expected_scope
        .into_inner()
        .as_hyphenated()
        .encode_lower(&mut buffer);
    match headers.get(HEADER_ORIGIN_SCOPE) {
        None => return Err(Unknown),
        Some(stamp) if stamp.as_str() == expected => {}
        Some(_) => return Err(MessageVerdict::ForeignScope),
    }
    let envelope: CloudEventEnvelope = serde_json::from_slice(payload).map_err(|_| Unknown)?;
    if envelope.specversion != "1.0" || envelope.event_type.trim().is_empty() {
        return Err(Unknown);
    }
    let event_id =
        parse_prefixed_uuid(&envelope.id, PrefixedUuidClass::Fact).map_err(|_| Unknown)?;
    if envelope.id != format_prefixed_uuid(event_id, PrefixedUuidClass::Fact) {
        return Err(Unknown);
    }
    let message_id = headers.get(HEADER_MSG_ID).ok_or(Unknown)?.as_str();
    if message_id != envelope.id {
        return Err(Unknown);
    }
    let content_type = headers.get(HEADER_CONTENT_TYPE).ok_or(Unknown)?.as_str();
    if content_type != CONTENT_TYPE_CLOUDEVENTS {
        return Err(Unknown);
    }
    let raw_owner = envelope.proximaowner.as_deref().ok_or(Unknown)?;
    let owner = parse_external_key(raw_owner).map_err(|_| Unknown)?;
    if owner.external_key() != raw_owner {
        return Err(Unknown);
    }
    let (kind, owner_id) = owner.columns();
    let expected_subject = crate::subject_for(
        COPY_CLEANER_SUBJECT_PREFIX,
        kind.as_str(),
        owner_id,
        &envelope.event_type,
    );
    if subject != expected_subject {
        return Err(Unknown);
    }
    Ok(MessageIdentity {
        fact: MemoryId::new(event_id),
        owner,
    })
}

/// Start the independently owned cleaner and return its read-only health
/// reader plus its ordinary join handle.
///
/// `origin_scope` must have been read from the database backing
/// `eligibility`; see [`JetStreamCopyCleaner::connect`].
#[must_use]
pub fn spawn_supervised_copy_cleaner(
    config: JetStreamCopyCleanerConfig,
    eligibility: Arc<dyn PublicationOriginEligibilityPort>,
    origin_scope: OriginScope,
    cancel: CancellationToken,
) -> (CopyCleanerHealthReader, JoinHandle<()>) {
    let inner = Arc::new(Mutex::new(CopyCleanerHealthInner::default()));
    let reader = CopyCleanerHealthReader {
        inner: inner.clone(),
    };
    // Capture the guard in the task future before it is first polled. Dropping
    // or aborting that future unpolled must still release its health slot.
    let guard = CopyCleanerTaskGuard {
        inner: inner.clone(),
    };
    let task = tokio::spawn(async move {
        guard.update(|inner| inner.task = CopyCleanerTaskState::Running);
        let run = async {
            loop {
                if cancel.is_cancelled() {
                    break;
                }
                match JetStreamCopyCleaner::connect(
                    config.clone(),
                    eligibility.clone(),
                    origin_scope,
                )
                .await
                {
                    Ok(cleaner) => {
                        guard.attach(cleaner.client.clone());
                        run_connected(cleaner, cancel.clone(), &guard).await;
                        if cancel.is_cancelled() {
                            break;
                        }
                        guard.disconnected();
                    }
                    Err(CopyCleanerConnectError::Connect | CopyCleanerConnectError::Stream) => {
                        guard.fail(CopyCleanerFailure::Connect);
                        tracing::warn!(
                            failure_category = "connect",
                            "JetStream copy cleaner could not bind the canonical stream"
                        );
                    }
                }
                tokio::select! {
                    () = cancel.cancelled() => break,
                    () = tokio::time::sleep(RECONNECT_DELAY) => {}
                }
            }
        };
        if std::panic::AssertUnwindSafe(run)
            .catch_unwind()
            .await
            .is_err()
        {
            guard.fail(CopyCleanerFailure::StreamRead);
            tracing::error!(failure = "panic", "JetStream copy cleaner task stopped");
        }
    });
    (reader, task)
}

async fn run_connected(
    mut cleaner: JetStreamCopyCleaner,
    cancel: CancellationToken,
    health: &CopyCleanerTaskGuard,
) {
    loop {
        if cancel.is_cancelled() {
            return;
        }
        let shutdown_grace = cleaner.config.shutdown_grace;
        let scan_interval = cleaner.config.scan_interval;
        let slice = cleaner.scan_slice();
        tokio::pin!(slice);
        tokio::select! {
            () = cancel.cancelled() => {
                let _ = tokio::time::timeout(shutdown_grace, &mut slice).await;
                return;
            }
            result = &mut slice => {
                match result {
                    Ok(report) => {
                        let cycle_complete = report.cycle_complete;
                        health.observe_slice(&report);
                        if cycle_complete {
                            tokio::select! {
                                () = cancel.cancelled() => return,
                                () = tokio::time::sleep(scan_interval) => {}
                            }
                        }
                    }
                    Err(failure) => {
                        health.fail(failure);
                        tokio::select! {
                            () = cancel.cancelled() => return,
                            () = tokio::time::sleep(scan_interval) => {}
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proxima_core::StorageError;

    #[test]
    fn cleaner_config_rejects_an_unbounded_zero_duration() {
        let items = NonZeroU32::new(2).expect("positive item limit");
        let config = JetStreamCopyCleanerConfig::new("nats://127.0.0.1:4222").bounds(
            items,
            Duration::from_secs(1),
            Duration::from_secs(1),
            Duration::ZERO,
            Duration::from_secs(1),
        );
        assert!(matches!(config, Err(CleanerConfigError::ZeroDuration)));
    }

    #[test]
    fn cleaner_config_redacts_credentials_from_debug() {
        let config = JetStreamCopyCleanerConfig::new(
            "user:secret@127.0.0.1:4222,nats://backup:second-secret@backup:4222",
        )
        .auth(NatsAuth::UserPassword {
            user: "cleaner".to_owned(),
            password: "secret".to_owned(),
        });
        let debug = format!("{config:?}");
        assert!(!debug.contains("secret"));
        assert!(debug.contains("<redacted>"));
        assert!(debug.contains("127.0.0.1:4222"));
        assert!(debug.contains("backup:4222"));
    }

    /// One canonical message, stamped by `scope`, and the subject it must
    /// have been published on.
    fn canonical_message(
        scope: Option<OriginScope>,
    ) -> (String, async_nats::HeaderMap, Vec<u8>, uuid::Uuid, OwnerRef) {
        let id = uuid::Uuid::now_v7();
        let owner = OwnerRef::Personal(proxima_core::UserId::new(uuid::Uuid::now_v7()));
        let event = serde_json::json!({
            "specversion": "1.0",
            "id": format_prefixed_uuid(id, PrefixedUuidClass::Fact),
            "source": "urn:proxima:test",
            "type": "fixture/listenable-v1",
            "proximaowner": owner.external_key(),
            "data": {"ok": true}
        });
        let subject = crate::subject_for(
            COPY_CLEANER_SUBJECT_PREFIX,
            owner.columns().0.as_str(),
            owner.stored_owner_id(),
            "fixture/listenable-v1",
        );
        let mut headers = async_nats::HeaderMap::new();
        headers.insert(
            HEADER_MSG_ID,
            format_prefixed_uuid(id, PrefixedUuidClass::Fact),
        );
        headers.insert(HEADER_CONTENT_TYPE, CONTENT_TYPE_CLOUDEVENTS);
        if let Some(scope) = scope {
            headers.insert(HEADER_ORIGIN_SCOPE, scope.to_string().as_str());
        }
        let payload = serde_json::to_vec(&event).expect("the event serializes");
        (subject, headers, payload, id, owner)
    }

    #[test]
    fn canonical_fact_id_and_original_owner_are_required() {
        let scope = OriginScope::new(uuid::Uuid::now_v7());
        let (subject, headers, payload, id, owner) = canonical_message(Some(scope));
        let identity = message_identity(&subject, &headers, &payload, scope)
            .expect("canonical capture identity");
        assert_eq!(identity.fact, MemoryId::new(id));
        assert_eq!(identity.owner, owner);

        let mut event: serde_json::Value =
            serde_json::from_slice(&payload).expect("the event parses");
        event["proximaowner"] = serde_json::Value::String("Personal:invalid".to_owned());
        let bad_owner_payload = serde_json::to_vec(&event).expect("the modified event serializes");
        assert_eq!(
            message_identity(&subject, &headers, &bad_owner_payload, scope).err(),
            Some(MessageVerdict::Unknown)
        );
    }

    /// The check the deletion rule rests on: a message is only this
    /// installation's to reason about if it says so.
    ///
    /// All three messages below are canonical Proxima output and would have
    /// passed every structural check. Nothing but the stamp separates the
    /// one that may be deleted from the two that may not.
    #[test]
    fn only_a_message_this_installation_stamped_yields_an_identity() {
        let scope = OriginScope::new(uuid::Uuid::now_v7());
        let foreign = OriginScope::new(uuid::Uuid::now_v7());

        let (subject, headers, payload, _, _) = canonical_message(Some(scope));
        assert!(message_identity(&subject, &headers, &payload, scope).is_ok());

        // Another installation's copy. Its origin rows live in a database
        // this cleaner never queries, so an absent row here proves nothing
        // — and the whole stream is suspect, not just this message.
        let (subject, headers, payload, _, _) = canonical_message(Some(foreign));
        assert_eq!(
            message_identity(&subject, &headers, &payload, scope).err(),
            Some(MessageVerdict::ForeignScope)
        );

        // Published before this installation stamped: the ordinary upgrade
        // case. Unattributable, so retained — but a skip, not a halt, or a
        // single pre-upgrade message would block cleaning forever.
        let (subject, headers, payload, _, _) = canonical_message(None);
        assert_eq!(
            message_identity(&subject, &headers, &payload, scope).err(),
            Some(MessageVerdict::Unknown)
        );
    }

    #[tokio::test]
    async fn abort_before_first_poll_releases_the_cleaner_health_slot() {
        let config = JetStreamCopyCleanerConfig::new("nats://127.0.0.1:4222");
        let cancel = CancellationToken::new();
        let (health, task) = spawn_supervised_copy_cleaner(
            config,
            Arc::new(EligibleOrigin),
            OriginScope::new(uuid::Uuid::now_v7()),
            cancel,
        );
        assert_eq!(health.snapshot().task, CopyCleanerTaskState::Starting);
        task.abort();
        let aborted = task.await.expect_err("the unpolled task is aborted");
        assert!(aborted.is_cancelled());
        let stopped = health.snapshot();
        assert_eq!(stopped.task, CopyCleanerTaskState::Stopped);
        assert_eq!(stopped.connection, CopyCleanerConnectionState::NotObserved);
    }

    struct EligibleOrigin;

    #[async_trait::async_trait]
    impl PublicationOriginEligibilityPort for EligibleOrigin {
        async fn check_committed(
            &self,
            _original_owner: OwnerRef,
            _fact_id: MemoryId,
        ) -> Result<PublicationOriginEligibility, StorageError> {
            Ok(PublicationOriginEligibility::Eligible)
        }

        async fn check_in_transaction(
            &self,
            _tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
            _original_owner: OwnerRef,
            _fact_id: MemoryId,
        ) -> Result<PublicationOriginEligibility, StorageError> {
            Ok(PublicationOriginEligibility::Eligible)
        }
    }
}
