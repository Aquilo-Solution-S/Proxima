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
///
/// Deliberately NOT `#[non_exhaustive]`. Transport adapters map every code
/// onto their own vocabulary — the REST surface's RFC 9457 status table
/// (docs/17 §Status mapping) is the first — and that map is required to be
/// exhaustive with no wildcard arm, so adding a variant here is a compile
/// error until someone chooses its rendering. `#[non_exhaustive]` would
/// force exactly the wildcard that silently buckets a new code as an
/// internal server error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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

    /// A write named a flavor-owned lifecycle scope whose registry row is
    /// absent for this owner (docs/03 §Scope declaration). `NotFound`
    /// rather than a code of its own: the caller named a row that is not
    /// there, which is what `NotFound` means, and `ErrorCode` is
    /// deliberately exhaustive at every transport, so a new variant would
    /// cost a rendering decision this refusal does not need.
    pub fn scope_not_registered(kind: impl AsRef<str>, id: uuid::Uuid) -> Self {
        Self {
            code: ErrorCode::NotFound,
            message: format!("scope not registered: {}:{id}", kind.as_ref()),
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
