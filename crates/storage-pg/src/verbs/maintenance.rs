//! Operator maintenance verb for the object-purge drain.
//!
//! This is an operator surface for the `proxima-mcp maintain-storage` CLI and
//! the runtime's object-maintenance pass (holding the database credentials is
//! the authority, like the embedding maintenance verbs). It is not a promise
//! to a user: the drain finishes destruction a committed erase already owes
//! the object store. What a host promises its users about retention, holds or
//! erasure lives in the host.
//!
//! The drain is written against a purge target (its [`ObjectStoreId`]) and a
//! delete port ([`ColdObjectStore`]) only: select the target's due debts,
//! delete each key, then clear the debt or record the failed attempt. Nothing
//! in it is specific to S3, so another kind of target can share the queue.

use std::time::Duration;

use crate::platform_scope::PgPlatformScope;
use proxima_core::{ColdObjectStore, ObjectStoreId, OwnerId, StorageError};
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::map_err;
use crate::sidecars::{int_to_u32, int_to_u64};

/// Most stuck debts an outcome names.
pub const MAX_STUCK_PURGE_DEBTS: usize = 32;

/// Most other stores an outcome names.
pub const MAX_FOREIGN_PURGE_STORES: usize = 8;

/// How a failed delete backs its debt off.
///
/// After the `n`th failed attempt the debt is due again after
/// `min(base * 2^(n-1), max)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PurgeBackoff {
    pub(crate) base: Duration,
    pub(crate) max: Duration,
}

impl Default for PurgeBackoff {
    fn default() -> Self {
        Self {
            base: DEFAULT_BACKOFF_BASE,
            max: DEFAULT_BACKOFF_MAX,
        }
    }
}

const DEFAULT_BATCH_SIZE: i64 = 100;
const DEFAULT_BACKOFF_BASE: Duration = Duration::from_secs(30);
const DEFAULT_BACKOFF_MAX: Duration = Duration::from_hours(6);
const DEFAULT_STUCK_AFTER: u32 = 8;

/// Options for one bounded drain of object-purge debt.
///
/// Build from [`Default`] and the setters:
///
/// | setting | default | meaning |
/// |---|---|---|
/// | `batch_size` | 100 | most due debts one call attempts |
/// | `dry_run` | `false` | select and count, write nothing |
/// | `backoff_base` | 30 s | delay after the first failed attempt |
/// | `backoff_max` | 6 h | ceiling for the doubling delay |
/// | `stuck_after` | 8 | attempts after which a debt is reported stuck |
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct ColdPurgeRetryOptions {
    batch_size: i64,
    dry_run: bool,
    backoff: PurgeBackoff,
    stuck_after: u32,
}

impl Default for ColdPurgeRetryOptions {
    fn default() -> Self {
        Self {
            batch_size: DEFAULT_BATCH_SIZE,
            dry_run: false,
            backoff: PurgeBackoff::default(),
            stuck_after: DEFAULT_STUCK_AFTER,
        }
    }
}

impl ColdPurgeRetryOptions {
    /// Most due debts one call attempts. Must be positive.
    #[must_use]
    pub const fn batch_size(mut self, batch_size: i64) -> Self {
        self.batch_size = batch_size;
        self
    }

    /// Select and count only; delete nothing and record nothing.
    #[must_use]
    pub const fn dry_run(mut self, dry_run: bool) -> Self {
        self.dry_run = dry_run;
        self
    }

    /// Delay after a debt's first failed attempt; doubles per attempt.
    #[must_use]
    pub const fn backoff_base(mut self, base: Duration) -> Self {
        self.backoff.base = base;
        self
    }

    /// Ceiling for the doubling delay.
    #[must_use]
    pub const fn backoff_max(mut self, max: Duration) -> Self {
        self.backoff.max = max;
        self
    }

    /// Attempts after which a debt is named in [`ColdPurgeRetryOutcome::stuck`].
    /// Must be positive.
    #[must_use]
    pub const fn stuck_after(mut self, attempts: u32) -> Self {
        self.stuck_after = attempts;
        self
    }

    pub(crate) const fn backoff(&self) -> PurgeBackoff {
        self.backoff
    }

    pub(crate) const fn stuck_threshold(&self) -> u32 {
        self.stuck_after
    }
}

/// A debt that has failed at least `stuck_after` times.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct StuckPurgeDebt {
    pub object_key: String,
    /// `None` when the owner was erased with the row that owed the key.
    pub owner_id: Option<OwnerId>,
    pub attempts: u32,
    /// The last failure, sanitized and bounded.
    pub last_error: Option<String>,
    pub next_attempt_at: time::OffsetDateTime,
}

