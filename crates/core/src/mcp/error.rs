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
                crate::StorageError::NotFound => McpToolErrorKind::NotFound,
                crate::StorageError::Conflict(_)
                | crate::StorageError::Suppressed(_)
                | crate::StorageError::IdempotencyConflict { .. } => {
                    McpToolErrorKind::InvalidRequest
                }
                crate::StorageError::Retryable(_)
                | crate::StorageError::Unavailable(_)
                | crate::StorageError::Internal(_)
                | crate::StorageError::V004ResetRequired { .. } => McpToolErrorKind::Internal,
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
            | McpToolErrorKind::InvalidRequest => self.to_string(),
            McpToolErrorKind::Internal => "internal server error".to_string(),
        }
    }
}

impl From<ToolError> for McpToolError {
    fn from(err: ToolError) -> Self {
        match err {
            ToolError::InvalidInput(message) => Self::InvalidInput(message),
            ToolError::NotAuthorized(tool) => Self::NotAuthorized(tool),
            ToolError::Protocol(err) => Self::Protocol(err),
            ToolError::LayeringViolation(message) => Self::LayeringViolation(message),
            ToolError::Storage(err) => Self::Storage(err),
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
}
