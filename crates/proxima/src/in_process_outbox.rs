//! The outbox drains: each host-provided [`DurableIntake`], registered under a
//! consumer name, receives every captured Fact in this process (docs/host-integration.md
//! §Delivery, §Consumers).
//!
//! One loop per consumer claims a batch of that consumer's lanes, calls
//! [`DurableIntake::accept`] once per record, marks the lanes it took
//! delivered in one transaction per batch, and hands the claim of a record it
//! did not take back. Drains share nothing but the record: a
//! consumer that fails or lags holds only its own lanes, and a record is
//! published once every consumer's lane is.
//!
//! - **Outcomes.** `accept` returning `Ok` (`Accepted` or `Rejected`) is a
//!   decision the intake made durable, so the record is marked published. An
//!   [`IntakeError`](proxima_core::publication::intake::IntakeError), an
//!   `accept` that outlives its timeout, or captured bytes that do not parse
//!   release the claim. Marks wait for the end of the batch, or half the lease,
//!   whichever comes first. The only way a lane leaves `pending`/`claimed` is
//!   `PublicationOutboxPort::mark_published_batch`, fenced on the claim tokens.
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
use proxima_core::write_wake::WriteWake;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// The longest a failing drain waits between passes, and the shipped
/// [`Settings::idle_poll_max`].
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
    /// How long a drain waits after its first empty claim; also the floor of
    /// the backoff after a failed pass. Each further empty claim doubles the
    /// wait, up to `idle_poll_max`; a committed write or a claim that finds
    /// work resets it.
    pub(crate) poll_interval: Duration,
    /// The longest an idle drain sleeps between claims when no write commits
    /// in this process: how late it finds work enqueued by another process.
    pub(crate) idle_poll_max: Duration,
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
            // Crate tests compile this with `cfg(test)` and must not sit
            // out the shipped 500 ms backoff between failed accepts.
            poll_interval: if cfg!(test) {
                Duration::from_millis(50)
            } else {
                Duration::from_millis(500)
            },
            idle_poll_max: MAX_BACKOFF,
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
    /// The intake made an outcome durable; the mark is still owed.
    Accepted,
    /// The claim had moved on: another drain owns the record now.
    Stale,
    /// The hook asked to behave as though the process died here.
    Abort,
    /// Shutdown or a quiesce: hand back everything this pass still holds.
    HandBack,
}

/// Records the intake took in this pass whose mark is still owed, and when
/// the oldest was taken.
#[derive(Default)]
struct Accepted<'a> {
    records: Vec<&'a ClaimedPublication>,
    since: Option<tokio::time::Instant>,
}

impl<'a> Accepted<'a> {
    fn push(&mut self, record: &'a ClaimedPublication) {
        self.since.get_or_insert_with(tokio::time::Instant::now);
        self.records.push(record);
    }

    fn age(&self) -> Duration {
        self.since.map_or(Duration::ZERO, |since| since.elapsed())
    }

