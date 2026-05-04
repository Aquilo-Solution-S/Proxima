//! Engine-wide error envelope per docs/14 §"Error envelope".

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{code:?}: {message}")]
pub struct ProtocolError {
    pub code: ErrorCode,
    pub message: String,
    pub request_id: Option<String>,
}

/// Subset of docs/14's `ErrorCode` exercised in M1. Additional
/// variants land with the verbs that raise them
/// (`UnknownSchema` with M3 schema validation,
/// `IdempotencyConflict` with M2 `GoalWrite`, etc.).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    AuthRequired,
    Forbidden,
    Internal,
}

impl ProtocolError {
    pub fn forbidden(message: impl Into<String>) -> Self {
        Self {
            code: ErrorCode::Forbidden,
            message: message.into(),
            request_id: None,
        }
    }

    pub fn auth_required() -> Self {
        Self {
            code: ErrorCode::AuthRequired,
            message: "authentication required".into(),
            request_id: None,
        }
    }
}
