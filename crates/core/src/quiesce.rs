//! The pause gate of a claim-based background worker (docs/10 §Quiesce and
//! resume).
//!
//! A worker that claims queue rows under a lease holds the [`GateWorker`] half
//! and calls [`GateWorker::checkpoint`] before every claim. The host holds the
//! [`QuiesceGate`] half. [`QuiesceGate::quiesce`] closes the gate and resolves
//! only once the worker task itself has parked at a checkpoint, so no claim
//! starts after it returns and every claim the worker held when the gate
//! closed has been settled. The gate says nothing about queue depth: rows
//! enqueued while the worker is parked stay queued.
//!
//! [`QuiesceGate::new`] is the only constructor. Dropping the [`GateWorker`]
//! (the task ended, for any reason) is what makes the gate
//! [`FeatureStatus::Stopped`]; the worker half is not `Clone`, so a clone
//! cannot keep a stopped task looking alive:
//!
//! ```compile_fail,E0277
//! fn needs_clone<T: Clone>() {}
//! needs_clone::<proxima_core::GateWorker>();
//! ```
//!
//! The control half cannot park, so a host holding a [`QuiesceGate`] cannot
//! acknowledge its own pause:
//!
//! ```compile_fail,E0599
//! async fn host(gate: proxima_core::QuiesceGate) {
//!     gate.checkpoint(std::future::pending()).await;
//! }
//! ```

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};

use tokio::sync::watch;

/// Where a gated feature is in its pause/resume cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeatureStatus {
    /// Claiming (or idle between claims).
    Running,
    /// A pause was requested; the worker has not parked yet.
    Quiescing,
    /// The worker is parked: it claims nothing and holds nothing.
    Quiescent,
    /// The worker task ended: runtime shutdown, or it terminated by itself.
    Stopped,
}

/// Why a [`QuiesceGate`] call did not take effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum FeatureControlError {
    /// The worker task has ended; nothing is left to pause or resume.
    #[error("the feature has stopped")]
    Stopped,
    /// A pending `quiesce()` was overtaken by a `resume()`.
    #[error("the feature was resumed before it quiesced")]
    Resumed,
}

#[derive(Debug, Clone, Copy)]
struct State {
    status: FeatureStatus,
    /// Bumped each time the gate closes, so a `quiesce()` waiting on one
    /// pause cannot mistake a later pause for its own.
    cycle: u64,
}

/// The host's half of the gate: asks the worker to pause and resume.
///
/// Cheap to clone; every clone controls the same worker. Dropping a pending
/// [`Self::quiesce`] does not reopen the gate: the pause stays requested.
#[derive(Debug, Clone)]
pub struct QuiesceGate {
    state: Arc<watch::Sender<State>>,
}

/// The worker's half of the gate. Not `Clone`; dropping it stops the gate.
#[derive(Debug)]
pub struct GateWorker {
    state: Arc<watch::Sender<State>>,
    seen: watch::Receiver<State>,
}

/// What a [`GateWorker::checkpoint`] decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub enum Checkpoint {
    /// The gate is open: claim.
    Proceed,
    /// The worker was cancelled: end the task.
    Cancelled,
}

impl QuiesceGate {
    /// A fresh open gate: the control half, and the worker half the task
    /// keeps.
    #[must_use]
    pub fn new() -> (Self, GateWorker) {
        let (sender, seen) = watch::channel(State {
            status: FeatureStatus::Running,
            cycle: 0,
        });
        let state = Arc::new(sender);
        (
            Self {
                state: state.clone(),
            },
            GateWorker { state, seen },
        )
    }

    /// The current status.
    #[must_use]
    pub fn status(&self) -> FeatureStatus {
        self.state.borrow().status
    }

