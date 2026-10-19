//! Shared storage-error → protocol-error mapping for engine verbs.
//!
//! Write verbs funnel storage failures through [`map_write_storage_error`]
//! so one taxonomy decides which failures are caller-fixable.

use crate::StorageError;
use crate::error::ProtocolError;
use crate::publication::PublicationError;

/// Map a refused publication capture onto the public protocol surface.
///
/// - an oversized export, a failed export and a listenable write on the
///   receipt-only route are things the CALLER (or the flavor author) can
///   fix, and read as `invalid_argument`;
/// - an exhausted outbox is backpressure on a legal write, and reads as
///   `capacity_exhausted` (MCP `-32000`, REST 503) so a caller backs off
///   instead of opening an incident (docs/host-integration.md);
/// - an unbound source is the deployment's, and reads as `internal`.
#[must_use]
pub(crate) fn map_publication_error(err: &PublicationError) -> ProtocolError {
    match err {
        PublicationError::PayloadTooLarge { .. }
        | PublicationError::ExportFailed(_)
        | PublicationError::UntypedListenableWrite { .. } => {
            ProtocolError::invalid_argument("publication", err.to_string())
        }
        PublicationError::CapacityExhausted { .. } => {
            ProtocolError::capacity_exhausted(err.to_string())
        }
        PublicationError::SourceUnbound { .. } => ProtocolError::internal(err.to_string()),
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
        StorageError::GoalChildrenOpen { .. } | StorageError::GoalChildrenAllAbandoned => {
            ProtocolError::invalid_argument(field, err.to_string())
        }
        StorageError::MembershipIsolation => ProtocolError::invalid_argument(
            "transaction_isolation",
            "membership writes require READ COMMITTED isolation",
        ),
        StorageError::MembershipForbidden(message) => ProtocolError::forbidden(message),
        StorageError::Suppressed(message) => ProtocolError::suppressed(message),
        StorageError::PublicationRefused(ref publication) => map_publication_error(publication),
        // A transient deadlock/serialization failure that outlived the
        // bounded storage retry surfaces as an internal (retry-later)
        // fault.
        StorageError::Retryable(message)
        | StorageError::Unavailable(message)
        | StorageError::Internal(message) => ProtocolError::internal(message),
        StorageError::SchemaResetRequired { details } => ProtocolError::internal(details),
        StorageError::MemoryIdCollision => ProtocolError::internal("memory UUID collision"),
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
    use super::*;
    use crate::error::ErrorCode;
    use crate::mcp::{McpToolError, McpToolErrorKind};

    /// Every shipped write reaches the client through `Engine`, so a
    /// refusal must read the same there as on the direct storage path:
    /// one class per refusal, whichever channel carries it.
    #[test]
    fn engine_and_storage_paths_classify_each_publication_refusal_alike() {
        for refusal in [
            PublicationError::PayloadTooLarge { bytes: 9, max: 8 },
            PublicationError::CapacityExhausted { pending: 3, max: 3 },
            PublicationError::ExportFailed("nope".into()),
            PublicationError::UntypedListenableWrite {
                schema_id: "x/y-v1".into(),
            },
            PublicationError::SourceUnbound {
                schema_id: "x/y-v1".into(),
            },
        ] {
            let engine = McpToolError::Protocol(map_publication_error(&refusal));
            let storage = McpToolError::Storage(StorageError::PublicationRefused(refusal));
            assert_eq!(engine.kind(), storage.kind(), "for {storage}");
        }
    }

    /// Backpressure is retry-later, not an incident, and the caller keeps
    /// the backlog depth it needs to decide how long to wait.
    #[test]
    fn exhausted_outbox_reads_as_capacity_exhausted_with_its_depth() {
        let err =
            map_publication_error(&PublicationError::CapacityExhausted { pending: 3, max: 3 });
        assert_eq!(err.code, ErrorCode::CapacityExhausted);

        let err = McpToolError::Protocol(err);
        assert_eq!(err.kind(), McpToolErrorKind::CapacityExhausted);
        assert!(err.client_message().contains("3 pending, limit 3"), "{err}");
    }
}
