//! RFC 9457 `application/problem+json` rendering of the dispatch seam's
//! errors.
//!
//! Two rules carry the whole module and neither is negotiable:
//!
//! - `detail` is [`McpToolError::client_message`], never `Display` and never
//!   a formatted source chain. That single call is what keeps an
//!   internal-kind error redacted to `internal server error` before the HTTP
//!   layer can see the storage DSN inside it, and what lets `Unavailable`
//!   through verbatim because it is a caller-actionable precondition.
//! - The status map matches on **variants**, exhaustively, with no wildcard
//!   arm. `McpToolErrorKind` is too coarse for HTTP — its `InvalidRequest`
//!   bucket holds authorization, conflict and capability failures that must
//!   not share a status — and [`ToolInvocationError::kind`] maps
//!   `ToolNotFound` to `InvalidInput`, a `400` where HTTP wants `404`.
//!   Adding an error variant is therefore a compile error until someone
//!   chooses its status.

use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use proxima_core::StorageError;
use proxima_core::error::ErrorCode;
use proxima_core::mcp::McpToolError;

use crate::server::ToolInvocationError;

/// Base of every `type` URI. A stable, dereferenceable namespace so a client
/// can switch on `type` instead of on the human `title`.
const TYPE_BASE: &str = "https://proxima.dev/errors/";

/// One RFC 9457 problem document plus the response metadata that travels
/// with it.
#[derive(Debug, Clone)]
pub struct Problem {
    status: StatusCode,
    /// Slug appended to [`TYPE_BASE`]; the machine-readable discriminator.
    slug: &'static str,
    title: &'static str,
    detail: String,
    instance: String,
    /// `Allow` for a `405`. Emitted by hand because the generated routes use
    /// `any()`, which calls `skip_allow_header()` and suppresses axum's own.
    allow: Option<&'static str>,
}

impl Problem {
    #[must_use]
    pub fn new(
        status: StatusCode,
        slug: &'static str,
        title: &'static str,
        detail: impl Into<String>,
        instance: &str,
    ) -> Self {
        Self {
            status,
            slug,
            title,
            detail: detail.into(),
            instance: instance.to_string(),
            allow: None,
        }
    }

    #[must_use]
    pub fn with_allow(mut self, allow: &'static str) -> Self {
        self.allow = Some(allow);
        self
    }

    /// No bound [`crate::McpAuthContext`]. In a served deployment this means
    /// the request bypassed the shared `mcp_auth` layer, which 401s before
    /// dispatch — so the surface fails closed rather than dispatching
    /// unauthenticated.
    #[must_use]
    pub fn auth_required(instance: &str) -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            "auth-required",
            "Authentication required",
            "a bearer token is required on this surface",
            instance,
        )
    }

    #[must_use]
    pub fn invalid_input(detail: impl Into<String>, instance: &str) -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "invalid-input",
            "Invalid input",
            detail,
            instance,
        )
    }

    #[must_use]
    pub fn not_found(detail: impl Into<String>, instance: &str) -> Self {
        Self::new(
            StatusCode::NOT_FOUND,
            "not-found",
            "Not found",
            detail,
            instance,
        )
    }

    pub(crate) fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "type": format!("{TYPE_BASE}{}", self.slug),
            "title": self.title,
            "status": self.status.as_u16(),
            "detail": self.detail,
            "instance": self.instance,
        })
    }
}

impl IntoResponse for Problem {
    fn into_response(self) -> Response {
        let body = self.to_json().to_string();
        let mut response = (self.status, body).into_response();
        let headers = response.headers_mut();
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/problem+json"),
        );
        // Every answer on this surface is owner- and token-scoped.
        headers.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static(super::NO_STORE),
        );
        if let Some(allow) = self.allow {
            headers.insert(header::ALLOW, HeaderValue::from_static(allow));
        }
        response
    }
}

/// Map one dispatch failure onto its problem document.
///
/// `ToolInvocationError` carries no `client_message` of its own: its two
/// non-`Tool` variants are produced by this crate from a name the caller
/// supplied, so there is no internal detail to redact. The `Tool` arm — the
/// only one that can carry a storage or protocol fault — goes through
/// [`McpToolError::client_message`].
#[must_use]
pub fn problem_for(err: &ToolInvocationError, instance: &str) -> Problem {
    match err {
        ToolInvocationError::NotAuthorized(name) => Problem::new(
            StatusCode::FORBIDDEN,
            "not-authorized",
            "Not authorized",
            McpToolError::NotAuthorized(name.clone()).client_message(),
            instance,
        ),
        ToolInvocationError::ToolNotFound(name) => Problem::new(
            StatusCode::NOT_FOUND,
            "tool-not-found",
            "Tool not found",
            format!("tool {name} not found"),
            instance,
        ),
        ToolInvocationError::Tool(inner) => tool_problem(inner, instance),
    }
}

