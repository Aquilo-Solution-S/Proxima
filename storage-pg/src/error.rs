//! sqlx → `StorageError` mapping shared by every verb.

use proxima_core::StorageError;

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn map_err(e: sqlx::Error) -> StorageError {
    use sqlx::Error;
    match &e {
        Error::Database(db) if db.is_unique_violation() => {
            StorageError::ConstraintViolation(db.message().to_string())
        }
        Error::Database(db) if db.is_check_violation() => {
            StorageError::ConstraintViolation(db.message().to_string())
        }
        _ => StorageError::Internal(e.to_string()),
    }
}
