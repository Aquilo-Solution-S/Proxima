//! Owed object work: the expired-upload sweep and the purge-debt drain.
//!
//! The runtime runs [`object_maintenance_pass`] on its own cadence whenever
//! an object store is configured ([`crate::Feature::ObjectPurge`]), as it
//! drains embedding jobs. A library host that composes [`PgStorage`] itself
//! calls the same function. Every pass takes the storage maintenance lock,
//! the one `proxima-mcp maintain-storage` takes, so concurrent passes in
//! several processes never drain the same queue at once.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use proxima_blob_s3::{CitedBlobStore, UploadExpirySweepOutcome};
use proxima_core::authz::SystemAuthority;
use proxima_core::quiesce::Checkpoint;
use proxima_core::{QuiesceGate, StorageError};
use proxima_storage_pg::{ColdPurgeRetryOptions, ObjectPurgePassOutcome, PgStorage};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// Expired uploads one sweep call decides.
const SWEEP_BATCH: u32 = 500;
/// Sweep calls one pass makes at most.
const MAX_SWEEP_BATCHES: u32 = 20;
/// Drain batches one pass runs at most, of the default batch size.
const MAX_PURGE_BATCHES: u32 = 100;

/// What one [`object_maintenance_pass`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ObjectMaintenanceOutcome {
    /// The expired-upload sweep, summed over its batches; `None` when the
    /// pass was skipped.
    pub sweep: Option<UploadExpirySweepOutcome>,
    /// The purge-debt drain and the queue it left. `purge.skipped` when
    /// another process held the storage maintenance lock.
    pub purge: ObjectPurgePassOutcome,
}

/// Sweep expired uploads, then drain due purge debt, under the storage
/// maintenance lock.
///
/// The drain is wired to `blobs`' own object store, whatever cold store
/// `storage` carries, so it only ever discharges debt owed to that store.
/// Call it on a cadence no longer than the configured upload TTL. When
/// another process holds the lock, the pass does nothing and reports
/// `purge.skipped` with current queue counts.
///
/// A queue holding debt for another object store refuses the pass: stores
/// do not move, so that debt means this process is configured for the
/// wrong store. `proxima-mcp maintain-storage` names such debt, and a
/// process configured for that store drains it.
///
/// # Errors
///
/// Returns storage errors from the lock, the sweep, the drain or the
/// census; a constraint violation naming both stores when the queue holds
/// debt for another store; and a constraint violation when `authority` did
/// not boot `blobs`.
pub async fn object_maintenance_pass(
    storage: &PgStorage,
    blobs: &CitedBlobStore,
    authority: &SystemAuthority,
) -> Result<ObjectMaintenanceOutcome, StorageError> {
    let storage = storage.clone().with_cold(Arc::new(blobs.cold_store()));
    let options = ColdPurgeRetryOptions::default();
    let census = storage.object_purge_census(options).await?;
    refuse_foreign_debt(blobs.store_id(), &census)?;
    let Some(lock) = storage.try_storage_maintenance_lock().await? else {
        let mut purge = census;
        purge.skipped = true;
        return Ok(ObjectMaintenanceOutcome { sweep: None, purge });
    };
    let mut sweep = UploadExpirySweepOutcome::default();
    for _ in 0..MAX_SWEEP_BATCHES {
        let batch = blobs.sweep_expired_uploads(authority, SWEEP_BATCH).await?;
        sweep.expired += batch.expired;
        sweep.owed += batch.owed;
        sweep.remaining = batch.remaining;
        if batch.expired == 0 || batch.remaining == 0 {
            break;
        }
    }
    let purge = storage
        .drain_due_object_purge_debt(&lock, options, MAX_PURGE_BATCHES)
        .await?;
    Ok(ObjectMaintenanceOutcome {
        sweep: Some(sweep),
        purge,
    })
}

/// Refuse to boot or run a pass while the queue holds debt for a store
/// other than `configured`. The error names both stores and the count.
pub(crate) fn refuse_foreign_debt(
    configured: &proxima_core::ObjectStoreId,
    census: &ObjectPurgePassOutcome,
) -> Result<(), StorageError> {
    if census.foreign == 0 {
        return Ok(());
    }
    let found = census
        .foreign_stores
        .iter()
        .map(|(store, count)| format!("{store} ({count})"))
        .collect::<Vec<_>>()
        .join(", ");
    Err(StorageError::ConstraintViolation(format!(
        "the object purge queue holds {} debts for another object store: configured {configured}, \
         found {found}. An object store does not move, so this process is configured for the \
         wrong store: check PROXIMA_S3_ENDPOINT_URL, PROXIMA_S3_BUCKET and PROXIMA_S3_STORE_ID, \
         or drain that debt with proxima-mcp maintain-storage --retry-cold-object-purges \
         configured for the store it names",
        census.foreign
    )))
}

/// Startup check for a process that wires an object store: refuse while
/// the queue holds another store's debt.
pub(crate) async fn refuse_foreign_purge_debt_at_boot(
    storage: &PgStorage,
    blobs: &CitedBlobStore,
) -> Result<(), StorageError> {
    let census = storage
        .clone()
        .with_cold(Arc::new(blobs.cold_store()))
        .object_purge_census(ColdPurgeRetryOptions::default())
        .await?;
    refuse_foreign_debt(blobs.store_id(), &census)
}

/// The purge queue as the last pass left it, for the readiness answer.
/// Counts only: readiness never changes because of purge debt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ObjectPurgeSnapshot {
    pub(crate) due: u64,
    pub(crate) deferred: u64,
    pub(crate) stuck: u64,
}

