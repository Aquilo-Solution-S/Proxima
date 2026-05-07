use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::{self, FromFnLayer, Next};
use axum::response::{IntoResponse, Response};
use http::HeaderMap;
use http::header::{AUTHORIZATION, ORIGIN};
use proxima_core::wake::token_store::WakeTokenStore;
use uuid::Uuid;

use crate::McpServerError;

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
    #[must_use]
    pub fn new(patterns: impl IntoIterator<Item = &'static str>) -> Self {
        Self {
            patterns: patterns.into_iter().map(parse_pattern).collect(),
        }
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

/// Concrete return type for [`wake_token_auth_layer`]. The extractor
/// tuple `(State, Request)` is locked here so the public signature
/// stays a single-line `WakeTokenAuthLayer` instead of a six-line
/// `FromFnLayer<...>` (per `clippy::type_complexity`).
pub type WakeTokenAuthLayer = FromFnLayer<
    fn(
        State<Arc<WakeTokenStore>>,
        Request,
        Next,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Response> + Send>>,
    Arc<WakeTokenStore>,
    (State<Arc<WakeTokenStore>>, Request),
>;

/// Bearer-token middleware that resolves `Authorization: Bearer <uuid>`
/// against the `WakeTokenStore` and injects the resolved
/// `WakeTokenContext` into request extensions. Missing or unknown tokens
/// short-circuit with HTTP 401.
///
/// Returns a [`WakeTokenAuthLayer`] (alias of [`FromFnLayer`]) so
/// callers apply it directly with [`axum::Router::layer`].
pub fn wake_token_auth_layer(store: Arc<WakeTokenStore>) -> WakeTokenAuthLayer {
    fn dispatch(
        state: State<Arc<WakeTokenStore>>,
        request: Request,
        next: Next,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Response> + Send>> {
        Box::pin(wake_token_auth(state, request, next))
    }
    middleware::from_fn_with_state(store, dispatch as fn(_, _, _) -> _)
}

async fn wake_token_auth(
    State(store): State<Arc<WakeTokenStore>>,
    mut request: Request,
    next: Next,
) -> Response {
    let Some(token) = extract_bearer(&request) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let Some(ctx) = store.resolve(token).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    request.extensions_mut().insert(ctx);
    next.run(request).await
}

fn extract_bearer(request: &Request) -> Option<Uuid> {
    let header_value = request.headers().get(AUTHORIZATION)?;
    let raw = header_value.to_str().ok()?;
    let token = raw.strip_prefix("Bearer ")?.trim();
    Uuid::parse_str(token).ok()
}

fn parse_pattern(value: &'static str) -> OriginPattern {
    let parsed = parse_origin(value).expect("static origin pattern must parse");
    OriginPattern {
        scheme: parsed.scheme,
        host: parsed.host,
        port: parsed.port,
    }
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