fn tool_problem(err: &McpToolError, instance: &str) -> Problem {
    let (status, slug, title) = match err {
        McpToolError::InvalidInput(_) => {
            (StatusCode::BAD_REQUEST, "invalid-input", "Invalid input")
        }
        McpToolError::NotFound(_) => (StatusCode::NOT_FOUND, "not-found", "Not found"),
        McpToolError::NotAuthorized(_) => {
            (StatusCode::FORBIDDEN, "not-authorized", "Not authorized")
        }
        McpToolError::LayeringViolation(_) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "layering-violation",
            "Layering violation",
        ),
        McpToolError::Unavailable(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
            "Capability unavailable",
        ),
        McpToolError::Protocol(inner) => protocol_status(inner.code),
        McpToolError::Storage(inner) => storage_status(inner),
        McpToolError::Other(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "Internal error",
        ),
    };
    // The redaction seam. `Other` and the internal storage variants collapse
    // to a generic string here; nothing else in this module may reach for
    // `Display`.
    Problem::new(status, slug, title, err.client_message(), instance)
}

const fn protocol_status(code: ErrorCode) -> (StatusCode, &'static str, &'static str) {
    match code {
        ErrorCode::AuthRequired => (
            StatusCode::UNAUTHORIZED,
            "auth-required",
            "Authentication required",
        ),
        ErrorCode::Forbidden => (StatusCode::FORBIDDEN, "forbidden", "Forbidden"),
        ErrorCode::UnknownSchema => (StatusCode::BAD_REQUEST, "unknown-schema", "Unknown schema"),
        ErrorCode::InvalidArgument => (
            StatusCode::BAD_REQUEST,
            "invalid-argument",
            "Invalid argument",
        ),
        ErrorCode::NotFound => (StatusCode::NOT_FOUND, "not-found", "Not found"),
        ErrorCode::ToolNotRegistered => (
            StatusCode::NOT_FOUND,
            "tool-not-registered",
            "Tool not registered",
        ),
        ErrorCode::AlreadyIngested => {
            (StatusCode::CONFLICT, "already-ingested", "Already ingested")
        }
        ErrorCode::IdempotencyConflict => (
            StatusCode::CONFLICT,
            "idempotency-conflict",
            "Idempotency conflict",
        ),
        ErrorCode::TriggerConflict => {
            (StatusCode::CONFLICT, "trigger-conflict", "Trigger conflict")
        }
        ErrorCode::DuplicateTriggerInRequest => (
            StatusCode::CONFLICT,
            "duplicate-trigger-in-request",
            "Duplicate trigger in request",
        ),
        // 409, not 451. Suppression is a erasure primitive (13) and is not
        // necessarily a legal hold; 451 would overclaim.
        ErrorCode::Suppressed => (StatusCode::CONFLICT, "suppressed", "Suppressed"),
        ErrorCode::Internal => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "Internal error",
        ),
    }
}