impl ObjectPurgeSnapshot {
    fn of(pass: &ObjectPurgePassOutcome) -> Self {
        Self {
            due: pass.due,
            deferred: pass.deferred,
            stuck: pass.stuck_count,
        }
    }

    /// The readiness line: `object-purge: owed=… due=… deferred=… stuck=…`.
    /// Counts only, no keys or owners: the probe is anonymous.
    pub(crate) fn line(self) -> String {
        format!(
            "object-purge: owed={} due={} deferred={} stuck={}",
            self.due.saturating_add(self.deferred),
            self.due,
            self.deferred,
            self.stuck,
        )
    }
}

/// Shared between the object-purge worker, which writes after each pass,
/// and the readiness probe, which reads.
#[derive(Debug, Clone, Default)]
pub(crate) struct ObjectPurgeHealth(Arc<Mutex<Option<ObjectPurgeSnapshot>>>);

impl ObjectPurgeHealth {
    pub(crate) fn snapshot(&self) -> Option<ObjectPurgeSnapshot> {
        *self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn record(&self, snapshot: ObjectPurgeSnapshot) {
        *self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(snapshot);
    }
}

/// What the object-purge worker runs over.
pub(crate) struct ObjectPurgeWorker {
    pub(crate) storage: PgStorage,
    pub(crate) blobs: CitedBlobStore,
    pub(crate) authority: Arc<SystemAuthority>,
    pub(crate) interval: Duration,
    pub(crate) health: ObjectPurgeHealth,
}

/// Run [`object_maintenance_pass`] every `interval` until cancellation.
/// Returns the worker's pause control and its task.
///
/// The gate is checked before every pass: a quiesce lets the pass in
/// progress finish, then parks. The first pass runs at once, so a restart
/// drains what the previous process left owed.
pub(crate) fn spawn_object_purge_worker(
    worker: ObjectPurgeWorker,
    cancel: CancellationToken,
) -> (QuiesceGate, JoinHandle<()>) {
    let (control, mut gate) = QuiesceGate::new();
    let task = tokio::spawn(async move {
        let mut stuck_seen = 0_u64;
        loop {
            if gate.checkpoint(cancel.cancelled()).await == Checkpoint::Cancelled {
                return;
            }
            match object_maintenance_pass(&worker.storage, &worker.blobs, &worker.authority).await {
                Ok(outcome) => {
                    let snapshot = ObjectPurgeSnapshot::of(&outcome.purge);
                    worker.health.record(snapshot);
                    log_pass(&outcome);
                    // Warn when the stuck count changes, not on every pass:
                    // it stays visible on the probe.
                    if snapshot.stuck != stuck_seen {
                        if snapshot.stuck > 0 {
                            tracing::warn!(
                                stuck = snapshot.stuck,
                                "object purge debt keeps failing: run proxima-mcp \
                                 maintain-storage --retry-cold-object-purges to name it"
                            );
                        }
                        stuck_seen = snapshot.stuck;
                    }
                }
                Err(err) => tracing::warn!(error = %err, "object maintenance pass failed"),
            }
            tokio::select! {
                () = cancel.cancelled() => return,
                () = gate.changed() => {}
                () = tokio::time::sleep(worker.interval) => {}
            }
        }
    });
    (control, task)
}

fn log_pass(outcome: &ObjectMaintenanceOutcome) {
    let purge = &outcome.purge;
    if purge.skipped {
        tracing::debug!("object maintenance pass skipped: another pass holds the lock");
        return;
    }
    let expired = outcome.sweep.map_or(0, |sweep| sweep.expired);
    if expired > 0 || purge.purged > 0 || purge.absent > 0 || purge.failed > 0 {
        tracing::info!(
            expired,
            purged = purge.purged,
            absent = purge.absent,
            failed = purge.failed,
            due = purge.due,
            deferred = purge.deferred,
            "object maintenance pass"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{ObjectPurgeHealth, ObjectPurgeSnapshot, refuse_foreign_debt};
    use proxima_core::{ObjectStoreId, StorageError};
    use proxima_storage_pg::ObjectPurgePassOutcome;

    #[test]
    fn the_readiness_line_carries_counts_only() {
        let health = ObjectPurgeHealth::default();
        assert_eq!(health.snapshot(), None, "no pass yet, nothing to report");
        let snapshot = ObjectPurgeSnapshot {
            due: 2,
            deferred: 3,
            stuck: 1,
        };
        health.record(snapshot);
        assert_eq!(health.snapshot(), Some(snapshot));
        assert_eq!(
            snapshot.line(),
            "object-purge: owed=5 due=2 deferred=3 stuck=1"
        );
    }

    #[test]
    fn foreign_debt_is_refused_naming_both_stores_and_the_count() {
        let configured = ObjectStoreId::operator("here").expect("valid id");
        let mut census = ObjectPurgePassOutcome::default();
        assert!(refuse_foreign_debt(&configured, &census).is_ok());
        census.foreign = 3;
        census.foreign_stores = vec![(ObjectStoreId::operator("there").expect("valid id"), 3)];
        let Err(StorageError::ConstraintViolation(message)) =
            refuse_foreign_debt(&configured, &census)
        else {
            panic!("foreign debt must be refused");
        };
        assert!(message.contains("holds 3 debts"), "{message}");
        assert!(message.contains("configured s3:id/here"), "{message}");
        assert!(message.contains("found s3:id/there (3)"), "{message}");
    }
}
