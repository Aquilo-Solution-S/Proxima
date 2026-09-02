//! An explicit ending for every transaction a command opens.
//!
//! A command that reports `committed: false` is making a claim about the
//! database, and a command that returns `Retryable` is telling a retry loop
//! it may re-enter immediately. Both claims depend on the transaction being
//! *over* — server-side — before the call returns. [`in_transaction`] makes
//! the two endings structural instead of hand-written per exit.
//!
//! **Why this is not a `Drop` guard.** sqlx's `Drop` for a transaction only
//! *queues* `ROLLBACK` into the connection's write buffer; the buffer is
//! flushed later, from the task spawned when the pooled connection is
//! returned. Nobody awaits it. Hydration takes `pg_advisory_xact_lock` over
//! its whole footprint, and those locks are released only when the
//! transaction ends server-side, so a drop-only guard would hand the command
//! port's immediate retry a footprint still locked by the abandoned
//! transaction — burning retry attempts against `statement_timeout` while an
//! unrelated spawned task gets around to the flush. A deliberate abort has
//! the second problem: its report claims nothing was written, which an
//! unawaited rollback does not prove. Async `Drop` is not stable and
//! `block_on` inside `Drop` under tokio deadlocks, so the ending has to be
//! awaited by a combinator. `Drop` stays the backstop for a panic or a
//! cancellation.

use std::future::Future;

use proxima_core::StorageError;
use sqlx::{PgPool, Postgres, Transaction};

use crate::error::map_err;

/// What a transaction body decided should happen to its own work.
///
/// Both arms carry the value the command returns: an abort is a *result*,
/// not a failure — the set that wrote nothing still reports one outcome per
/// requested id.
pub(crate) enum TxOutcome<T> {
    Commit(T),
    Abort(T),
}

/// Run `body` in one transaction and await whichever ending it asks for.
///
/// The two rollback forms are not interchangeable, and the split is not
/// awaited-vs-not — all three endings are awaited. It is who owns the
/// failure: a deliberate abort propagates a rollback error, because a report
/// claiming `committed: false` must not rest on an unproven rollback; a body
/// that is already failing swallows it, because a rollback error must not
/// mask the original — above all not a `Retryable` a caller's retry loop
/// depends on.
///
/// The transaction is handed over by value and handed back, rather than lent
/// as `&mut`. A borrowing body needs a higher-ranked bound
/// (`for<'a> FnOnce(&'a mut Transaction) -> Future + 'a`), and a
/// higher-ranked body future cannot be proven `Send` through the
/// `async_trait` command ports these commands are reached by. By value there
/// is no lifetime to quantify over: the body owns the transaction for its
/// duration, borrows it locally as it pleases, and the type system — not a
/// convention — makes it hand the transaction back, so exactly one ending is
/// awaited and it is awaited here.
pub(crate) async fn in_transaction<T, F, Fut>(pool: &PgPool, body: F) -> Result<T, StorageError>
where
    F: FnOnce(Transaction<'static, Postgres>) -> Fut,
    Fut: Future<
        Output = (
            Transaction<'static, Postgres>,
            Result<TxOutcome<T>, StorageError>,
        ),
    >,
{
    let tx = pool.begin().await.map_err(map_err)?;
    let (tx, outcome) = body(tx).await;
    match outcome {
        // A deliberate abort's report claims nothing changed; prove it.
        Ok(TxOutcome::Abort(value)) => {
            tx.rollback().await.map_err(map_err)?;
            Ok(value)
        }
        Ok(TxOutcome::Commit(value)) => {
            tx.commit().await.map_err(map_err)?;
            Ok(value)
        }
        // Already failing: a rollback error must not mask this one.
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}
