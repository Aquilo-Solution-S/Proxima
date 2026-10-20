//! Store-wide maintenance singletons held as a lease row, not a session lock.
//!
//! A maintenance pass (the object-purge drain and upload sweep, the embedding
//! maintenance commands) must not overlap another pass of its family on the
//! same database, and it deletes objects with no database transaction open
//! (#482), so no transaction-scoped lock can span it. A session advisory lock
//! did, on a connection detached from the pool; behind a transaction-mode
//! pooler such a lock stays on a shared server connection and is granted again
//! to whichever client reuses it.
//!
//! The lease is a row of `proxima_core.maintenance_lease` written in short
//! platform transactions: taken when missing or expired, renewed by its holder
//! while the pass runs, and deleted when the pass ends. A holder that dies
//! stops renewing, and its lease expires after [`MAINTENANCE_LEASE_TTL`].

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use proxima_core::StorageError;
use uuid::Uuid;

use crate::PgPlatformScope;
use crate::error::map_err;

/// How long a lease outlives its last renewal. A pass renews every third of
/// it, so a holder must miss two renewals in a row before another process
/// can take the lease.
pub(crate) const MAINTENANCE_LEASE_TTL: Duration = Duration::from_mins(1);

const RENEW_EVERY: Duration = Duration::from_secs(20);

/// Which maintenance family a lease serializes. The two families may run
/// concurrently; each never overlaps itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LeaseName {
    Storage,
    Embedding,
}

impl LeaseName {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Storage => "storage",
            Self::Embedding => "embedding",
        }
    }
}

/// A held lease. Renewed in the background while held; released by
/// [`Self::release`] or, best effort, on drop. Expiry is the backstop.
pub(crate) struct MaintenanceLease {
    platform: PgPlatformScope,
    name: LeaseName,
    holder: Uuid,
    ttl: Duration,
    lost: Arc<AtomicBool>,
    renewer: Option<tokio::task::JoinHandle<()>>,
    released: bool,
}

impl MaintenanceLease {
    /// Take `name` if no live lease holds it.
    pub(crate) async fn try_acquire(
        platform: &PgPlatformScope,
        name: LeaseName,
    ) -> Result<Option<Self>, StorageError> {
        let (ttl, renew_every) = (MAINTENANCE_LEASE_TTL, RENEW_EVERY);
        let holder = Uuid::now_v7();
        let mut tx = platform.begin().await?;
        let taken: Option<Uuid> = sqlx::query_scalar(
            "INSERT INTO proxima_core.maintenance_lease AS lease (name, holder, expires_at)
                  VALUES ($1, $2, clock_timestamp() + make_interval(secs => $3))
             ON CONFLICT (name) DO UPDATE
                     SET holder = EXCLUDED.holder, expires_at = EXCLUDED.expires_at
                   WHERE lease.expires_at < clock_timestamp()
               RETURNING holder",
        )
        .bind(name.as_str())
        .bind(holder)
        .bind(ttl.as_secs_f64())
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_err)?;
        tx.commit().await.map_err(map_err)?;
        if taken != Some(holder) {
            return Ok(None);
        }
        let lost = Arc::new(AtomicBool::new(false));
        let renewer = tokio::spawn(renew_until_lost(
            platform.clone(),
            name,
            holder,
            ttl,
            renew_every,
            lost.clone(),
        ));
        Ok(Some(Self {
            platform: platform.clone(),
            name,
            holder,
            ttl,
            lost,
            renewer: Some(renewer),
            released: false,
        }))
    }

    /// Extend the lease now.
    ///
    /// # Errors
    ///
    /// [`StorageError::Unavailable`] when the lease is no longer this
    /// holder's: it expired and another process took it, or it was released.
    /// A pass stops on this error rather than run beside another pass.
    pub(crate) async fn renew(&self) -> Result<(), StorageError> {
        if self.lost.load(Ordering::Acquire)
            || !renew(&self.platform, self.name, self.holder, self.ttl).await?
        {
            self.lost.store(true, Ordering::Release);
            return Err(StorageError::Unavailable(format!(
                "the {} maintenance lease expired and another pass may hold it",
                self.name.as_str()
            )));
        }
        Ok(())
    }

    /// Delete the lease so the next pass need not wait for it to expire.
    pub(crate) async fn release(mut self) -> Result<(), StorageError> {
        self.released = true;
        if let Some(renewer) = self.renewer.take() {
            renewer.abort();
        }
        release(&self.platform, self.name, self.holder).await
    }
}

impl Drop for MaintenanceLease {
    fn drop(&mut self) {
        if let Some(renewer) = self.renewer.take() {
            renewer.abort();
        }
        if self.released {
            return;
        }
        // Best effort: an async delete cannot run in Drop. Without a runtime
        // (or if the delete fails) the lease simply expires.
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let platform = self.platform.clone();
            let (name, holder) = (self.name, self.holder);
            runtime.spawn(async move {
                if let Err(error) = release(&platform, name, holder).await {
                    tracing::debug!(
                        lease = name.as_str(),
                        %error,
                        "maintenance lease release failed; it expires instead"
                    );
                }
            });
        }
    }
}

async fn renew(
    platform: &PgPlatformScope,
    name: LeaseName,
    holder: Uuid,
    ttl: Duration,
) -> Result<bool, StorageError> {
    let mut tx = platform.begin().await?;
    let renewed = sqlx::query(
        "UPDATE proxima_core.maintenance_lease
            SET expires_at = clock_timestamp() + make_interval(secs => $3)
          WHERE name = $1 AND holder = $2 AND expires_at >= clock_timestamp()",
    )
    .bind(name.as_str())
    .bind(holder)
    .bind(ttl.as_secs_f64())
    .execute(&mut *tx)
    .await
    .map_err(map_err)?
    .rows_affected()
        == 1;
    tx.commit().await.map_err(map_err)?;
    Ok(renewed)
}

async fn release(
    platform: &PgPlatformScope,
    name: LeaseName,
    holder: Uuid,
) -> Result<(), StorageError> {
    let mut tx = platform.begin().await?;
    sqlx::query("DELETE FROM proxima_core.maintenance_lease WHERE name = $1 AND holder = $2")
        .bind(name.as_str())
        .bind(holder)
        .execute(&mut *tx)
        .await
        .map_err(map_err)?;
    tx.commit().await.map_err(map_err)
}

/// Renew every `every` until a renewal finds the lease gone. A failed
/// renewal attempt (the database unreachable) is retried at the next tick;
/// if the lease expires meanwhile, the next renewal reports it lost.
async fn renew_until_lost(
    platform: PgPlatformScope,
    name: LeaseName,
    holder: Uuid,
    ttl: Duration,
    every: Duration,
    lost: Arc<AtomicBool>,
) {
    loop {
        tokio::time::sleep(every).await;
        match renew(&platform, name, holder, ttl).await {
            Ok(true) => {}
            Ok(false) => {
                lost.store(true, Ordering::Release);
                tracing::warn!(
                    lease = name.as_str(),
                    "maintenance lease expired before renewal; the pass stops at its next check"
                );
                return;
            }
            Err(error) => {
                tracing::warn!(lease = name.as_str(), %error, "maintenance lease renewal failed");
            }
        }
    }
}
