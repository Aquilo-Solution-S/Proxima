//! Engine-wide error envelope per docs/14 §"Error envelope".

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, serde::Serialize, serde::Deserialize)]
#[error("{code:?}: {message}")]
pub struct ProtocolError {
    pub code: ErrorCode,
    pub message: String,
    pub request_id: Option<String>,
}

/// Subset of docs/14's `ErrorCode` exercised so far. Additional
/// variants land with the verbs that raise them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "snake_case")]
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
    Suppressed,
    Internal,
}

impl ErrorCode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AuthRequired => "auth_required",
            Self::Forbidden => "forbidden",
            Self::UnknownSchema => "unknown_schema",
            Self::AlreadyIngested => "already_ingested",
            Self::IdempotencyConflict => "idempotency_conflict",
            Self::NotFound => "not_found",
            Self::InvalidArgument => "invalid_argument",
            Self::ToolNotRegistered => "tool_not_registered",
            Self::TriggerConflict => "trigger_conflict",
            Self::DuplicateTriggerInRequest => "duplicate_trigger_in_request",
            Self::Suppressed => "suppressed",
            Self::Internal => "internal",
        }
    }
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

    pub fn suppressed(message: impl Into<String>) -> Self {
        Self {
            code: ErrorCode::Suppressed,
            message: message.into(),
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

#[cfg(test)]
mod tests {
    use super::ErrorCode;

    const ALL_ERROR_CODES: &[ErrorCode] = &[
        ErrorCode::AuthRequired,
        ErrorCode::Forbidden,
        ErrorCode::UnknownSchema,
        ErrorCode::AlreadyIngested,
        ErrorCode::IdempotencyConflict,
        ErrorCode::NotFound,
        ErrorCode::InvalidArgument,
        ErrorCode::ToolNotRegistered,
        ErrorCode::TriggerConflict,
        ErrorCode::DuplicateTriggerInRequest,
        ErrorCode::Suppressed,
        ErrorCode::Internal,
    ];

    #[test]
    fn error_code_as_str_matches_json_wire() {
        for code in ALL_ERROR_CODES {
            let wire = serde_json::to_value(code).expect("serialize error code");
            assert_eq!(wire, serde_json::Value::String(code.as_str().to_string()));
        }
    }
}