    /// Close the gate and wait until the worker has parked.
    ///
    /// Concurrent calls join the same pause and resolve together; on a
    /// parked worker it resolves at once.
    ///
    /// # Errors
    ///
    /// [`FeatureControlError::Stopped`] when the worker task has ended, also
    /// while this call is pending; [`FeatureControlError::Resumed`] when
    /// [`Self::resume`] reopens the gate while this call is pending.
    pub async fn quiesce(&self) -> Result<(), FeatureControlError> {
        let mut watcher = self.state.subscribe();
        let mut cycle = 0;
        self.state.send_if_modified(|state| {
            let closes = state.status == FeatureStatus::Running;
            if closes {
                state.status = FeatureStatus::Quiescing;
                state.cycle += 1;
            }
            cycle = state.cycle;
            closes
        });
        loop {
            let state = *watcher.borrow_and_update();
            if state.cycle != cycle {
                return Err(FeatureControlError::Resumed);
            }
            match state.status {
                FeatureStatus::Quiescent => return Ok(()),
                FeatureStatus::Stopped => return Err(FeatureControlError::Stopped),
                FeatureStatus::Running => return Err(FeatureControlError::Resumed),
                FeatureStatus::Quiescing => {}
            }
            // The sender lives in `self`, so the channel cannot close.
            if watcher.changed().await.is_err() {
                return Err(FeatureControlError::Stopped);
            }
        }
    }

    /// Reopen the gate and wake the worker. `Ok` and no change when it is
    /// already open.
    ///
    /// # Errors
    ///
    /// [`FeatureControlError::Stopped`] when the worker task has ended.
    pub fn resume(&self) -> Result<(), FeatureControlError> {
        let mut outcome = Ok(());
        self.state.send_if_modified(|state| match state.status {
            FeatureStatus::Stopped => {
                outcome = Err(FeatureControlError::Stopped);
                false
            }
            FeatureStatus::Running => false,
            FeatureStatus::Quiescing | FeatureStatus::Quiescent => {
                state.status = FeatureStatus::Running;
                true
            }
        });
        outcome
    }
}

impl GateWorker {
    /// Whether a pause was requested. A worker that holds a batch checks
    /// this between records and hands the unattempted rest back.
    #[must_use]
    pub fn is_closing(&self) -> bool {
        self.seen.borrow().status != FeatureStatus::Running
    }

    /// Resolves once the gate changed since this worker last looked, so an
    /// idle sleep ends on a quiesce or a resume instead of waiting out its
    /// interval. Cancel-safe.
    pub async fn changed(&mut self) {
        // The sender lives in `self.state`, so the channel cannot close.
        let _ = self.seen.changed().await;
    }

    /// The point before every claim: returns at once while the gate is open;
    /// while it is closed, acknowledges the pause (which resolves the pending
    /// [`QuiesceGate::quiesce`] calls) and parks until the gate reopens.
    /// `cancelled` always wins, so a parked worker still ends on shutdown.
    pub async fn checkpoint(&mut self, cancelled: impl Future<Output = ()>) -> Checkpoint {
        let mut cancelled = std::pin::pin!(cancelled);
        loop {
            if poll_once(cancelled.as_mut()).is_ready() {
                return Checkpoint::Cancelled;
            }
            let status = self.seen.borrow_and_update().status;
            match status {
                FeatureStatus::Running => return Checkpoint::Proceed,
                FeatureStatus::Quiescing => {
                    self.state.send_if_modified(|state| {
                        let parks = state.status == FeatureStatus::Quiescing;
                        if parks {
                            state.status = FeatureStatus::Quiescent;
                        }
                        parks
                    });
                }
                FeatureStatus::Quiescent | FeatureStatus::Stopped => {}
            }
            tokio::select! {
                () = &mut cancelled => return Checkpoint::Cancelled,
                _ = self.seen.changed() => {}
            }
        }
    }
}

/// One poll with no wake-up registered: asks whether `future` is done
/// already. The caller polls it again for real if the answer is no.
fn poll_once<F: Future>(future: Pin<&mut F>) -> Poll<F::Output> {
    future.poll(&mut Context::from_waker(Waker::noop()))
}

impl Drop for GateWorker {
    fn drop(&mut self) {
        self.state
            .send_modify(|state| state.status = FeatureStatus::Stopped);
    }
}

#[cfg(test)]
mod tests {
    use std::pin::pin;
    use std::time::Duration;

    use tokio::sync::oneshot;
    use tokio::time::Instant;

    use super::*;

