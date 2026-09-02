//! Operator maintenance verbs — the cold-object purge drain and change-log
//! rotation.
//!
//! Both are operator surfaces for the `proxima-mcp maintain-storage` CLI
//! (holding the database credentials is the authority, like the embedding
//! maintenance verbs). Neither is a promise to a user: the drain finishes
//! destruction a committed erase already owes the object store, and the
//! prune is log rotation on an operator-supplied horizon with no default.
//! What a host promises its users about retention, holds or erasure lives in
//! the host.

use proxima_core::ColdObjectStore;
use proxima_core::{Owner, OwnerRefKind, StorageError};
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::map_err;

/// Options for one `change_event` prune pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChangeEventPruneOptions {
    /// Age horizon: only events older than this many seconds are pruned.
    /// There is deliberately no default — destruction requires an
    /// explicit operator choice.
    pub older_than_seconds: i64,
    /// Events deleted per transaction. Each batch is its own transaction, so
    /// a long rotation never holds one open across the whole log.
    pub batch_size: i64,
    /// Count prunable events without deleting anything.
    pub dry_run: bool,
}

/// Per-owner outcome of a `change_event` prune pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PruneOwnerOutcome {
    pub owner: Owner,
    /// Events deleted (dry run: events that would be deleted).
    pub events_pruned: u64,
}

/// Outcome of one `change_event` prune pass.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ChangeEventPruneOutcome {
    pub owners: Vec<PruneOwnerOutcome>,
    pub events_pruned: u64,
    pub dry_run: bool,
}

/// Options for one bounded retry of exact object-store purge debts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColdPurgeRetryOptions {
    pub batch_size: i64,
    pub dry_run: bool,
}

/// Outcome of one bounded cold-object purge retry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ColdPurgeRetryOutcome {
    pub selected: u64,
    pub purged: u64,
    pub failed: u64,
    pub remaining: u64,
    pub dry_run: bool,
}

/// Retry a stable, bounded batch of durable exact-key purge debts.
pub(crate) async fn retry_cold_object_purges(
    pool: &PgPool,
    cold: &dyn ColdObjectStore,
    options: ColdPurgeRetryOptions,
) -> Result<ColdPurgeRetryOutcome, StorageError> {
    if options.batch_size < 1 {
        return Err(StorageError::ConstraintViolation(
            "cold purge retry batch_size must be positive".into(),
        ));
    }
    refuse_foreign_backends(pool, cold.backend()).await?;
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT object_key, backend
           FROM proxima_core.cold_purge_pending
          WHERE backend IN ('', $2)
          ORDER BY enqueued_at, object_key
          LIMIT $1",
    )
    .bind(options.batch_size)
    .bind(cold.backend())
    .fetch_all(pool)
    .await
    .map_err(map_err)?;
    let selected = u64::try_from(rows.len()).unwrap_or(u64::MAX);
    if options.dry_run {
        return Ok(ColdPurgeRetryOutcome {
            selected,
            remaining: pending_cold_purge_count(pool).await?,
            dry_run: true,
            ..ColdPurgeRetryOutcome::default()
        });
    }
    let plan = crate::verbs::forget::ColdPurgePlan::from_entries(
        rows.into_iter()
            .map(
                |(object_key, backend)| crate::verbs::forget::ColdPurgeEntry {
                    object_key,
                    backend,
                },
            )
            .collect(),
    );
    let purge = crate::verbs::forget::purge_cold_objects_after_commit(pool, cold, &plan).await;
    Ok(ColdPurgeRetryOutcome {
        selected,
        purged: purge.purged,
        failed: purge.failed,
        remaining: pending_cold_purge_count(pool).await?,
        dry_run: false,
    })
}

/// Refuse the drain while the queue holds a debt against another object store.
///
/// Not a skip, and not a best-effort filter. A `cold_purge_pending` row naming
/// a backend this store is not means the deployment's queue and its wired
/// `ColdObjectStore` disagree about which bucket holds the bytes — and the two
/// wrong answers are both worse than stopping. Draining anyway against the
/// wired store deletes nothing and clears the debt, so bytes an erase promised
/// to destroy survive with no record that they do. Skipping the rows silently
/// leaves a queue that never empties and never says why. The operator is the
/// only party that can fix the wiring, so the operator is told.
///
/// A row with no recorded backend is not foreign: see
/// [`proxima_core::UNRECORDED_BACKEND`].
async fn refuse_foreign_backends(pool: &PgPool, store_backend: &str) -> Result<(), StorageError> {
    if store_backend == proxima_core::UNRECORDED_BACKEND {
        // A store that declares no identity cannot be the wrong one.
        return Ok(());
    }
    let foreign: Vec<(String, i64)> = sqlx::query_as(
        "SELECT backend, count(*)::bigint
           FROM proxima_core.cold_purge_pending
          WHERE backend NOT IN ('', $1)
          GROUP BY backend
          ORDER BY backend
          LIMIT 4",
    )
    .bind(store_backend)
    .fetch_all(pool)
    .await
    .map_err(map_err)?;
    if foreign.is_empty() {
        return Ok(());
    }
    let named = foreign
        .iter()
        .map(|(backend, count)| format!("{backend}={count}"))
        .collect::<Vec<_>>()
        .join(", ");
    Err(StorageError::ConstraintViolation(format!(
        "cold purge queue holds debts against object stores this process is not wired to \
         ({named}); the wired store is {store_backend:?}. Wire the store those objects were \
         published to, or the erase that promised to destroy them cannot keep its promise."
    )))
}

