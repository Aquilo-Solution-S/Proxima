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
            Self::NotAuthorized(_) | Self::LayeringViolation(_) => McpToolErrorKind::InvalidRequest,
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
                | crate::error::ErrorCode::DuplicateTriggerInRequest => {
                    McpToolErrorKind::InvalidRequest
                }
            },
            Self::Storage(storage) => match storage {
                crate::StorageError::ConstraintViolation(_) | crate::StorageError::NotFound => {
                    McpToolErrorKind::InvalidInput
                }
                crate::StorageError::Conflict(_) => McpToolErrorKind::InvalidRequest,
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