    /// A worker task that waits for `go`, then runs one checkpoint with no
    /// cancellation, reporting what it decided.
    fn worker_at_checkpoint(
        mut worker: GateWorker,
    ) -> (oneshot::Sender<()>, tokio::task::JoinHandle<Checkpoint>) {
        let (go, wait) = oneshot::channel();
        let task = tokio::spawn(async move {
            let _ = wait.await;
            worker.checkpoint(std::future::pending()).await
        });
        (go, task)
    }

    #[tokio::test]
    async fn quiesce_resolves_only_after_the_worker_parks() {
        let (gate, worker) = QuiesceGate::new();
        let (go, task) = worker_at_checkpoint(worker);

        let mut quiesce = pin!(gate.quiesce());
        assert_eq!(poll_once(quiesce.as_mut()), Poll::Pending);
        assert_eq!(gate.status(), FeatureStatus::Quiescing);

        // The worker reaches its checkpoint: only now does the pause settle.
        go.send(()).expect("worker waits");
        assert_eq!(quiesce.await, Ok(()));
        assert_eq!(gate.status(), FeatureStatus::Quiescent);
        assert!(!task.is_finished(), "the worker stays parked");

        assert_eq!(gate.resume(), Ok(()));
        assert_eq!(gate.status(), FeatureStatus::Running);
        assert_eq!(task.await.expect("worker"), Checkpoint::Proceed);
    }

    #[tokio::test]
    async fn concurrent_quiesce_calls_resolve_together_and_a_parked_one_resolves_at_once() {
        let (gate, worker) = QuiesceGate::new();
        let (go, task) = worker_at_checkpoint(worker);

        let mut first = pin!(gate.quiesce());
        let mut second = pin!(gate.quiesce());
        assert_eq!(poll_once(first.as_mut()), Poll::Pending);
        assert_eq!(poll_once(second.as_mut()), Poll::Pending);
        go.send(()).expect("worker waits");
        assert_eq!(tokio::join!(first, second), (Ok(()), Ok(())));

        let mut parked = pin!(gate.quiesce());
        assert_eq!(poll_once(parked.as_mut()), Poll::Ready(Ok(())));
        assert_eq!(gate.resume(), Ok(()));
        assert_eq!(task.await.expect("worker"), Checkpoint::Proceed);
    }

    #[tokio::test]
    async fn resume_while_quiescing_resolves_the_pending_quiesce_as_resumed() {
        let (gate, mut worker) = QuiesceGate::new();
        let mut quiesce = pin!(gate.quiesce());
        assert_eq!(poll_once(quiesce.as_mut()), Poll::Pending);

        assert_eq!(gate.resume(), Ok(()));
        assert_eq!(quiesce.await, Err(FeatureControlError::Resumed));
        assert_eq!(gate.status(), FeatureStatus::Running);
        // The worker was never asked to park: it continues.
        assert_eq!(
            worker.checkpoint(std::future::pending()).await,
            Checkpoint::Proceed
        );
        assert_eq!(gate.status(), FeatureStatus::Running);
    }

    #[tokio::test]
    async fn a_quiesce_does_not_mistake_a_later_pause_for_its_own() {
        let (gate, worker) = QuiesceGate::new();
        let mut earlier = pin!(gate.quiesce());
        assert_eq!(poll_once(earlier.as_mut()), Poll::Pending);
        assert_eq!(gate.resume(), Ok(()));
        let mut later = pin!(gate.quiesce());
        assert_eq!(poll_once(later.as_mut()), Poll::Pending);

        let (go, task) = worker_at_checkpoint(worker);
        go.send(()).expect("worker waits");
        assert_eq!(later.await, Ok(()));
        assert_eq!(earlier.await, Err(FeatureControlError::Resumed));
        assert_eq!(gate.resume(), Ok(()));
        assert_eq!(task.await.expect("worker"), Checkpoint::Proceed);
    }

    #[tokio::test]
    async fn resume_on_a_running_gate_is_ok_and_changes_nothing() {
        let (gate, _worker) = QuiesceGate::new();
        assert_eq!(gate.resume(), Ok(()));
        assert_eq!(gate.resume(), Ok(()));
        assert_eq!(gate.status(), FeatureStatus::Running);
    }

