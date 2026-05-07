//! Per-(owner, `type_id`, `instance_id`) advisory locking for wakes.
//!
//! Sessions in `PostgreSQL` hold session-level advisory locks; the lock is
//! bound to the connection that took it. Sqlx's pool may hand the same
//! connection to a follow-up wake (which would re-take its own lock,
//! since `pg_advisory_lock` is reentrant within a session) OR a
//! different one, in which case acquiring the lock would block until
//! the lock-holding connection is returned and the unlock can fire.
//!
//! To keep the bookkeeping correct we acquire a dedicated
//! `PoolConnection`, take the lock on it, and stash the connection
//! inside the returned guard. The guard's `Drop` impl spawns an async
//! task that reuses the same connection to release the lock and then
//! drops the connection back to the pool.

use std::sync::Mutex;

use proxima_core::personality::PersonalityRef;
use proxima_core::storage::WakeLockGuard;
use proxima_core::{Owner, Principal, StorageError};
use sqlx::PgPool;
use sqlx::pool::PoolConnection;
use sqlx::postgres::Postgres;

use crate::error::map_err;

fn instance_lock_key(owner: &Owner, instance: &PersonalityRef) -> i64 {
    let principal_bytes = match &owner.principal {
        Principal::User(u) => u.into_inner().as_bytes().to_vec(),
        Principal::Group(g) => g.into_inner().as_bytes().to_vec(),
    };
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"proxima-wake-lock\x00");
    hasher.update(&principal_bytes);
    hasher.update(owner.org_id.into_inner().as_bytes());
    hasher.update(instance.personality_type_id.as_bytes());
    hasher.update(instance.personality_instance_id.into_inner().as_bytes());
    let digest = hasher.finalize();
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&digest.as_bytes()[0..8]);
    i64::from_be_bytes(buf)
}

pub async fn acquire_wake_lock(
    pool: &PgPool,
    owner: &Owner,
    instance: &PersonalityRef,
) -> Result<WakeLockGuard, StorageError> {
    let key = instance_lock_key(owner, instance);
    let mut conn = pool.acquire().await.map_err(map_err)?;
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(key)
        .execute(&mut *conn)
        .await
        .map_err(map_err)?;
    // Stash the lock-holding connection inside the guard so the unlock
    // task uses the same session that took the lock.
    let conn_slot: Mutex<Option<PoolConnection<Postgres>>> = Mutex::new(Some(conn));
    let release: Box<dyn FnOnce() + Send + Sync> = Box::new(move || {
        let Some(mut conn) = conn_slot.lock().ok().and_then(|mut slot| slot.take()) else {
            return;
        };
        tokio::spawn(async move {
            if let Err(err) = sqlx::query("SELECT pg_advisory_unlock($1)")
                .bind(key)
                .execute(&mut *conn)
                .await
            {
                tracing::warn!(?err, key, "pg_advisory_unlock failed");
            }
        });
    });
    Ok(WakeLockGuard {
        release: Some(release),
    })
}
