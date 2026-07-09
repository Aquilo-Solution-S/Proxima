use crate::ToolError;

use super::handles::ResolveError;

#[derive(Debug, thiserror::Error)]
pub enum McpToolError {
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("tool not authorized: {0}")]
    NotAuthorized(String),
    #[error("{0}")]
    Resolve(ResolveError),
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
    InvalidRequest,
    Internal,
}

impl McpToolError {
    #[must_use]
    pub fn kind(&self) -> McpToolErrorKind {
        match self {
            Self::InvalidInput(_) | Self::Resolve(_) => McpToolErrorKind::InvalidInput,
            Self::NotAuthorized(_) | Self::LayeringViolation(_) | Self::Unavailable(_) => {
                McpToolErrorKind::InvalidRequest
            }
            Self::Protocol(e) => match e.code {
                crate::error::ErrorCode::InvalidArgument => McpToolErrorKind::InvalidInput,
                crate::error::ErrorCode::Internal => McpToolErrorKind::Internal,
                crate::error::ErrorCode::AuthRequired
                | crate::error::ErrorCode::Forbidden
                | crate::error::ErrorCode::UnknownSchema
                | crate::error::ErrorCode::AlreadyIngested
                | crate::error::ErrorCode::IdempotencyConflict
                | crate::error::ErrorCode::NotFound
                | crate::error::ErrorCode::ToolNotRegistered
                | crate::error::ErrorCode::TriggerConflict
                | crate::error::ErrorCode::DuplicateTriggerInRequest
                | crate::error::ErrorCode::Suppressed => McpToolErrorKind::InvalidRequest,
            },
            Self::Storage(storage) => match storage {
                crate::StorageError::ConstraintViolation(_) | crate::StorageError::NotFound => {
                    McpToolErrorKind::InvalidInput
                }
                crate::StorageError::Conflict(_) | crate::StorageError::Suppressed(_) => {
                    McpToolErrorKind::InvalidRequest
                }
                crate::StorageError::Unavailable(_)
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
            McpToolErrorKind::InvalidInput | McpToolErrorKind::InvalidRequest => self.to_string(),
            McpToolErrorKind::Internal => "internal server error".to_string(),
        }
    }
}

impl From<ResolveError> for McpToolError {
    fn from(e: ResolveError) -> Self {
        McpToolError::Resolve(e)
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
