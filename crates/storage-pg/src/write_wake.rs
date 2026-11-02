//! Commit a write and wake the in-process drains
//! ([`proxiima_core::write_wake`]).

use proxiima_core::StorageError;
use sqlx::{Postgres, Transaction};

/// Commit `tx`, then signal [`proxiima_core::write_wake::write_committed`].
/// Use it for transactions that write rows a background drain claims (the
/// publication outbox, embedding jobs) or may write them; a wake for a write
/// that enqueued nothing costs a drain one empty claim.
pub(crate) async fn commit(tx: Transaction<'_, Postgres>) -> Result<(), StorageError> {
    tx.commit().await.map_err(crate::error::map_err)?;
    proxiima_core::write_wake::write_committed();
    Ok(())
}