    #[tokio::test]
    async fn the_worker_ending_while_quiescing_resolves_the_pending_quiesce_as_stopped() {
        let (gate, worker) = QuiesceGate::new();
        let mut quiesce = pin!(gate.quiesce());
        assert_eq!(poll_once(quiesce.as_mut()), Poll::Pending);

        drop(worker);
        assert_eq!(quiesce.await, Err(FeatureControlError::Stopped));
        assert_eq!(gate.status(), FeatureStatus::Stopped);
    }

    #[tokio::test]
    async fn every_call_after_the_worker_ended_is_stopped() {
        let (gate, worker) = QuiesceGate::new();
        drop(worker);
        assert_eq!(gate.status(), FeatureStatus::Stopped);
        assert_eq!(gate.quiesce().await, Err(FeatureControlError::Stopped));
        assert_eq!(gate.resume(), Err(FeatureControlError::Stopped));
        assert_eq!(gate.status(), FeatureStatus::Stopped);
    }

    #[tokio::test]
    async fn the_worker_ending_while_quiescent_stops_the_gate() {
        let (gate, worker) = QuiesceGate::new();
        let (go, task) = worker_at_checkpoint(worker);
        go.send(()).expect("worker waits");
        assert_eq!(gate.quiesce().await, Ok(()));
        task.abort();
        assert!(task.await.expect_err("aborted").is_cancelled());
        assert_eq!(gate.status(), FeatureStatus::Stopped);
        assert_eq!(gate.resume(), Err(FeatureControlError::Stopped));
        assert_eq!(gate.quiesce().await, Err(FeatureControlError::Stopped));
    }

    #[tokio::test]
    async fn a_cancelled_worker_never_proceeds_and_a_parked_one_still_ends() {
        let (gate, mut worker) = QuiesceGate::new();
        assert_eq!(
            worker.checkpoint(std::future::ready(())).await,
            Checkpoint::Cancelled,
            "cancellation wins over an open gate"
        );

        let (cancel, cancelled) = oneshot::channel::<()>();
        let task = tokio::spawn(async move {
            worker
                .checkpoint(async {
                    let _ = cancelled.await;
                })
                .await
        });
        assert_eq!(gate.quiesce().await, Ok(()));
        cancel.send(()).expect("worker waits");
        assert_eq!(task.await.expect("worker"), Checkpoint::Cancelled);
        assert_eq!(gate.status(), FeatureStatus::Stopped);
    }

    #[tokio::test]
    async fn is_closing_follows_the_request_not_the_acknowledgement() {
        let (gate, worker) = QuiesceGate::new();
        assert!(!worker.is_closing());
        let mut quiesce = pin!(gate.quiesce());
        assert_eq!(poll_once(quiesce.as_mut()), Poll::Pending);
        assert!(worker.is_closing(), "closing before the worker parks");
        assert_eq!(gate.resume(), Ok(()));
        assert!(!worker.is_closing());
    }

    /// The loop shape both workers share, under a paused clock: an idle
    /// sleep that quiesce and resume interrupt, a checkpoint before the
    /// "claim". No sleep stands in for a signal here.
    #[tokio::test(start_paused = true)]
    async fn a_parked_worker_claims_nothing_across_intervals_and_resume_skips_the_interval() {
        const INTERVAL: Duration = Duration::from_hours(1);
        let (gate, mut worker) = QuiesceGate::new();
        let (claims, mut claimed) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            loop {
                if worker.checkpoint(std::future::pending()).await == Checkpoint::Cancelled {
                    return;
                }
                claims.send(()).expect("test holds the receiver");
                tokio::select! {
                    () = worker.changed() => {}
                    () = tokio::time::sleep(INTERVAL) => {}
                }
            }
        });
        claimed.recv().await.expect("the first pass claims");
        let started = Instant::now();

        // The idle sleep ends on the quiesce: no interval elapses.
        assert_eq!(gate.quiesce().await, Ok(()));
        assert!(started.elapsed() < INTERVAL);

        // Parked across several intervals: not one more claim.
        tokio::time::advance(INTERVAL * 3).await;
        assert!(claimed.try_recv().is_err());

        // Resume claims at once, without waiting out an interval.
        let resumed = Instant::now();
        assert_eq!(gate.resume(), Ok(()));
        claimed.recv().await.expect("resume wakes the worker");
        assert_eq!(resumed.elapsed(), Duration::ZERO);
    }
}