const fn storage_status(err: &StorageError) -> (StatusCode, &'static str, &'static str) {
    match err {
        StorageError::ConstraintViolation(_) => {
            (StatusCode::BAD_REQUEST, "invalid-input", "Invalid input")
        }
        StorageError::NotFound => (StatusCode::NOT_FOUND, "not-found", "Not found"),
        StorageError::Conflict(_) => (StatusCode::CONFLICT, "conflict", "Conflict"),
        StorageError::IdempotencyConflict { .. } => (
            StatusCode::CONFLICT,
            "idempotency-conflict",
            "Idempotency conflict",
        ),
        StorageError::Suppressed(_) => (StatusCode::CONFLICT, "suppressed", "Suppressed"),
        StorageError::Retryable(_)
        | StorageError::Unavailable(_)
        | StorageError::Internal(_)
        | StorageError::SchemaResetRequired { .. } => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "Internal error",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proxima_core::error::ProtocolError;

    fn status_of(err: &ToolInvocationError) -> StatusCode {
        problem_for(err, "/v1/tools/x").status
    }

    fn protocol(code: ErrorCode) -> ToolInvocationError {
        ToolInvocationError::Tool(McpToolError::Protocol(ProtocolError {
            code,
            message: "boom".into(),
            request_id: None,
        }))
    }

    #[test]
    fn variant_map_matches_the_documented_table() {
        assert_eq!(
            status_of(&ToolInvocationError::ToolNotFound("core_nope".into())),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            status_of(&ToolInvocationError::NotAuthorized("core_transfer".into())),
            StatusCode::FORBIDDEN
        );
        for (err, expected) in [
            (
                McpToolError::InvalidInput("x".into()),
                StatusCode::BAD_REQUEST,
            ),
            (McpToolError::NotFound("x".into()), StatusCode::NOT_FOUND),
            (
                McpToolError::NotAuthorized("x".into()),
                StatusCode::FORBIDDEN,
            ),
            (
                McpToolError::LayeringViolation("x".into()),
                StatusCode::UNPROCESSABLE_ENTITY,
            ),
            (
                McpToolError::Unavailable("x".into()),
                StatusCode::SERVICE_UNAVAILABLE,
            ),
            (
                McpToolError::from(proxima_core::ToolError::NotFound("x".into())),
                StatusCode::NOT_FOUND,
            ),
            (
                McpToolError::from(proxima_core::ToolError::Unavailable("x".into())),
                StatusCode::SERVICE_UNAVAILABLE,
            ),
            (
                McpToolError::Other("x".into()),
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
        ] {
            assert_eq!(
                status_of(&ToolInvocationError::Tool(err)),
                expected,
                "variant map drifted"
            );
        }

        for (code, expected) in [
            (ErrorCode::AuthRequired, StatusCode::UNAUTHORIZED),
            (ErrorCode::Forbidden, StatusCode::FORBIDDEN),
            (ErrorCode::UnknownSchema, StatusCode::BAD_REQUEST),
            (ErrorCode::InvalidArgument, StatusCode::BAD_REQUEST),
            (ErrorCode::NotFound, StatusCode::NOT_FOUND),
            (ErrorCode::ToolNotRegistered, StatusCode::NOT_FOUND),
            (ErrorCode::AlreadyIngested, StatusCode::CONFLICT),
            (ErrorCode::IdempotencyConflict, StatusCode::CONFLICT),
            (ErrorCode::TriggerConflict, StatusCode::CONFLICT),
            (ErrorCode::DuplicateTriggerInRequest, StatusCode::CONFLICT),
            (ErrorCode::Suppressed, StatusCode::CONFLICT),
            (ErrorCode::Internal, StatusCode::INTERNAL_SERVER_ERROR),
        ] {
            assert_eq!(status_of(&protocol(code)), expected, "protocol {code:?}");
        }

        for (err, expected) in [
            (
                StorageError::ConstraintViolation("x".into()),
                StatusCode::BAD_REQUEST,
            ),
            (StorageError::NotFound, StatusCode::NOT_FOUND),
            (StorageError::Conflict("x".into()), StatusCode::CONFLICT),
            (
                StorageError::IdempotencyConflict {
                    request_id: "r".into(),
                },
                StatusCode::CONFLICT,
            ),
            (StorageError::Suppressed("x".into()), StatusCode::CONFLICT),
            (
                StorageError::Retryable("x".into()),
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
            (
                StorageError::Unavailable("x".into()),
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
            (
                StorageError::Internal("x".into()),
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
            (
                StorageError::SchemaResetRequired {
                    details: "x".into(),
                },
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
        ] {
            assert_eq!(
                status_of(&ToolInvocationError::Tool(McpToolError::Storage(err))),
                expected
            );
        }
    }

    /// R4. An internal fault must not leak its message into `detail`, and a
    /// caller-actionable precondition must reach the caller verbatim.
    #[test]
    fn detail_is_client_message_not_display() {
        let leaky = ToolInvocationError::Tool(McpToolError::Other(
            "serialize tool output: fixture secret from output serializer".into(),
        ));
        let problem = problem_for(&leaky, "/v1/tools/core_remember");
        assert_eq!(problem.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(problem.slug, "internal");
        assert_eq!(problem.detail, "internal server error");
        assert!(!problem.detail.contains("fixture secret"));

        let precondition = ToolInvocationError::Tool(McpToolError::Unavailable(
            "semantic search unavailable: no embedding client is configured for this host".into(),
        ));
        assert_eq!(
            problem_for(&precondition, "/v1/tools/core_search_memories").detail,
            "semantic search unavailable: no embedding client is configured for this host"
        );
    }

    #[test]
    fn document_carries_the_rfc_9457_members() {
        let problem = problem_for(
            &ToolInvocationError::NotAuthorized("core_transfer:to_owner".into()),
            "/v1/tools/core_transfer/to_owner",
        );
        let json = problem.to_json();
        assert_eq!(json["type"], "https://proxima.dev/errors/not-authorized");
        assert_eq!(json["title"], "Not authorized");
        assert_eq!(json["status"], 403);
        assert_eq!(
            json["detail"],
            "tool core_transfer:to_owner not authorized for this MCP token"
        );
        assert_eq!(json["instance"], "/v1/tools/core_transfer/to_owner");
    }
}
