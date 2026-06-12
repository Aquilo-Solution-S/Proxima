use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::{self, FromFnLayer, Next};
use axum::response::{IntoResponse, Response};
use http::HeaderMap;
use http::header::{AUTHORIZATION, ORIGIN};

use crate::McpServerError;
use crate::auth::McpEdgeAuth;

#[derive(Clone, Debug)]
pub struct OriginAllowlist {
    patterns: Vec<OriginPattern>,
}

#[derive(Clone, Debug)]
struct OriginPattern {
    scheme: String,
    host: String,
    port: PortMatch,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum PortMatch {
    Any,
    Exact(u16),
}

impl OriginAllowlist {
    /// # Panics
    ///
    /// Panics when a static pattern is empty, `"*"`, or unparseable.
    #[must_use]
    pub fn new(patterns: impl IntoIterator<Item = &'static str>) -> Self {
        Self::parse(patterns).expect("static origin pattern must parse")
    }

    /// # Errors
    ///
    /// Returns `InvalidOrigin` when an entry is empty, `"*"`, or not a
    /// concrete origin pattern.
    pub fn parse<I, S>(patterns: I) -> Result<Self, McpServerError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Ok(Self {
            patterns: patterns
                .into_iter()
                .map(|pattern| parse_pattern(pattern.as_ref()))
                .collect::<Result<Vec<_>, _>>()?,
        })
    }

    #[must_use]
    pub fn allows(&self, headers: &HeaderMap) -> bool {
        let Some(origin) = headers.get(ORIGIN).and_then(|v| v.to_str().ok()) else {
            return false;
        };
        let Some(parsed) = parse_origin(origin) else {
            return false;
        };
        self.patterns.iter().any(|pattern| {
            pattern.scheme == parsed.scheme
                && pattern.host == parsed.host
                && match (&pattern.port, &parsed.port) {
                    (PortMatch::Any, _) => true,
                    (PortMatch::Exact(expected), PortMatch::Exact(actual)) => expected == actual,
                    (PortMatch::Exact(_), PortMatch::Any) => false,
                }
        })
    }

    #[must_use]
    pub fn origins(&self) -> Vec<String> {
        self.patterns
            .iter()
            .map(|pattern| {
                let base = format!("{}://{}", pattern.scheme, pattern.host);
                match pattern.port {
                    PortMatch::Any => base,
                    PortMatch::Exact(port) => format!("{base}:{port}"),
                }
            })
            .collect()
    }
}

#[must_use]
pub fn default_allowlist() -> OriginAllowlist {
    OriginAllowlist::new([
        "http://localhost",
        "http://127.0.0.1",
        "http://[::1]",
        "tauri://localhost",
        "https://tauri.localhost",
    ])
}

/// # Errors
///
/// Returns `NonLoopbackBind` for any non-loopback address.
pub fn assert_loopback(addr: &SocketAddr) -> Result<(), McpServerError> {
    if !addr.ip().is_loopback() {
        return Err(McpServerError::NonLoopbackBind(addr.ip()));
    }
    Ok(())
}

