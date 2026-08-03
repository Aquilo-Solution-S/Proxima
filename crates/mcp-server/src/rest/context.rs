//! Call context for the REST surface: who is calling, from what client,
//! as which model — and the refusal that keeps those answers out of the
//! request body.
//!
//! MCP sources context partly from the JSON-RPC peer identity and partly
//! from reserved argument fields that `strip_call_context_args` removes
//! before validation. REST has no peer identity and no client-compatibility
//! debt, so context is header-borne and a reserved body field is an error
//! rather than a silently ignored duplicate.

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::{HeaderMap, header};
use proxima_core::mcp::McpAuthorContext;
use proxima_core::{MemoryId, RELEASE_VERSION};

use crate::auth::McpAuthContext;
use crate::rest::error::Problem;

/// Model label for the write's provenance.
pub const MODEL_ID_HEADER: &str = "X-Proxima-Model-Id";
/// The caller's own Self Perspective, as a `P:` reference.
pub const SELF_PERSPECTIVE_HEADER: &str = "X-Proxima-Self-Perspective";

/// Recorded as `client_name` when the caller sends no `User-Agent`.
const ADAPTER_CLIENT_NAME: &str = "proxima-rest";

/// Argument names that are call context on this surface, paired with the
/// header that carries them.
///
/// Rejecting is not pedantry: these fields carry provenance into an
/// append-only store. A client that copies an MCP payload to REST and has
/// its `model_id` silently stripped gets `200 OK` on every call while every
/// Fact it writes is attributed to `unknown` — and append-only means that
/// attribution cannot be corrected in place afterward, only superseded or
/// rebuilt. The failure is invisible at write time and expensive at read
/// time. One failed request during integration, carrying the exact fix, is
/// the cheaper trade.
///
/// They stay reserved even where REST reads nothing from them, so a future
/// move of a field between header and body cannot be mistaken for a schema
/// change.
pub const RESERVED_ARGUMENTS: &[(&str, &str)] = &[
    ("model_id", MODEL_ID_HEADER),
    ("caller_self_perspective", SELF_PERSPECTIVE_HEADER),
    ("_proxima_caller_self_perspective", SELF_PERSPECTIVE_HEADER),
    (
        "current_root_perspective_memory_id",
        SELF_PERSPECTIVE_HEADER,
    ),
];

/// The full [`McpAuthContext`] — owner, authz and model id — as injected by
/// the shared `mcp_auth` layer.
///
/// Deliberately the whole context rather than `proxima::app::Authz`'s
/// `AuthzContext` alone: the dispatch seam needs the bound `Owner` to build
/// an `McpToolCtx`, and the token's model id is the fallback when the caller
/// sends no `X-Proxima-Model-Id`. Nothing here authenticates — the layer
/// already did, and re-implementing it is exactly what R3 forbids.
#[derive(Debug, Clone)]
pub struct RestAuth(pub McpAuthContext);

impl<S> FromRequestParts<S> for RestAuth
where
    S: Send + Sync,
{
    type Rejection = Problem;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<McpAuthContext>()
            .cloned()
            .map(Self)
            .ok_or_else(|| Problem::auth_required(parts.uri.path()))
    }
}

/// Build the author context for one REST call from its headers.
///
/// # Errors
///
/// Returns a `400` problem when `X-Proxima-Self-Perspective` is not a `P:`
/// reference to a UUID.
pub fn author_from_headers(
    headers: &HeaderMap,
    auth: &McpAuthContext,
    instance: &str,
) -> Result<McpAuthorContext, Problem> {
    let (client_name, client_version) = client_implementation(headers);
    Ok(McpAuthorContext {
        model_id: header_str(headers, MODEL_ID_HEADER)
            .or(auth.model_id.as_deref())
            .unwrap_or("unknown")
            .to_string(),
        client_name,
        client_version,
        caller_self_perspective: self_perspective(headers, instance)?,
    })
}

fn self_perspective(headers: &HeaderMap, instance: &str) -> Result<Option<MemoryId>, Problem> {
    let Some(raw) = header_str(headers, SELF_PERSPECTIVE_HEADER) else {
        return Ok(None);
    };
    // The wire grammar is prefixed (`P:<uuid>`); the bare form is accepted so
    // a caller that already holds a raw id is not forced to re-prefix it.
    let bare = raw.strip_prefix("P:").unwrap_or(raw);
    uuid::Uuid::parse_str(bare)
        .map(|id| Some(MemoryId::new(id)))
        .map_err(|err| {
            Problem::invalid_input(
                format!("{SELF_PERSPECTIVE_HEADER} must be a `P:<uuid>` reference: {err}"),
                instance,
            )
        })
}

