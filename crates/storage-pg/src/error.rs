//! sqlx → `StorageError` mapping shared by every verb.

use proxima_core::StorageError;

pub(crate) fn internal(e: impl std::fmt::Display) -> StorageError {
    StorageError::Internal(e.to_string())
}

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn map_err(e: sqlx::Error) -> StorageError {
    use sqlx::Error;
    match &e {
        Error::Database(db)
            if db.is_unique_violation()
                && db.constraint() == Some("memories_ftoa_batch_exclusive_uidx") =>
        {
            StorageError::Conflict(db.message().to_string())
        }
        Error::Database(db) if db.is_unique_violation() => {
            StorageError::ConstraintViolation(db.message().to_string())
        }
        Error::Database(db) if db.is_check_violation() => {
            StorageError::ConstraintViolation(db.message().to_string())
        }
        // FK violations are caller-facing (referenced row missing / retracted),
        // not a 5xx-shaped internal fault — surface as Conflict.
        Error::Database(db) if db.is_foreign_key_violation() => {
            StorageError::Conflict(db.message().to_string())
        }
        // Deadlock (40P01) and serialization failure (40001) are transient:
        // the whole transaction can be re-run. See `with_bounded_retry`.
        Error::Database(db) if is_retryable_sqlstate(db.code().as_deref()) => {
            StorageError::Retryable(db.message().to_string())
        }
        _ => StorageError::Internal(e.to_string()),
    }
}

/// SQLSTATE codes that mark a transient, retry-after-rollback failure.
fn is_retryable_sqlstate(code: Option<&str>) -> bool {
    matches!(code, Some("40P01" | "40001"))
}

/// Bounded retry budget for [`with_bounded_retry`]: the total number of
/// attempts (initial try + retries) for a transient transaction.
pub(crate) const MAX_STORAGE_ATTEMPTS: usize = 3;

/// Re-run `op` (a full begin→body→commit transaction) up to
/// [`MAX_STORAGE_ATTEMPTS`] times while it fails with
/// [`StorageError::Retryable`]. Any other outcome (including a `Retryable`
/// on the final attempt) is returned as-is.
///
/// `op` must be self-contained: it opens its own transaction each call, so a
/// rolled-back deadlocked attempt leaves no partial state behind.
pub(crate) async fn with_bounded_retry<T, F, Fut>(mut op: F) -> Result<T, StorageError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, StorageError>>,
{
    let mut attempt = 1;
    loop {
        match op().await {
            Err(StorageError::Retryable(_)) if attempt < MAX_STORAGE_ATTEMPTS => {
                attempt += 1;
            }
            outcome => return outcome,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_STORAGE_ATTEMPTS, is_retryable_sqlstate, with_bounded_retry};
    use proxima_core::StorageError;
    use std::cell::Cell;

    #[test]
    fn deadlock_and_serialization_sqlstates_are_retryable() {
        assert!(is_retryable_sqlstate(Some("40P01")));
        assert!(is_retryable_sqlstate(Some("40001")));
        assert!(!is_retryable_sqlstate(Some("23503")));
        assert!(!is_retryable_sqlstate(Some("23505")));
        assert!(!is_retryable_sqlstate(None));
    }

    #[tokio::test]
    async fn bounded_retry_stops_after_max_attempts() {
        let calls = Cell::new(0usize);
        let result: Result<(), StorageError> = with_bounded_retry(|| {
            calls.set(calls.get() + 1);
            async { Err(StorageError::Retryable("deadlock".into())) }
        })
        .await;
        assert!(matches!(result, Err(StorageError::Retryable(_))));
        assert_eq!(calls.get(), MAX_STORAGE_ATTEMPTS);
    }

    #[tokio::test]
    async fn bounded_retry_returns_first_success() {
        let calls = Cell::new(0usize);
        let result: Result<u8, StorageError> = with_bounded_retry(|| {
            calls.set(calls.get() + 1);
            async {
                if calls.get() < 2 {
                    Err(StorageError::Retryable("deadlock".into()))
                } else {
                    Ok(7)
                }
            }
        })
        .await;
        assert_eq!(result.unwrap(), 7);
        assert_eq!(calls.get(), 2);
    }

    #[tokio::test]
    async fn bounded_retry_does_not_retry_non_retryable() {
        let calls = Cell::new(0usize);
        let result: Result<(), StorageError> = with_bounded_retry(|| {
            calls.set(calls.get() + 1);
            async { Err(StorageError::Conflict("fk".into())) }
        })
        .await;
        assert!(matches!(result, Err(StorageError::Conflict(_))));
        assert_eq!(calls.get(), 1);
    }
}