/// Outcome of one bounded drain, and the queue the wired store is left with.
///
/// `due`, `deferred` and `stuck` count the wired store's debts after the
/// batch. Debts naming any other store are never selected, attempted or
/// cleared; they are counted in `foreign` and named in `foreign_stores`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct ColdPurgeRetryOutcome {
    /// Due debts this call picked up.
    pub selected: u64,
    /// Picked-up debts whose object was deleted (or already retired).
    pub purged: u64,
    /// Picked-up debts whose key held nothing; cleared.
    pub absent: u64,
    /// Picked-up debts whose delete failed; backed off.
    pub failed: u64,
    /// Wired-store debts selectable now.
    pub due: u64,
    /// Wired-store debts backing off after a failure.
    pub deferred: u64,
    /// Debts owed to other stores.
    pub foreign: u64,
    /// Wired-store debts with at least `stuck_after` failed attempts.
    pub stuck_count: u64,
    /// Up to [`MAX_STUCK_PURGE_DEBTS`] of them, most attempts first.
    pub stuck: Vec<StuckPurgeDebt>,
    /// Up to [`MAX_FOREIGN_PURGE_STORES`] other stores and their debt counts.
    pub foreign_stores: Vec<(ObjectStoreId, u64)>,
    pub dry_run: bool,
}

impl ColdPurgeRetryOutcome {
    /// Work still owed to the wired store: `due + deferred`.
    #[must_use]
    pub const fn remaining(&self) -> u64 {
        self.due.saturating_add(self.deferred)
    }
}

/// Outcome of one object-purge pass: batches drained until no due debt is
/// left (or the batch bound is reached), and the queue the wired store is
/// left with.
///
/// `skipped` means another process held the storage maintenance lease and
/// this pass drained nothing; the queue counts are still current.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct ObjectPurgePassOutcome {
    pub skipped: bool,
    /// Batches this pass ran.
    pub batches: u32,
    pub purged: u64,
    pub absent: u64,
    pub failed: u64,
    /// Wired-store debts selectable now, after the pass.
    pub due: u64,
    /// Wired-store debts backing off after a failure.
    pub deferred: u64,
    /// Wired-store debts with at least `stuck_after` failed attempts.
    pub stuck_count: u64,
    /// Up to [`MAX_STUCK_PURGE_DEBTS`] of them, named with owner, attempts
    /// and last error.
    pub stuck: Vec<StuckPurgeDebt>,
    /// Debts owed to other stores, never touched by this pass.
    pub foreign: u64,
    pub foreign_stores: Vec<(ObjectStoreId, u64)>,
}

impl ObjectPurgePassOutcome {
    /// Debts still owed to the wired store: `due + deferred`.
    #[must_use]
    pub const fn owed(&self) -> u64 {
        self.due.saturating_add(self.deferred)
    }

    fn take_census(&mut self, census: PurgeQueueCensus) {
        self.due = census.due;
        self.deferred = census.deferred;
        self.stuck_count = census.stuck_count;
        self.stuck = census.stuck;
        self.foreign = census.foreign;
        self.foreign_stores = census.foreign_stores;
    }
}

/// Drain due debts in batches until none is due or `max_batches` ran. The
/// caller holds the storage maintenance lease.
pub(crate) async fn drain_due_purge_debt(
    pool: &PgPool,
    platform: &PgPlatformScope,
    cold: &dyn ColdObjectStore,
    lease: &crate::maintenance_lease::MaintenanceLease,
    options: ColdPurgeRetryOptions,
    max_batches: u32,
) -> Result<ObjectPurgePassOutcome, StorageError> {
    let mut pass = ObjectPurgePassOutcome::default();
    if options.dry_run {
        return Err(StorageError::ConstraintViolation(
            "an object purge pass does not dry-run; use retry_cold_object_purges".into(),
        ));
    }
    loop {
        if pass.batches >= max_batches {
            break;
        }
        // A batch deletes objects with no transaction open; confirm the pass
        // still holds the store's maintenance lease before starting one.
        lease.renew().await?;
        let batch = retry_cold_object_purges(pool, platform, cold, options).await?;
        pass.batches += 1;
        pass.purged += batch.purged;
        pass.absent += batch.absent;
        pass.failed += batch.failed;
        let drained_nothing = batch.selected == 0;
        pass.take_census(PurgeQueueCensus {
            due: batch.due,
            deferred: batch.deferred,
            foreign: batch.foreign,
            stuck_count: batch.stuck_count,
            stuck: batch.stuck,
            foreign_stores: batch.foreign_stores,
        });
        if drained_nothing || pass.due == 0 {
            return Ok(pass);
        }
    }
    if pass.batches == 0 {
        let census =
            purge_queue_census(platform, cold.store_id(), options.stuck_threshold()).await?;
        pass.take_census(census);
    }
    Ok(pass)
}