/// Reject any reserved argument name in a request body.
///
/// # Errors
///
/// Returns a `400` problem whose `detail` names the header to use instead.
pub fn reject_reserved_arguments(args: &serde_json::Value, instance: &str) -> Result<(), Problem> {
    let Some(object) = args.as_object() else {
        return Ok(());
    };
    for (field, header_name) in RESERVED_ARGUMENTS {
        if object.contains_key(*field) {
            return Err(Problem::new(
                axum::http::StatusCode::BAD_REQUEST,
                "reserved-argument",
                "Reserved argument in request body",
                format!(
                    "`{field}` is call context on this surface; send it as the {header_name} header"
                ),
                instance,
            ));
        }
    }
    Ok(())
}

/// `(name, version)` from `User-Agent`, recorded as operator provenance.
///
/// An unattributed call records the adapter's own name rather than
/// `"unknown"`: the write really was made through this surface, and that is
/// the honest attribution available.
fn client_implementation(headers: &HeaderMap) -> (String, String) {
    let Some(agent) = header_str(headers, header::USER_AGENT.as_str()) else {
        return (ADAPTER_CLIENT_NAME.to_string(), RELEASE_VERSION.to_string());
    };
    // `Name/1.2.3 (comment) other/4` — the leading product token is the
    // client; trailing products are transport middleware and are dropped.
    let product = agent.split_whitespace().next().unwrap_or(agent);
    product.split_once('/').map_or_else(
        || (product.to_string(), "0".to_string()),
        |(name, version)| (name.to_string(), version.to_string()),
    )
}

fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proxima_core::{AuthPath, AuthzContext, Owner, OwnerRef, UserId};

    fn auth(model_id: Option<&str>) -> McpAuthContext {
        let owner: Owner = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));
        McpAuthContext {
            owner,
            authz: AuthzContext::single_owner(&owner, AuthPath::HostBearer),
            model_id: model_id.map(ToString::to_string),
        }
    }

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(
                axum::http::HeaderName::from_bytes(name.as_bytes()).expect("header name"),
                value.parse().expect("header value"),
            );
        }
        map
    }

    #[test]
    fn model_id_prefers_the_header_then_the_token_then_unknown() {
        let with_header = author_from_headers(
            &headers(&[(MODEL_ID_HEADER, "claude-opus-5")]),
            &auth(Some("token-model")),
            "/v1/tools/core_remember",
        )
        .expect("author");
        assert_eq!(with_header.model_id, "claude-opus-5");

        let from_token = author_from_headers(
            &headers(&[]),
            &auth(Some("token-model")),
            "/v1/tools/core_remember",
        )
        .expect("author");
        assert_eq!(from_token.model_id, "token-model");

        let unattributed =
            author_from_headers(&headers(&[]), &auth(None), "/v1/tools/core_remember")
                .expect("author");
        assert_eq!(unattributed.model_id, "unknown");
    }

    #[test]
    fn self_perspective_accepts_the_prefixed_reference() {
        let id = uuid::Uuid::now_v7();
        let author = author_from_headers(
            &headers(&[(SELF_PERSPECTIVE_HEADER, &format!("P:{id}"))]),
            &auth(None),
            "/v1/tools/core_remember",
        )
        .expect("author");
        assert_eq!(
            author.caller_self_perspective.map(MemoryId::into_inner),
            Some(id)
        );

        let err = author_from_headers(
            &headers(&[(SELF_PERSPECTIVE_HEADER, "P:not-a-uuid")]),
            &auth(None),
            "/v1/tools/core_remember",
        )
        .expect_err("malformed reference");
        assert_eq!(
            err.to_json()["status"],
            u64::from(axum::http::StatusCode::BAD_REQUEST.as_u16())
        );
    }

    #[test]
    fn user_agent_is_split_into_name_and_version() {
        let author = author_from_headers(
            &headers(&[(header::USER_AGENT.as_str(), "acme-cli/1.2.3 (linux) curl/8")]),
            &auth(None),
            "/v1/tools/core_remember",
        )
        .expect("author");
        assert_eq!(author.client_name, "acme-cli");
        assert_eq!(author.client_version, "1.2.3");

        let bare = author_from_headers(&headers(&[]), &auth(None), "/v1/tools/core_remember")
            .expect("author");
        assert_eq!(bare.client_name, ADAPTER_CLIENT_NAME);
    }

    #[test]
    fn every_reserved_name_is_refused_and_names_its_header() {
        for (field, header_name) in RESERVED_ARGUMENTS {
            let args = serde_json::json!({ "text": "x", *field: "y" });
            let err = reject_reserved_arguments(&args, "/v1/tools/core_remember")
                .expect_err("reserved argument must be refused");
            let json = err.to_json();
            assert_eq!(json["status"], 400);
            assert_eq!(json["type"], "https://proxima.dev/errors/reserved-argument");
            let detail = json["detail"].as_str().expect("detail");
            assert!(detail.contains(field), "{detail}");
            assert!(detail.contains(header_name), "{detail}");
        }

        reject_reserved_arguments(
            &serde_json::json!({ "text": "x" }),
            "/v1/tools/core_remember",
        )
        .expect("an ordinary body passes");
    }
}
