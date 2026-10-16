//! The outbox drains: each host-provided [`DurableIntake`], registered under a
//! consumer name, receives every captured Fact in this process (docs/host-integration.md
//! §Delivery, §Consumers).
//!
//! One loop per consumer claims a batch of that consumer's lanes, calls
//! [`DurableIntake::accept`] once per record, and then marks the lane
//! delivered or hands its claim back. Drains share nothing but the record: a
//! consumer that fails or lags holds only its own lanes, and a record is
//! published once every consumer's lane is.
//!
//! - **Outcomes.** `accept` returning `Ok` (`Accepted` or `Rejected`) is a
//!   decision the intake made durable, so the record is marked published. An
//!   [`IntakeError`](proxima_core::publication::intake::IntakeError), an
//!   `accept` that outlives its timeout, or captured bytes that do not parse
//!   release the claim. The only way a lane leaves `pending`/`claimed` is
//!   `PublicationOutboxPort::mark_published`, fenced on the claim token.
//! - **Per-record isolation.** A record that fails is released and the pass
//!   carries on with the rest of the batch. A pass reports a failure only when
//!   nothing at all got through.
//! - **Hand-back.** Shutdown and a quiesce release the record in flight and
//!   every claim not yet attempted, so no lease has to expire for work nobody
//!   is doing.
//! - **Pass backoff.** A pass that delivered nothing doubles the wait before
//!   the next claim, from the poll interval up to 30 s.
//!
//! Delivery is at-least-once: a process that stops between `accept` and the
//! mark redelivers the record when its lease expires, with a higher
//! `delivered_count`, and the intake deduplicates on the event id.

use std::future::Future;
use std::num::NonZeroU32;
use std::ops::ControlFlow;
use std::sync::Arc;
use std::time::Duration;

use proxima_core::QuiesceGate;
use proxima_core::publication::intake::{DurableIntake, ReceivedEvent};
use proxima_core::quiesce::{Checkpoint, GateWorker};
use proxima_core::storage_ports::publication::{
    AckOutcome, ClaimedPublication, ConsumerName, PublicationOutboxPort, PublisherId,
    ReleaseOutcome,
};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// The longest a failing drain waits between passes.
const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// How one drain claims and delivers.
#[derive(Debug, Clone)]
pub(crate) struct Settings {
    /// Names this drain on every claim it holds.
    pub(crate) publisher_id: PublisherId,
    /// Records leased per pass.
    pub(crate) batch: NonZeroU32,
    /// How long a claim holds a record before it becomes deliverable again.
    /// The storage port refuses a lease under one second.
    pub(crate) lease: Duration,
    /// How long an idle drain waits before it claims again; also the floor of
    /// the backoff after a failed pass.
    pub(crate) poll_interval: Duration,
    /// How long one `accept` may take before its claim is released.
    pub(crate) accept_timeout: Duration,
}

impl Settings {
    /// The shipped values. The label is `proxima:<pid>`: an operator's hint
    /// for who holds a claim, not an identity (the claim token fences).
    pub(crate) fn new() -> Self {
        Self {
            publisher_id: PublisherId::new(format!("proxima:{}", std::process::id()))
                .expect("`proxima:<pid>` is a valid publisher label"),
            batch: NonZeroU32::new(64).expect("64 is not zero"),
            lease: Duration::from_secs(30),
            poll_interval: Duration::from_millis(500),
            accept_timeout: Duration::from_secs(5),
        }
    }
}

/// Seam between "the intake accepted it" and "the outbox knows".
///
/// It exists because that window is the one this design cannot test any other
/// way: a crash after `accept` and before the mark is the failure the whole
/// delivery contract is built to survive, and it lives inside a single
/// `await` a test cannot otherwise interrupt. It is a real, always-compiled
/// dispatch rather than a `cfg(test)` branch: a fault path production never
/// runs is a fault path production never checked.
#[async_trait::async_trait]
pub(crate) trait AfterAccept: Send + Sync + std::fmt::Debug {
    /// Called with the event id once `accept` returned, before the record is
    /// marked published. `Break` behaves as though the process died here: the
    /// pass is abandoned at once, leaving this record and every unattempted
    /// claim leased.
    async fn after_accept(&self, event_id: &str) -> ControlFlow<()>;
}

/// The shipped hook: carry on.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Proceed;

#[async_trait::async_trait]
impl AfterAccept for Proceed {
    async fn after_accept(&self, _event_id: &str) -> ControlFlow<()> {
        ControlFlow::Continue(())
    }
}

/// Why one record, or a whole pass, delivered nothing. Only the category is
/// logged: the intake's own words stay in the intake.
#[derive(Debug, Clone, Copy)]
enum Failure {
    /// The outbox could not claim, mark or release.
    Storage,
    /// The captured bytes are not a `CloudEvents` envelope.
    Malformed,
    /// The intake could not make an outcome durable.
    Intake,
    /// `accept` did not return within the timeout.
    TimedOut,
}