async fn pending_cold_purge_count(pool: &PgPool) -> Result<u64, StorageError> {
    let count: i64 =
        sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.cold_purge_pending")
            .fetch_one(pool)
            .await
            .map_err(map_err)?;
    Ok(count.unsigned_abs())
}

pub(crate) async fn prune_change_log(
    pool: &PgPool,
    options: ChangeEventPruneOptions,
) -> Result<ChangeEventPruneOutcome, StorageError> {
    if options.older_than_seconds < 1 {
        return Err(StorageError::ConstraintViolation(
            "change_event prune horizon must be positive".into(),
        ));
    }
    if options.batch_size < 1 {
        return Err(StorageError::ConstraintViolation(
            "change_event prune batch_size must be positive".into(),
        ));
    }
    let candidates: Vec<(OwnerRefKind, Uuid)> = sqlx::query_as(
        "SELECT DISTINCT o.kind, a.owner_id
           FROM proxima_core.announce a
           JOIN proxima_core.owners o ON o.owner_id = a.owner_id
          WHERE COALESCE(uuid_extract_timestamp(a.seq), TIMESTAMPTZ '1970-01-01')
                < now() - make_interval(secs => ($1::bigint)::double precision)
          ORDER BY o.kind, a.owner_id",
    )
    .bind(options.older_than_seconds)
    .fetch_all(pool)
    .await
    .map_err(map_err)?;

    let mut outcome = ChangeEventPruneOutcome {
        dry_run: options.dry_run,
        ..ChangeEventPruneOutcome::default()
    };
    for (owner_kind, owner_id) in candidates {
        let owner = decode_owner(owner_kind, owner_id);
        let owner_outcome = prune_owner(pool, owner, options).await?;
        outcome.events_pruned += owner_outcome.events_pruned;
        outcome.owners.push(owner_outcome);
    }
    Ok(outcome)
}

async fn prune_owner(
    pool: &PgPool,
    owner: Owner,
    options: ChangeEventPruneOptions,
) -> Result<PruneOwnerOutcome, StorageError> {
    let (_owner_kind, owner_id) = owner.columns();
    let mut events_pruned: u64 = 0;
    loop {
        let mut tx = pool.begin().await.map_err(map_err)?;
        if options.dry_run {
            let due: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM proxima_core.announce
                  WHERE owner_id IS NOT DISTINCT FROM $1
                    AND COALESCE(uuid_extract_timestamp(seq), TIMESTAMPTZ '1970-01-01') < now()
                        - make_interval(secs => ($2::bigint)::double precision)",
            )
            .bind(owner_id)
            .bind(options.older_than_seconds)
            .fetch_one(tx.as_mut())
            .await
            .map_err(map_err)?;
            return Ok(PruneOwnerOutcome {
                owner,
                events_pruned: due.unsigned_abs(),
            });
        }
        let deleted = sqlx::query(
            "DELETE FROM proxima_core.announce
              WHERE seq IN (
                  SELECT seq FROM proxima_core.announce
                   WHERE owner_id IS NOT DISTINCT FROM $1
                     AND COALESCE(uuid_extract_timestamp(seq), TIMESTAMPTZ '1970-01-01') < now()
                         - make_interval(secs => ($2::bigint)::double precision)
                   ORDER BY seq
                   LIMIT $3
              )",
        )
        .bind(owner_id)
        .bind(options.older_than_seconds)
        .bind(options.batch_size)
        .execute(tx.as_mut())
        .await
        .map_err(map_err)?
        .rows_affected();
        tx.commit().await.map_err(map_err)?;
        events_pruned += deleted;
        if deleted < options.batch_size.unsigned_abs() {
            return Ok(PruneOwnerOutcome {
                owner,
                events_pruned,
            });
        }
    }
}

fn decode_owner(owner_kind: OwnerRefKind, owner_id: Uuid) -> Owner {
    owner_kind.with_uuid(owner_id)
}