    fn take(&mut self) -> Vec<&'a ClaimedPublication> {
        self.since = None;
        std::mem::take(&mut self.records)
    }
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

    /// Drain until cancelled: claim again at once while claims return work
    /// or a write commits in this process, back off while claims come back
    /// empty, and back off while the intake is unhappy.
    ///
    /// Never panics and never returns an error: a drain that stopped on an
    /// intake outage would turn a recoverable delivery pause into a silent
    /// permanent one, and capture keeps running either way.
    pub(crate) async fn run(&self, cancel: &CancellationToken, mut gate: GateWorker) {
        // Subscribed before the first claim, so a write that commits while a
        // pass runs wakes the drain right after it.
        let mut writes = WriteWake::subscribe();
        let floor = self.settings.poll_interval;
        let mut idle_wait = floor;
        let mut failure_wait = floor;
        loop {
            // Before every claim: a quiesced drain parks here, holding
            // nothing, until it is resumed or cancelled.
            if gate.checkpoint(cancel.cancelled()).await == Checkpoint::Cancelled {
                return;
            }
            match self.pass(cancel, &gate).await {
                // A pass that claimed anything is a pass with more waiting.
                Ok(claimed) if claimed > 0 => {
                    idle_wait = floor;
                    failure_wait = floor;
                }
                // An empty claim: sleep, longer each time nothing turns up,
                // until a write commits here. Writes committed by another
                // process are found by the poll, within `idle_poll_max`.
                Ok(_) => {
                    failure_wait = floor;
                    let sleep = idle_wait;
                    idle_wait = next_backoff(idle_wait, floor, self.settings.idle_poll_max);
                    tokio::select! {
                        () = cancel.cancelled() => return,
                        () = gate.changed() => {}
                        () = writes.changed() => idle_wait = floor,
                        () = tokio::time::sleep(sleep) => {}
                    }
                }
                // A failing intake is not hurried by new writes.
                Err(failure) => {
                    tracing::warn!(
                        consumer = %self.consumer,
                        failure_category = ?failure,
                        "in-process outbox delivery failed; the records stay pending and are retried"
                    );
                    idle_wait = floor;
                    failure_wait = next_backoff(failure_wait, floor, MAX_BACKOFF);
                    tokio::select! {
                        () = cancel.cancelled() => return,
                        () = gate.changed() => {}
                        () = tokio::time::sleep(failure_wait) => {}
                    }
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
        let mut accepted = Accepted::default();
        let mut records = claimed.iter();
        while let Some(record) = records.next() {
            let flow = if gate.is_closing() {
                Ok(Flow::HandBack)
            } else {
                self.deliver(record, cancel).await
            };
            match flow {
                Ok(Flow::Accepted) => {
                    accepted.push(record);
                    // The lease the renew gave this claim runs out whatever
                    // the rest of the batch costs, so marks do not wait past
                    // half of it.
                    if accepted.age() >= self.settings.lease / 2 {
                        self.mark(&mut accepted, &mut delivered, &mut first_failure)
                            .await;
                    }
                }
                Ok(Flow::Stale) => {}
                // As though the process died here: what is accepted and not
                // yet marked stays leased, as `after_accept` promises.
                Ok(Flow::Abort) => return Ok(count),
                Ok(Flow::HandBack) => {
                    // Not a failure: what the intake already took is marked,
                    // and everything else this pass holds goes back so the
                    // next process does not wait out a lease for work nobody
                    // is doing.
                    self.mark(&mut accepted, &mut delivered, &mut first_failure)
                        .await;
                    self.release(record).await;
                    for rest in records {
                        self.release(rest).await;
                    }
                    return Ok(count);
                }
                Err(failure) => {
                    self.release(record).await;
                    first_failure.get_or_insert(failure);
                }
            }
        }
        self.mark(&mut accepted, &mut delivered, &mut first_failure)
            .await;
        match first_failure {
            // Nothing got through at all: report it, so `run` backs off
            // rather than spinning on a dead intake.
            Some(failure) if delivered == 0 => Err(failure),
            _ => Ok(count),
        }
    }

    /// Mark every accepted record published in one transaction and leave
    /// `accepted` empty. A failed mark leaves the claims to expire.
    async fn mark(
        &self,
        accepted: &mut Accepted<'_>,
        delivered: &mut usize,
        first_failure: &mut Option<Failure>,
    ) {
        let records = accepted.take();
        if records.is_empty() {
            return;
        }
        let claims: Vec<_> = records
            .iter()
            .map(|record| (record.id, record.claim))
            .collect();
        match self
            .outbox
            .mark_published_batch(&self.consumer, &claims)
            .await
        {
            Ok(outcomes) => {
                for (record, outcome) in records.iter().zip(outcomes) {
                    match outcome {
                        AckOutcome::Published | AckOutcome::AlreadyPublished => *delivered += 1,
                        AckOutcome::StaleClaim => tracing::info!(
                            consumer = %self.consumer,
                            event_id = %record.event_id,
                            "the mark was fenced out by a newer claim"
                        ),
                    }
                }
            }
            Err(_) => {
                first_failure.get_or_insert(Failure::Storage);
            }
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
        // Re-check the claim right before the hand-over: a lease that expired
        // while this pass was busy may belong to another drain now, and the
        // record may have been erased since the claim. Either way it is not
        // ours to deliver; it is neither marked nor released.
        let still_ours = self
            .outbox
            .renew_claim(&self.consumer, record.id, record.claim, self.settings.lease)
            .await
            .map_err(|_| Failure::Storage)?;
        if !still_ours {
            tracing::debug!(
                consumer = %self.consumer,
                skipped = 1u32,
                "a claimed record is no longer deliverable under this claim; skipped"
            );
            return Ok(Flow::Stale);
        }
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
        Ok(Flow::Accepted)
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
fn next_backoff(current: Duration, floor: Duration, ceiling: Duration) -> Duration {
    current.saturating_mul(2).min(ceiling).max(floor)
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
    use proxima_core::publication::intake::{Intake, IntakeError};
    use proxima_core::storage_ports::publication::ClaimToken;
    use proxima_core::write_wake::write_committed;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use uuid::Uuid;

    /// An outbox that never has anything to deliver; counts its claims.
    #[derive(Debug, Default)]
    struct EmptyOutbox {
        claims: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl PublicationOutboxPort for EmptyOutbox {
        async fn claim(
            &self,
            _consumer: &ConsumerName,
            _publisher: &PublisherId,
            _limit: NonZeroU32,
            _lease: Duration,
        ) -> Result<Vec<ClaimedPublication>, proxima_core::StorageError> {
            self.claims.fetch_add(1, Ordering::SeqCst);
            Ok(Vec::new())
        }

        async fn mark_published_batch(
            &self,
            _consumer: &ConsumerName,
            _claims: &[(Uuid, ClaimToken)],
        ) -> Result<Vec<AckOutcome>, proxima_core::StorageError> {
            unreachable!("nothing is claimed")
        }

        async fn renew_claim(
            &self,
            _consumer: &ConsumerName,
            _id: Uuid,
            _claim: ClaimToken,
            _lease: Duration,
        ) -> Result<bool, proxima_core::StorageError> {
            unreachable!("nothing is claimed")
        }

        async fn release(
            &self,
            _consumer: &ConsumerName,
            _id: Uuid,
            _claim: ClaimToken,
        ) -> Result<ReleaseOutcome, proxima_core::StorageError> {
            unreachable!("nothing is claimed")
        }

        async fn pending_count(&self) -> Result<u64, proxima_core::StorageError> {
            Ok(0)
        }
    }

    #[derive(Debug)]
    struct NoIntake;

    #[async_trait::async_trait]
    impl DurableIntake for NoIntake {
        async fn accept(&self, _event: &ReceivedEvent) -> Result<Intake, IntakeError> {
            unreachable!("nothing is claimed")
        }
    }

    fn run_drain(
        outbox: &Arc<EmptyOutbox>,
        settings: Settings,
    ) -> (CancellationToken, JoinHandle<()>) {
        let drain = Drain::new(
            outbox.clone(),
            ConsumerName::default(),
            Arc::new(NoIntake),
            settings,
            Arc::new(Proceed),
        );
        let cancel = CancellationToken::new();
        let (_gates, task) = spawn(vec![drain], cancel.clone(), |_| async {});
        (cancel, task)
    }

    /// An idle drain backs off to the ceiling: over ten minutes it claims
    /// about thirty times, not once per poll interval (1200 times).
    #[tokio::test(start_paused = true)]
    async fn an_idle_drain_backs_off_to_the_ceiling() {
        let outbox = Arc::new(EmptyOutbox::default());
        let settings = Settings::new();
        let (poll, ceiling) = (settings.poll_interval, settings.idle_poll_max);
        let (cancel, task) = run_drain(&outbox, settings);
        tokio::time::sleep(Duration::from_mins(10)).await;
        cancel.cancel();
        task.await.expect("drain joins");

        // 0.5 + 1 + 2 + 4 + 8 + 16 s of doubling, then one claim per 30 s.
        let doubling = 6;
        let ramp: Duration = (0..doubling).map(|step| poll * (1 << step)).sum();
        let at_ceiling = Duration::from_mins(10)
            .checked_sub(ramp)
            .expect("the ramp is shorter than ten minutes")
            .as_secs()
            / ceiling.as_secs();
        let bound = 1 + doubling + usize::try_from(at_ceiling).expect("small");
        let claims = outbox.claims.load(Ordering::SeqCst);
        assert!(
            (bound - 2..=bound + 1).contains(&claims),
            "{claims} claims in ten idle minutes, expected about {bound}"
        );
    }

    /// A write committed in this process wakes an idle drain at once, however
    /// long its fallback sleep.
    #[tokio::test]
    async fn a_committed_write_wakes_an_idle_drain() {
        let outbox = Arc::new(EmptyOutbox::default());
        let settings = Settings {
            poll_interval: Duration::from_hours(1),
            idle_poll_max: Duration::from_hours(1),
            ..Settings::new()
        };
        let (cancel, task) = run_drain(&outbox, settings);
        let claims = |wanted: usize| {
            let outbox = outbox.clone();
            tokio::time::timeout(Duration::from_secs(1), async move {
                while outbox.claims.load(Ordering::SeqCst) < wanted {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
        };
        claims(1).await.expect("the first claim runs at once");
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(outbox.claims.load(Ordering::SeqCst), 1, "then it sleeps");

        write_committed();
        claims(2).await.expect("a committed write wakes the drain");
        write_committed();
        claims(3).await.expect("and wakes it again");
        cancel.cancel();
        task.await.expect("drain joins");
    }

    #[test]
    fn the_backoff_doubles_to_the_ceiling_and_a_higher_floor_wins() {
        let poll = Duration::from_millis(500);
        assert_eq!(
            next_backoff(poll, poll, MAX_BACKOFF),
            Duration::from_secs(1)
        );
        assert_eq!(
            next_backoff(Duration::from_secs(20), poll, MAX_BACKOFF),
            Duration::from_secs(30)
        );
        let slow = Duration::from_mins(1);
        assert_eq!(
            next_backoff(slow, slow, MAX_BACKOFF),
            slow,
            "a floor over the ceiling wins"
        );
    }
}

#[cfg(test)]
#[path = "in_process_outbox_tests.rs"]
mod in_process_outbox_tests;
