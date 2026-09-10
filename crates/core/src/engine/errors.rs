//! Shared storage-error → protocol-error mapping for engine verbs.
//!
//! Write verbs funnel storage failures through [`map_write_storage_error`]
//! so one taxonomy decides which failures are caller-fixable.

use crate::StorageError;
use crate::error::ProtocolError;
use crate::publication::PublicationError;

/// Map a refused publication capture onto the public protocol surface.
///
/// `ErrorCode` is deliberately exhaustive at every transport, so a new code
/// would cost every adapter a rendering decision this refusal does not
/// need. The split that matters is caller-fixable versus deployment fault:
///
/// - an oversized export, a failed export and a listenable write on the
///   receipt-only route are things the CALLER (or the flavor author) can
///   fix, and read as `invalid_argument`;
/// - an unbound source and an exhausted outbox are the deployment's, and
///   read as `internal` — the message names which, so backpressure is not
///   mistaken for a bug in the request.
#[must_use]
pub(crate) fn map_publication_error(err: &PublicationError) -> ProtocolError {
    match err {
        PublicationError::PayloadTooLarge { .. }
        | PublicationError::ExportFailed(_)
        | PublicationError::UntypedListenableWrite { .. } => {
            ProtocolError::invalid_argument("publication", err.to_string())
        }
        PublicationError::SourceUnbound { .. } | PublicationError::CapacityExhausted { .. } => {
            ProtocolError::internal(err.to_string())
        }
    }
}

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
        StorageError::ScopeMissing { kind, id } => ProtocolError::scope_not_registered(&kind, id),
        StorageError::IdempotencyConflict { request_id } => {
            ProtocolError::idempotency_conflict(request_id)
        }
        StorageError::ConstraintViolation(message) | StorageError::Conflict(message) => {
            ProtocolError::invalid_argument(field, message)
        }
        StorageError::Suppressed(message) => ProtocolError::suppressed(message),
        StorageError::PublicationRefused(ref publication) => map_publication_error(publication),
        // A transient deadlock/serialization failure that outlived the
        // bounded storage retry surfaces as an internal (retry-later)
        // fault.
        StorageError::Retryable(message)
        | StorageError::Unavailable(message)
        | StorageError::Internal(message) => ProtocolError::internal(message),
        StorageError::SchemaResetRequired { details } => ProtocolError::internal(details),
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
                StorageError::ConstraintViolation("duplicate natural key".into()),
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
            // A refused capture is caller-fixable when the payload is at
            // fault and a deployment fault otherwise; neither collapses
            // into an opaque `Internal(String)`.
            (
                StorageError::PublicationRefused(
                    crate::publication::PublicationError::PayloadTooLarge { bytes: 2, max: 1 },
                ),
                ErrorCode::InvalidArgument,
            ),
            (
                StorageError::PublicationRefused(
                    crate::publication::PublicationError::CapacityExhausted {
                        pending: 10,
                        max: 10,
                    },
                ),
                ErrorCode::Internal,
            ),
        ];
        for (err, expected) in cases {
            let mapped = map_write_storage_error(err, "field", "row not found");
            assert_eq!(mapped.code, expected, "{}", mapped.message);
        }
    }

    #[test]
    fn field_and_not_found_labels_reach_the_message() {
        let invalid = map_write_storage_error(
            StorageError::ConstraintViolation("duplicate natural key".into()),
            "fact",
            "fact row not found",
        );
        assert!(invalid.message.contains("fact"));
        assert!(invalid.message.contains("duplicate natural key"));

        let missing = map_write_storage_error(StorageError::NotFound, "fact", "fact row not found");
        assert_eq!(missing.message, "fact row not found");
    }
}
