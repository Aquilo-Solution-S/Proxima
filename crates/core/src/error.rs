//! Engine-wide error envelope per docs/14 §"Error envelope".

#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    thiserror::Error,
    serde::Serialize,
    serde::Deserialize,
    specta::Type,
)]
#[error("{code:?}: {message}")]
pub struct ProtocolError {
    pub code: ErrorCode,
    pub message: String,
    pub request_id: Option<String>,
}

/// Subset of docs/14's `ErrorCode` exercised in M1. Additional
/// variants land with the verbs that raise them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type)]
pub enum ErrorCode {
    AuthRequired,
    Forbidden,
    UnknownSchema,
    AlreadyIngested,
    IdempotencyConflict,
    NotFound,
    InvalidArgument,
    ToolNotRegistered,
    TriggerConflict,
    DuplicateTriggerInRequest,
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

    pub fn unknown_schema(schema_id: impl AsRef<str>, version: u32) -> Self {
        Self {
            code: ErrorCode::UnknownSchema,
            message: format!("schema not registered: {} v{}", schema_id.as_ref(), version),
            request_id: None,
        }
    }

    pub fn idempotency_conflict(request_id: impl AsRef<str>) -> Self {
        Self {
            code: ErrorCode::IdempotencyConflict,
            message: format!(
                "request_id already used with different body: {}",
                request_id.as_ref(),
            ),
            request_id: None,
        }
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self {
            code: ErrorCode::NotFound,
            message: message.into(),
            request_id: None,
        }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self {
            code: ErrorCode::Internal,
            message: message.into(),
            request_id: None,
        }
    }

    pub fn invalid_argument(field: impl AsRef<str>, reason: impl AsRef<str>) -> Self {
        Self {
            code: ErrorCode::InvalidArgument,
            message: format!("invalid argument {}: {}", field.as_ref(), reason.as_ref()),
            request_id: None,
        }
    }

    pub fn tool_not_registered(tool_id: impl AsRef<str>) -> Self {
        Self {
            code: ErrorCode::ToolNotRegistered,
            message: format!("tool not registered: {}", tool_id.as_ref()),
            request_id: None,
        }
    }

    pub fn trigger_conflict(trigger_kind: impl AsRef<str>, trigger_id: impl AsRef<str>) -> Self {
        Self {
            code: ErrorCode::TriggerConflict,
            message: format!(
                "trigger conflict: {} {}",
                trigger_kind.as_ref(),
                trigger_id.as_ref()
            ),
            request_id: None,
        }
    }

    pub fn duplicate_trigger_in_request(
        trigger_kind: impl AsRef<str>,
        trigger_id: impl AsRef<str>,
    ) -> Self {
        Self {
            code: ErrorCode::DuplicateTriggerInRequest,
            message: format!(
                "duplicate trigger in request: {} {}",
                trigger_kind.as_ref(),
                trigger_id.as_ref()
            ),
            request_id: None,
        }
    }
}
