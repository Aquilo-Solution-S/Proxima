use crate::ToolError;

#[derive(Debug, thiserror::Error)]
pub enum McpToolError {
    #[error("invalid input: {0}")]
    InvalidInput(String),
    /// A well-formed reference to an entity that does not exist or is not
    /// visible to the caller (deliberately indistinguishable). Resource
    /// reads surface this as JSON-RPC `resource_not_found`; tool calls as
    /// `invalid_params`. The message names the wire handle, e.g.
    /// `memory F:<uuid> not found`.
    #[error("{0}")]
    NotFound(String),
    #[error("tool not authorized: {0}")]
    NotAuthorized(String),
    #[error("{0}")]
    Protocol(#[from] crate::error::ProtocolError),
    #[error("layering violation: {0}")]
    LayeringViolation(String),
    #[error("storage: {0}")]
    Storage(#[from] crate::StorageError),
    /// A required capability (e.g. a semantic-search embedding client) is not
    /// configured for this host. Unlike [`Self::Other`], its message is a
    /// caller-actionable precondition and is passed through verbatim rather
    /// than redacted to a generic internal-server-error.
    #[error("{0}")]
    Unavailable(String),
    #[error("{0}")]
    Other(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpToolErrorKind {
    InvalidInput,
    /// Well-formed reference to a missing (or invisible) entity.
    NotFound,
    InvalidRequest,
    /// Declared backpressure: the request was well-formed and legal, the
    /// deployment is momentarily unable to accept it, and the caller should
    /// slow down and retry. Its own kind so a client's retry and alerting
    /// policy can tell "the outbox is full" from "the substrate is broken";
    /// [`Self::Internal`] would page an operator for a queue depth.
    ///
    /// NOT the class of [`McpToolError::Unavailable`], which is a
    /// PERMANENT missing-capability precondition (no embedding client is
    /// configured) that retrying will never resolve — that stays
    /// [`Self::InvalidRequest`].
    CapacityExhausted,
    Internal,
}

impl McpToolError {
    #[must_use]
    pub fn kind(&self) -> McpToolErrorKind {
        match self {
            Self::InvalidInput(_) => McpToolErrorKind::InvalidInput,
            Self::NotFound(_) => McpToolErrorKind::NotFound,
            Self::NotAuthorized(_) | Self::LayeringViolation(_) | Self::Unavailable(_) => {
                McpToolErrorKind::InvalidRequest
            }
            Self::Protocol(e) => match e.code {
                crate::error::ErrorCode::InvalidArgument => McpToolErrorKind::InvalidInput,
                crate::error::ErrorCode::NotFound => McpToolErrorKind::NotFound,
                crate::error::ErrorCode::Internal => McpToolErrorKind::Internal,
                crate::error::ErrorCode::AuthRequired
                | crate::error::ErrorCode::Forbidden
                | crate::error::ErrorCode::UnknownSchema
                | crate::error::ErrorCode::AlreadyIngested
                | crate::error::ErrorCode::IdempotencyConflict
                | crate::error::ErrorCode::ToolNotRegistered
                | crate::error::ErrorCode::TriggerConflict
                | crate::error::ErrorCode::DuplicateTriggerInRequest
                | crate::error::ErrorCode::Suppressed => McpToolErrorKind::InvalidRequest,
            },
            Self::Storage(storage) => match storage {
                crate::StorageError::ConstraintViolation(_) => McpToolErrorKind::InvalidInput,
                crate::StorageError::NotFound | crate::StorageError::ScopeMissing { .. } => {
                    McpToolErrorKind::NotFound
                }
                crate::StorageError::Conflict(_)
                | crate::StorageError::Suppressed(_)
                | crate::StorageError::IdempotencyConflict { .. } => {
                    McpToolErrorKind::InvalidRequest
                }
                // A refused publication capture keeps the split the write
                // path already draws: an oversized or unexportable payload
                // and a listenable write on the receipt-only route are the
                // caller's (or the flavor author's) to fix; an unbound
                // source and an exhausted outbox are the deployment's.
                crate::StorageError::PublicationRefused(publication) => match publication {
                    crate::publication::PublicationError::PayloadTooLarge { .. }
                    | crate::publication::PublicationError::ExportFailed(_)
                    | crate::publication::PublicationError::UntypedListenableWrite { .. } => {
                        McpToolErrorKind::InvalidInput
                    }
                    crate::publication::PublicationError::SourceUnbound { .. } => {
                        McpToolErrorKind::Internal
                    }
                    // Backpressure, not a fault: the write was legal and the
                    // publisher is behind. A caller told "internal server
                    // error" learns nothing and retries immediately.
                    crate::publication::PublicationError::CapacityExhausted { .. } => {
                        McpToolErrorKind::CapacityExhausted
                    }
                },
                crate::StorageError::Retryable(_)
                | crate::StorageError::Unavailable(_)
                | crate::StorageError::Internal(_)
                | crate::StorageError::SchemaResetRequired { .. } => McpToolErrorKind::Internal,
            },
            Self::Other(_) => McpToolErrorKind::Internal,
        }
    }

    #[must_use]
    pub fn client_message(&self) -> String {
        if let Self::NotAuthorized(name) = self {
            return format!("tool {name} not authorized for this MCP token");
        }
        match self.kind() {
            McpToolErrorKind::InvalidInput
            | McpToolErrorKind::NotFound
            | McpToolErrorKind::InvalidRequest
            // Verbatim: the message carries the backlog depth and the
            // configured bound, which is exactly what a caller needs to
            // decide how long to wait.
            | McpToolErrorKind::CapacityExhausted => self.to_string(),
            McpToolErrorKind::Internal => "internal server error".to_string(),
        }
    }
}

impl From<ToolError> for McpToolError {
    fn from(err: ToolError) -> Self {
        match err {
            ToolError::InvalidInput(message) => Self::InvalidInput(message),
            ToolError::NotFound(message) => Self::NotFound(message),
            ToolError::NotAuthorized(tool) => Self::NotAuthorized(tool),
            ToolError::Protocol(err) => Self::Protocol(err),
            ToolError::LayeringViolation(message) => Self::LayeringViolation(message),
            ToolError::Storage(err) => Self::Storage(err),
            ToolError::Unavailable(message) => Self::Unavailable(message),
            ToolError::Other(message) => Self::Other(message),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{McpToolError, McpToolErrorKind};

    /// Missing-entity faults form their own kind so the resource path can
    /// surface JSON-RPC `resource_not_found` while tools keep
    /// `invalid_params` — and the message must reach the caller verbatim,
    /// never redacted to "internal server error".
    #[test]
    fn not_found_classifies_uniformly_across_sources() {
        let direct = McpToolError::NotFound("memory F:018f not found".into());
        assert_eq!(direct.kind(), McpToolErrorKind::NotFound);
        assert_eq!(direct.client_message(), "memory F:018f not found");

        let storage = McpToolError::Storage(crate::StorageError::NotFound);
        assert_eq!(storage.kind(), McpToolErrorKind::NotFound);

        let protocol = McpToolError::Protocol(crate::error::ProtocolError {
            code: crate::error::ErrorCode::NotFound,
            message: "goal G:018f not found".into(),
            request_id: None,
        });
        assert_eq!(protocol.kind(), McpToolErrorKind::NotFound);
    }

    #[test]
    fn unavailable_message_reaches_caller_verbatim() {
        let err = McpToolError::Unavailable(
            "semantic search unavailable: no embedding client is configured for this host".into(),
        );
        // Precondition faults classify as a well-formed-but-illegal request,
        // NOT an internal fault (which would redact the message).
        assert_eq!(err.kind(), McpToolErrorKind::InvalidRequest);
        assert_eq!(
            err.client_message(),
            "semantic search unavailable: no embedding client is configured for this host"
        );
        assert_ne!(err.client_message(), "internal server error");
    }

    /// Declared backpressure must not be indistinguishable from a
    /// substrate bug on the wire: an exhausted outbox is the deployment
    /// asking the caller to slow down, and its message names the backlog.
    #[test]
    fn an_exhausted_outbox_is_backpressure_not_an_internal_fault() {
        let err = McpToolError::Storage(crate::StorageError::PublicationRefused(
            crate::publication::PublicationError::CapacityExhausted {
                pending: 100_000,
                max: 100_000,
            },
        ));
        assert_eq!(err.kind(), McpToolErrorKind::CapacityExhausted);
        assert_ne!(err.client_message(), "internal server error");
        assert!(
            err.client_message().contains("100000"),
            "the backlog and the bound must reach the caller: {}",
            err.client_message()
        );
    }

    /// The other four refusals keep the classes they already had, so the
    /// new kind is one condition rather than a bucket.
    #[test]
    fn the_other_publication_refusals_keep_their_classes() {
        use crate::publication::PublicationError;

        for (refusal, expected) in [
            (
                PublicationError::PayloadTooLarge { bytes: 9, max: 8 },
                McpToolErrorKind::InvalidInput,
            ),
            (
                PublicationError::ExportFailed("nope".into()),
                McpToolErrorKind::InvalidInput,
            ),
            (
                PublicationError::UntypedListenableWrite {
                    schema_id: "x/y-v1".into(),
                },
                McpToolErrorKind::InvalidInput,
            ),
            (
                PublicationError::SourceUnbound {
                    schema_id: "x/y-v1".into(),
                },
                McpToolErrorKind::Internal,
            ),
        ] {
            let err = McpToolError::Storage(crate::StorageError::PublicationRefused(refusal));
            assert_eq!(err.kind(), expected, "for {err}");
        }
    }

    #[test]
    fn tool_error_not_found_and_unavailable_map_losslessly() {
        let not_found = McpToolError::from(crate::ToolError::NotFound("repo not found".into()));
        assert!(matches!(not_found, McpToolError::NotFound(ref m) if m == "repo not found"));
        assert_eq!(not_found.kind(), McpToolErrorKind::NotFound);
        assert_eq!(not_found.client_message(), "repo not found");

        let unavailable = McpToolError::from(crate::ToolError::Unavailable(
            "semantic search unavailable".into(),
        ));
        assert!(matches!(
            unavailable,
            McpToolError::Unavailable(ref m) if m == "semantic search unavailable"
        ));
        assert_eq!(unavailable.kind(), McpToolErrorKind::InvalidRequest);
        assert_eq!(unavailable.client_message(), "semantic search unavailable");
        assert_ne!(unavailable.client_message(), "internal server error");
    }
}
