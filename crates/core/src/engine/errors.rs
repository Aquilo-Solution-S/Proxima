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