/// What became of one record that did not fail.
enum Flow {
    /// Marked published, by this claim or an earlier one.
    Delivered,
    /// The claim had moved on: another drain owns the record now.
    Stale,
    /// The hook asked to behave as though the process died here.
    Abort,
    /// Shutdown or a quiesce: hand back everything this pass still holds.
    HandBack,
}

/// The claim → accept → mark loop over one consumer's lanes and its intake.
pub(crate) struct Drain {
    outbox: Arc<dyn PublicationOutboxPort>,
    consumer: ConsumerName,
    intake: Arc<dyn DurableIntake>,
    settings: Settings,
    hook: Arc<dyn AfterAccept>,
}

impl Drain {
    pub(crate) fn new(
        outbox: Arc<dyn PublicationOutboxPort>,
        consumer: ConsumerName,
        intake: Arc<dyn DurableIntake>,
        settings: Settings,
        hook: Arc<dyn AfterAccept>,
    ) -> Self {
        Self {
            outbox,
            consumer,
            intake,
            settings,
            hook,
        }
    }

    /// Drain until cancelled, backing off while the intake is unhappy.
    ///
    /// Never panics and never returns an error: a drain that stopped on an
    /// intake outage would turn a recoverable delivery pause into a silent
    /// permanent one, and capture keeps running either way.
    pub(crate) async fn run(&self, cancel: &CancellationToken, mut gate: GateWorker) {
        let mut backoff = self.settings.poll_interval;
        loop {
            // Before every claim: a quiesced drain parks here, holding
            // nothing, until it is resumed or cancelled.
            if gate.checkpoint(cancel.cancelled()).await == Checkpoint::Cancelled {
                return;
            }
            let idle = match self.pass(cancel, &gate).await {
                Ok(claimed) => {
                    backoff = self.settings.poll_interval;
                    // A pass that claimed anything is a pass with more
                    // waiting; only an empty claim earns the sleep.
                    claimed == 0
                }
                Err(failure) => {
                    tracing::warn!(
                        consumer = %self.consumer,
                        failure_category = ?failure,
                        "in-process outbox delivery failed; the records stay pending and are retried"
                    );
                    backoff = next_backoff(backoff, self.settings.poll_interval);
                    true
                }
            };
            if idle {
                tokio::select! {
                    () = cancel.cancelled() => return,
                    () = gate.changed() => {}
                    () = tokio::time::sleep(backoff) => {}
                }
            }
        }
    }

    /// One claim → accept → mark pass; the number of records it claimed.
    ///
    /// A quiesce lets the record in flight finish: it is delivered and
    /// marked, or fails, like any other. Only the records not yet started are
    /// released, so settling costs one `accept`, not a batch of them.
    /// Shutdown also abandons the `accept` in flight.
    ///
    /// Failures are ISOLATED. One record the intake will never take would
    /// otherwise be re-claimed at the head of every pass, and every later Fact
    /// would stall behind it until the outbox filled and listenable writes
    /// started failing. Its claim goes straight back and the pass continues;
    /// the port's `attempts ASC` ordering demotes it behind fresher work on
    /// the next claim, so it costs one slot per pass.
    ///
    /// # Errors
    ///
    /// The first failure, when NO record got through, so an error means the
    /// intake or the outbox, not one envelope, is the problem.
    async fn pass(&self, cancel: &CancellationToken, gate: &GateWorker) -> Result<usize, Failure> {
        let claimed = self
            .outbox
            .claim(
                &self.consumer,
                &self.settings.publisher_id,
                self.settings.batch,
                self.settings.lease,
            )
            .await
            .map_err(|_| Failure::Storage)?;
        let count = claimed.len();
        let mut delivered = 0usize;
        let mut first_failure: Option<Failure> = None;
        let mut records = claimed.into_iter();
        while let Some(record) = records.next() {
            let flow = if gate.is_closing() {
                Ok(Flow::HandBack)
            } else {
                self.deliver(&record, cancel).await
            };
            match flow {
                Ok(Flow::Delivered) => delivered += 1,
                Ok(Flow::Stale) => {}
                Ok(Flow::Abort) => return Ok(count),
                Ok(Flow::HandBack) => {
                    // Not a failure: hand back everything this pass still
                    // holds so the next process does not wait out a lease for
                    // work nobody is doing.
                    self.release(&record).await;
                    for rest in records {
                        self.release(&rest).await;
                    }
                    return Ok(count);
                }
                Err(failure) => {
                    self.release(&record).await;
                    first_failure.get_or_insert(failure);
                }
            }
        }
        match first_failure {
            // Nothing got through at all: report it, so `run` backs off
            // rather than spinning on a dead intake.
            Some(failure) if delivered == 0 => Err(failure),
            _ => Ok(count),
        }
    }

