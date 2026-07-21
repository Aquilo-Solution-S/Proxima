//! Shared storage-error → protocol-error mapping for engine verbs.
//!
//! Every write verb funnels its storage failures through
//! [`map_write_storage_error`] so one taxonomy decides which failures are
//! caller-fixable. Before this module, three verbs carried their own
//! full-taxonomy copies while `fact_ingest`/`close_batch`/edge-append used
//! ad-hoc matches that collapsed caller errors (closed batch, concurrent
//! citation) into `Internal`.

use crate::StorageError;
use crate::error::ProtocolError;

/// Map a write-verb storage failure onto the public protocol surface.
/// `field` labels the caller-fixable `ConstraintViolation`/`Conflict`
/// messages (`invalid_argument`); `not_found_message` names the missing
/// row without echoing storage internals.
pub(in crate::engine) fn map_write_storage_error(
    err: StorageError,
    field: &str,
    not_found_message: &str,
) -> ProtocolError {
    match err {
        StorageError::NotFound => ProtocolError::not_found(not_found_message),
        StorageError::IdempotencyConflict { request_id } => {
            ProtocolError::idempotency_conflict(request_id)
        }
        StorageError::ConstraintViolation(message) | StorageError::Conflict(message) => {
            ProtocolError::invalid_argument(field, message)
        }
        StorageError::Suppressed(message) => ProtocolError::suppressed(message),
        // A transient deadlock/serialization failure that outlived the
        // bounded storage retry surfaces as an internal (retry-later)
        // fault.
        StorageError::Retryable(message)
        | StorageError::Unavailable(message)
        | StorageError::Internal(message) => ProtocolError::internal(message),
        StorageError::V004ResetRequired { details } => ProtocolError::internal(details),
    }
}

/// Read-path collapse: reads surface every storage failure as `Internal`
/// with the verb name as context (a read has no caller-fixable storage
/// failure class to distinguish).
pub(in crate::engine) fn internal_storage_error(
    context: &str,
    err: &StorageError,
) -> ProtocolError {
    ProtocolError::internal(format!("{context}: {err}"))
}

#[cfg(test)]
mod tests {
    use super::map_write_storage_error;
    use crate::StorageError;
    use crate::error::ErrorCode;

    /// The category table every write verb inherits: caller-fixable
    /// storage failures must not surface as `Internal`. This is the
    /// invariant that was silently broken on the fact-ingest path before
    /// the mapper was shared.
    #[test]
    fn write_storage_errors_keep_their_categories() {
        let cases = [
            (StorageError::NotFound, ErrorCode::NotFound),
            (
                StorageError::IdempotencyConflict {
                    request_id: "req-1".into(),
                },
                ErrorCode::IdempotencyConflict,
            ),
            (
                StorageError::ConstraintViolation("closed batch".into()),
                ErrorCode::InvalidArgument,
            ),
            (
                StorageError::Conflict("citation attached concurrently".into()),
                ErrorCode::InvalidArgument,
            ),
            (
                StorageError::Suppressed("suppressed".into()),
                ErrorCode::Suppressed,
            ),
            (
                StorageError::Retryable("serialization failure".into()),
                ErrorCode::Internal,
            ),
            (
                StorageError::Unavailable("pool exhausted".into()),
                ErrorCode::Internal,
            ),
            (StorageError::Internal("boom".into()), ErrorCode::Internal),
        ];
        for (err, expected) in cases {
            let mapped = map_write_storage_error(err, "field", "row not found");
            assert_eq!(mapped.code, expected, "{}", mapped.message);
        }
    }

    #[test]
    fn field_and_not_found_labels_reach_the_message() {
        let invalid = map_write_storage_error(
            StorageError::ConstraintViolation("cannot ingest into closed batch".into()),
            "fact",
            "fact row not found",
        );
        assert!(invalid.message.contains("fact"));
        assert!(invalid.message.contains("closed batch"));

        let missing = map_write_storage_error(StorageError::NotFound, "fact", "fact row not found");
        assert_eq!(missing.message, "fact row not found");
    }
}