/// Concrete return type for [`mcp_auth_layer`]. The extractor
/// tuple `(State, Request)` is locked here so the public signature
/// stays a single-line `McpAuthLayer` instead of a six-line
/// `FromFnLayer<...>` (per `clippy::type_complexity`).
pub type McpAuthLayer = FromFnLayer<
    fn(
        State<McpAuthLayerState>,
        Request,
        Next,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Response> + Send>>,
    McpAuthLayerState,
    (State<McpAuthLayerState>, Request),
>;

#[derive(Clone, Debug)]
pub struct McpAuthLayerState {
    auth: Arc<McpEdgeAuth>,
    allowlist: OriginAllowlist,
}

/// Bearer-token middleware that resolves
/// `Authorization: Bearer <wire-token>` via MCP edge auth and injects
/// the resolved context into request extensions. Wire tokens are
/// `pxw_<uuid>` wake tokens, `pxm_<uuid>` local master tokens, or host
/// bearer material. Missing or unknown tokens short-circuit with HTTP
/// 401. A present but disallowed `Origin` short-circuits with 403;
/// missing `Origin` is allowed after bearer auth for native CLI clients.
///
/// Returns a [`McpAuthLayer`] (alias of [`FromFnLayer`]) so
/// callers apply it directly with [`axum::Router::layer`].
pub fn mcp_auth_layer(auth: Arc<McpEdgeAuth>, allowlist: OriginAllowlist) -> McpAuthLayer {
    fn dispatch(
        state: State<McpAuthLayerState>,
        request: Request,
        next: Next,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Response> + Send>> {
        Box::pin(mcp_auth(state, request, next))
    }
    middleware::from_fn_with_state(
        McpAuthLayerState { auth, allowlist },
        dispatch as fn(_, _, _) -> _,
    )
}

async fn mcp_auth(
    State(state): State<McpAuthLayerState>,
    mut request: Request,
    next: Next,
) -> Response {
    let Some(token) = extract_bearer(&request) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let Some(ctx) = state.auth.resolve(&token).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    if request.headers().contains_key(ORIGIN) && !state.allowlist.allows(request.headers()) {
        return (StatusCode::FORBIDDEN, "origin not allowed").into_response();
    }
    request.extensions_mut().insert(ctx);
    next.run(request).await
}

fn extract_bearer(request: &Request) -> Option<String> {
    let header_value = request.headers().get(AUTHORIZATION)?;
    let raw = header_value.to_str().ok()?;
    Some(raw.strip_prefix("Bearer ")?.trim().to_string())
}

fn parse_pattern(value: &str) -> Result<OriginPattern, McpServerError> {
    let pattern = value.trim();
    if pattern.is_empty() || pattern == "*" {
        return Err(invalid_origin(value));
    }
    let Some(parsed) = parse_origin(pattern) else {
        return Err(invalid_origin(value));
    };
    if parsed.scheme.is_empty() || parsed.host.is_empty() {
        return Err(invalid_origin(value));
    }
    Ok(OriginPattern {
        scheme: parsed.scheme,
        host: parsed.host,
        port: parsed.port,
    })
}

fn invalid_origin(value: &str) -> McpServerError {
    McpServerError::InvalidOrigin(format!("origin pattern {value:?}"))
}

struct ParsedOrigin {
    scheme: String,
    host: String,
    port: PortMatch,
}

fn parse_origin(value: &str) -> Option<ParsedOrigin> {
    let (scheme, rest) = value.split_once("://")?;
    let (host, port) = if let Some(rest) = rest.strip_prefix('[') {
        let (host, after) = rest.split_once(']')?;
        let port = if let Some(port) = after.strip_prefix(':') {
            PortMatch::Exact(port.parse().ok()?)
        } else if after.is_empty() {
            PortMatch::Any
        } else {
            return None;
        };
        (format!("[{}]", host.to_ascii_lowercase()), port)
    } else if let Some((host, port)) = rest.rsplit_once(':') {
        (
            host.to_ascii_lowercase(),
            PortMatch::Exact(port.parse().ok()?),
        )
    } else {
        (rest.to_ascii_lowercase(), PortMatch::Any)
    };
    Some(ParsedOrigin {
        scheme: scheme.to_ascii_lowercase(),
        host,
        port,
    })
}

#[cfg(test)]
mod tests {
    use super::OriginAllowlist;
    use crate::McpServerError;

    #[test]
    fn parse_valid_patterns_round_trips_into_origins() {
        let allowlist =
            OriginAllowlist::parse(["http://LOCALHOST", "https://example.com:8443"]).unwrap();

        assert_eq!(
            allowlist.origins(),
            vec![
                "http://localhost".to_string(),
                "https://example.com:8443".to_string(),
            ]
        );
    }

    #[test]
    fn parse_rejects_star() {
        assert_invalid_origin("*");
    }

    #[test]
    fn parse_rejects_garbage() {
        assert_invalid_origin("not an origin");
    }

    #[test]
    fn parse_rejects_empty_entry() {
        assert_invalid_origin("  ");
    }

    #[test]
    fn parse_empty_list_allows_nothing() {
        let allowlist = OriginAllowlist::parse(std::iter::empty::<&str>()).unwrap();

        assert!(allowlist.origins().is_empty());
    }

    fn assert_invalid_origin(pattern: &str) {
        let err = OriginAllowlist::parse([pattern]).unwrap_err();
        let McpServerError::InvalidOrigin(message) = err else {
            panic!("expected invalid origin");
        };
        assert!(message.contains(pattern));
    }
}