    /// Hand one claimed record to the intake and record what it said.
    ///
    /// Cancellation is observed BETWEEN records and inside the `accept`
    /// itself, so a shutdown costs one storage round trip rather than
    /// `batch × accept timeout`.
    async fn deliver(
        &self,
        record: &ClaimedPublication,
        cancel: &CancellationToken,
    ) -> Result<Flow, Failure> {
        if cancel.is_cancelled() {
            return Ok(Flow::HandBack);
        }
        // Parsed here, once. Only the category of the parse error is logged:
        // its text can quote the payload.
        let event = ReceivedEvent::from_record(record).map_err(|error| {
            tracing::error!(
                consumer = %self.consumer,
                event_id = %record.event_id,
                parse_error = ?error.classify(),
                "a captured envelope is not a CloudEvents envelope; the record is released \
                 and stays undelivered"
            );
            Failure::Malformed
        })?;
        let accepted = tokio::select! {
            () = cancel.cancelled() => return Ok(Flow::HandBack),
            accepted = tokio::time::timeout(
                self.settings.accept_timeout,
                self.intake.accept(&event),
            ) => accepted,
        };
        match accepted {
            // `Accepted` and `Rejected` are both outcomes the intake made
            // durable.
            Ok(Ok(_decision)) => {}
            Ok(Err(_)) => return Err(Failure::Intake),
            Err(_) => return Err(Failure::TimedOut),
        }
        if self.hook.after_accept(&record.event_id).await.is_break() {
            return Ok(Flow::Abort);
        }
        match self
            .outbox
            .mark_published(&self.consumer, record.id, record.claim)
            .await
            .map_err(|_| Failure::Storage)?
        {
            AckOutcome::Published | AckOutcome::AlreadyPublished => Ok(Flow::Delivered),
            AckOutcome::StaleClaim => {
                tracing::info!(
                    consumer = %self.consumer,
                    event_id = %record.event_id,
                    "the mark was fenced out by a newer claim"
                );
                Ok(Flow::Stale)
            }
        }
    }

    /// Hand one claim back, best effort. A failed release is not an error:
    /// the lease expires on its own, which is the whole reason releases are an
    /// optimization rather than a correctness requirement.
    async fn release(&self, record: &ClaimedPublication) {
        match self
            .outbox
            .release(&self.consumer, record.id, record.claim)
            .await
        {
            Ok(
                ReleaseOutcome::Released
                | ReleaseOutcome::AlreadyPublished
                | ReleaseOutcome::StaleClaim,
            ) => {}
            Err(_) => {
                tracing::warn!(
                    consumer = %self.consumer,
                    failure_category = ?Failure::Storage,
                    "releasing a claim failed; the lease expiry still returns it"
                );
            }
        }
    }
}

/// Doubling backoff, capped so a long outage still polls often enough to
/// notice recovery within a few seconds.
///
/// `min` then `max`, never `clamp`: `Ord::clamp` PANICS when `min > max`, and
/// a floor above the ceiling must not kill the drain task and leave the
/// outbox undrained for the life of the process. A floor above the ceiling
/// wins.
fn next_backoff(current: Duration, floor: Duration) -> Duration {
    current.saturating_mul(2).min(MAX_BACKOFF).max(floor)
}

/// Spawn one task per drain beside `housekeeping` (the retained-record
/// prune), each under its own pause gate, returned in `drains` order.
///
/// Each drain runs as its own task so a consumer whose `accept` blocks holds
/// up only itself. `housekeeping` starts at once; a quiesce does not pause
/// it, because it claims no queue row. The returned handle finishes when all
/// of them have, and aborting it aborts every drain.
pub(crate) fn spawn<F, Fut>(
    drains: Vec<Drain>,
    cancel: CancellationToken,
    housekeeping: F,
) -> (Vec<QuiesceGate>, JoinHandle<()>)
where
    F: FnOnce(CancellationToken) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let (gates, workers): (Vec<_>, Vec<_>) = drains
        .into_iter()
        .map(|drain| {
            let (gate, worker) = QuiesceGate::new();
            (gate, (drain, worker))
        })
        .unzip();
    let task = tokio::spawn(async move {
        let mut running = tokio::task::JoinSet::new();
        for (drain, worker) in workers {
            let cancel = cancel.clone();
            running.spawn(async move { drain.run(&cancel, worker).await });
        }
        let housekeeping = housekeeping(cancel.clone());
        let drains = async move { while running.join_next().await.is_some() {} };
        tokio::join!(drains, housekeeping);
    });
    (gates, task)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_backoff_doubles_to_the_ceiling_and_a_higher_floor_wins() {
        let poll = Duration::from_millis(500);
        assert_eq!(next_backoff(poll, poll), Duration::from_secs(1));
        assert_eq!(
            next_backoff(Duration::from_secs(20), poll),
            Duration::from_secs(30)
        );
        let slow = Duration::from_mins(1);
        assert_eq!(
            next_backoff(slow, slow),
            slow,
            "a floor over the ceiling wins"
        );
    }
}

#[cfg(test)]
#[path = "in_process_outbox_tests.rs"]
mod in_process_outbox_tests;
