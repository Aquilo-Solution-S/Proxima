//! Operator maintenance verb for the cold-object purge drain.
//!
//! This is an operator surface for the `proxima-mcp maintain-storage` CLI
//! (holding the database credentials is the authority, like the embedding
//! maintenance verbs). It is not a promise to a user: the drain finishes
//! destruction a committed erase already owes the object store. What a host
//! promises its users about retention, holds or erasure lives in the host.

use crate::platform_scope::PgPlatformScope;
use proxima_core::ColdObjectStore;
use proxima_core::StorageError;
use sqlx::PgPool;

use crate::error::map_err;

async fn begin_platform(
    _pool: &PgPool,
    platform: &PgPlatformScope,
) -> Result<sqlx::Transaction<'static, sqlx::Postgres>, StorageError> {
    platform.begin().await
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
    platform: &PgPlatformScope,
    cold: &dyn ColdObjectStore,
    options: ColdPurgeRetryOptions,
) -> Result<ColdPurgeRetryOutcome, StorageError> {
    if options.batch_size < 1 {
        return Err(StorageError::ConstraintViolation(
            "cold purge retry batch_size must be positive".into(),
        ));
    }
    refuse_foreign_stores(pool, platform, cold.store_id().as_str()).await?;
    let mut tx = begin_platform(pool, platform).await?;
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT object_key, object_store
           FROM proxima_core.cold_purge_pending
          WHERE object_store = $2
          ORDER BY enqueued_at, object_key
          LIMIT $1",
    )
    .bind(options.batch_size)
    .bind(cold.store_id().as_str())
    .fetch_all(tx.as_mut())
    .await
    .map_err(map_err)?;
    tx.commit().await.map_err(map_err)?;
    let selected = u64::try_from(rows.len()).unwrap_or(u64::MAX);
    if options.dry_run {
        return Ok(ColdPurgeRetryOutcome {
            selected,
            remaining: pending_cold_purge_count(pool, platform).await?,
            dry_run: true,
            ..ColdPurgeRetryOutcome::default()
        });
    }
    let plan = crate::verbs::forget::ColdPurgePlan::from_entries(
        rows.into_iter()
            .map(
                |(object_key, object_store)| crate::verbs::forget::ColdPurgeEntry {
                    object_key,
                    object_store,
                },
            )
            .collect(),
    );
    let purge = crate::verbs::forget::purge_cold_objects_after_commit_with_platform(
        pool, platform, cold, &plan,
    )
    .await;
    Ok(ColdPurgeRetryOutcome {
        selected,
        purged: purge.purged,
        failed: purge.failed,
        remaining: pending_cold_purge_count(pool, platform).await?,
        dry_run: false,
    })
}

/// Refuse the drain while the queue holds a debt against another object store.
///
/// Not a skip, and not a best-effort filter. A `cold_purge_pending` row naming
/// a store this one is not means the deployment's queue and its wired
/// `ColdObjectStore` disagree about which store holds the bytes — and the two
/// wrong answers are both worse than stopping. Draining anyway against the
/// wired store deletes nothing and clears the debt, so bytes an erase promised
/// to destroy survive with no record that they do. Skipping the rows silently
/// leaves a queue that never empties and never says why. The operator is the
/// only party that can fix the wiring, so the operator is told.
async fn refuse_foreign_stores(
    pool: &PgPool,
    platform: &PgPlatformScope,
    wired_store: &str,
) -> Result<(), StorageError> {
    let mut tx = begin_platform(pool, platform).await?;
    let foreign: Vec<(String, i64)> = sqlx::query_as(
        "SELECT object_store, count(*)::bigint
           FROM proxima_core.cold_purge_pending
          WHERE object_store <> $1
          GROUP BY object_store
          ORDER BY object_store
          LIMIT 4",
    )
    .bind(wired_store)
    .fetch_all(tx.as_mut())
    .await
    .map_err(map_err)?;
    tx.commit().await.map_err(map_err)?;
    if foreign.is_empty() {
        return Ok(());
    }
    let named = foreign
        .iter()
        .map(|(store, count)| format!("{store}={count}"))
        .collect::<Vec<_>>()
        .join(", ");
    Err(StorageError::ConstraintViolation(format!(
        "purge queue holds debts against object stores this process is not wired to \
         ({named}); the wired store is {wired_store:?}. Wire the store those objects were \
         published to (PROXIMA_S3_STORE_ID, or its endpoint and bucket), or the erase that \
         promised to destroy them cannot keep its promise."
    )))
}

async fn pending_cold_purge_count(
    pool: &PgPool,
    platform: &PgPlatformScope,
) -> Result<u64, StorageError> {
    let mut tx = begin_platform(pool, platform).await?;
    let count: i64 =
        sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.cold_purge_pending")
            .fetch_one(tx.as_mut())
            .await
            .map_err(map_err)?;
    tx.commit().await.map_err(map_err)?;
    Ok(count.unsigned_abs())
}