/// The queue counts of a pass that drained nothing; `skipped` when it
/// could not take the maintenance lease.
pub(crate) async fn undrained_purge_pass(
    platform: &PgPlatformScope,
    cold: &dyn ColdObjectStore,
    options: ColdPurgeRetryOptions,
    skipped: bool,
) -> Result<ObjectPurgePassOutcome, StorageError> {
    if options.stuck_threshold() < 1 {
        return Err(StorageError::ConstraintViolation(
            "cold purge retry stuck_after must be positive".into(),
        ));
    }
    let mut pass = ObjectPurgePassOutcome {
        skipped,
        ..ObjectPurgePassOutcome::default()
    };
    pass.take_census(
        purge_queue_census(platform, cold.store_id(), options.stuck_threshold()).await?,
    );
    Ok(pass)
}

/// The purge queue as one store sees it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct PurgeQueueCensus {
    pub(crate) due: u64,
    pub(crate) deferred: u64,
    pub(crate) foreign: u64,
    pub(crate) stuck_count: u64,
    pub(crate) stuck: Vec<StuckPurgeDebt>,
    pub(crate) foreign_stores: Vec<(ObjectStoreId, u64)>,
}

/// Drain one bounded batch of the wired store's due debts.
pub(crate) async fn retry_cold_object_purges(
    pool: &PgPool,
    platform: &PgPlatformScope,
    cold: &dyn ColdObjectStore,
    options: ColdPurgeRetryOptions,
) -> Result<ColdPurgeRetryOutcome, StorageError> {
    if options.batch_size < 1 {
        return Err(StorageError::ConstraintViolation(
            "cold purge retry batch_size must be positive".into(),
        ));
    }
    if options.stuck_after < 1 {
        return Err(StorageError::ConstraintViolation(
            "cold purge retry stuck_after must be positive".into(),
        ));
    }
    let target = cold.store_id();
    let mut tx = platform.begin().await?;
    let keys: Vec<String> = sqlx::query_scalar(
        "SELECT object_key
           FROM proxima_core.cold_purge_pending
          WHERE object_store = $2 AND next_attempt_at <= now()
          ORDER BY next_attempt_at, enqueued_at, object_key
          LIMIT $1",
    )
    .bind(options.batch_size)
    .bind(target.as_str())
    .fetch_all(tx.as_mut())
    .await
    .map_err(map_err)?;
    tx.commit().await.map_err(map_err)?;
    let selected = u64::try_from(keys.len()).unwrap_or(u64::MAX);
    let mut outcome = ColdPurgeRetryOutcome {
        selected,
        dry_run: options.dry_run,
        ..ColdPurgeRetryOutcome::default()
    };
    if !options.dry_run {
        let plan = crate::verbs::forget::ColdPurgePlan::from_entries(
            keys.into_iter()
                .map(|object_key| crate::verbs::forget::ColdPurgeEntry {
                    object_key,
                    object_store: target.as_str().to_owned(),
                })
                .collect(),
        );
        let purge = crate::verbs::forget::purge_cold_objects_with_backoff(
            pool,
            platform,
            cold,
            &plan,
            options.backoff(),
        )
        .await;
        outcome.purged = purge.purged;
        outcome.absent = purge.absent;
        outcome.failed = purge.failed;
    }
    let census = purge_queue_census(platform, target, options.stuck_threshold()).await?;
    outcome.due = census.due;
    outcome.deferred = census.deferred;
    outcome.foreign = census.foreign;
    outcome.stuck_count = census.stuck_count;
    outcome.stuck = census.stuck;
    outcome.foreign_stores = census.foreign_stores;
    if outcome.foreign > 0 {
        tracing::warn!(
            wired_store = %target,
            foreign = outcome.foreign,
            stores = ?outcome.foreign_stores,
            "purge queue holds debt for object stores this process is not wired to; \
             it is left in place until a process wired to that store drains it"
        );
    }
    Ok(outcome)
}

#[derive(sqlx::FromRow)]
struct StuckRow {
    object_key: String,
    owner_id: Option<Uuid>,
    attempts: i32,
    last_error: Option<String>,
    next_attempt_at: time::OffsetDateTime,
}

/// Count the queue for `target`: due, deferred and stuck debts, and debt
/// owed to every other store.
pub(crate) async fn purge_queue_census(
    platform: &PgPlatformScope,
    target: &ObjectStoreId,
    stuck_after: u32,
) -> Result<PurgeQueueCensus, StorageError> {
    let stuck_after = i32::try_from(stuck_after).unwrap_or(i32::MAX);
    let mut tx = platform.begin().await?;
    let (due, deferred, foreign, stuck_count): (i64, i64, i64, i64) = sqlx::query_as(
        "SELECT count(*) FILTER (WHERE object_store = $1 AND next_attempt_at <= now()),
                count(*) FILTER (WHERE object_store = $1 AND next_attempt_at > now()),
                count(*) FILTER (WHERE object_store <> $1),
                count(*) FILTER (WHERE object_store = $1 AND attempts >= $2)
           FROM proxima_core.cold_purge_pending",
    )
    .bind(target.as_str())
    .bind(stuck_after)
    .fetch_one(tx.as_mut())
    .await
    .map_err(map_err)?;
    let stuck: Vec<StuckRow> = if stuck_count > 0 {
        sqlx::query_as(
            "SELECT object_key, owner_id, attempts, last_error, next_attempt_at
               FROM proxima_core.cold_purge_pending
              WHERE object_store = $1 AND attempts >= $2
              ORDER BY attempts DESC, object_key
              LIMIT $3",
        )
        .bind(target.as_str())
        .bind(stuck_after)
        .bind(i64::try_from(MAX_STUCK_PURGE_DEBTS).unwrap_or(i64::MAX))
        .fetch_all(tx.as_mut())
        .await
        .map_err(map_err)?
    } else {
        Vec::new()
    };
    let foreign_stores: Vec<(String, i64)> = if foreign > 0 {
        sqlx::query_as(
            "SELECT object_store, count(*)::bigint
               FROM proxima_core.cold_purge_pending
              WHERE object_store <> $1
              GROUP BY object_store
              ORDER BY count(*) DESC, object_store
              LIMIT $2",
        )
        .bind(target.as_str())
        .bind(i64::try_from(MAX_FOREIGN_PURGE_STORES).unwrap_or(i64::MAX))
        .fetch_all(tx.as_mut())
        .await
        .map_err(map_err)?
    } else {
        Vec::new()
    };
    tx.commit().await.map_err(map_err)?;
    census_from_rows([due, deferred, foreign, stuck_count], stuck, foreign_stores)
}

/// The census as the queue reports it: `counts` are due, deferred, foreign and
/// stuck. A negative count or attempt number is a fault in the database, not
/// a number to report.
fn census_from_rows(
    counts: [i64; 4],
    stuck: Vec<StuckRow>,
    foreign_stores: Vec<(String, i64)>,
) -> Result<PurgeQueueCensus, StorageError> {
    let [due, deferred, foreign, stuck_count] = counts;
    let foreign_stores = foreign_stores
        .into_iter()
        .map(|(store, count)| {
            let store = ObjectStoreId::parse(&store).map_err(|error| {
                StorageError::Internal(format!("purge queue holds a malformed store id: {error}"))
            })?;
            Ok((store, int_to_u64(count, "foreign purge debts")?))
        })
        .collect::<Result<Vec<_>, StorageError>>()?;
    let stuck = stuck
        .into_iter()
        .map(|row| {
            Ok(StuckPurgeDebt {
                object_key: row.object_key,
                owner_id: row.owner_id.map(OwnerId::from),
                attempts: int_to_u32(row.attempts, "purge attempts")?,
                last_error: row.last_error,
                next_attempt_at: row.next_attempt_at,
            })
        })
        .collect::<Result<Vec<_>, StorageError>>()?;
    Ok(PurgeQueueCensus {
        due: int_to_u64(due, "due purge debts")?,
        deferred: int_to_u64(deferred, "deferred purge debts")?,
        foreign: int_to_u64(foreign, "foreign purge debts")?,
        stuck_count: int_to_u64(stuck_count, "stuck purge debts")?,
        stuck,
        foreign_stores,
    })
}

#[cfg(test)]
mod tests {
    use super::{StuckRow, census_from_rows};

    fn stuck_row(attempts: i32) -> StuckRow {
        StuckRow {
            object_key: "objects/k".into(),
            owner_id: None,
            attempts,
            last_error: None,
            next_attempt_at: time::OffsetDateTime::UNIX_EPOCH,
        }
    }

    #[test]
    fn the_census_reports_what_the_queue_counted() {
        let census = census_from_rows(
            [4, 3, 2, 1],
            vec![stuck_row(9)],
            vec![("s3:id/other".into(), 2)],
        )
        .expect("a census");
        assert_eq!(
            (
                census.due,
                census.deferred,
                census.foreign,
                census.stuck_count
            ),
            (4, 3, 2, 1)
        );
        assert_eq!(census.stuck[0].attempts, 9);
        assert_eq!(census.foreign_stores[0].1, 2);
    }

    #[test]
    fn a_negative_count_is_a_fault_not_a_count() {
        for counts in [[-1, 0, 0, 0], [0, -1, 0, 0], [0, 0, -1, 0], [0, 0, 0, -1]] {
            census_from_rows(counts, Vec::new(), Vec::new()).expect_err("negative count");
        }
        census_from_rows([0; 4], vec![stuck_row(-1)], Vec::new()).expect_err("negative attempts");
        census_from_rows([0; 4], Vec::new(), vec![("s3:id/other".into(), -1)])
            .expect_err("negative foreign count");
    }
}
