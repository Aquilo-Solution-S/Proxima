use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{Method, StatusCode};
use axum::middleware::{self, FromFnLayer, Next};
use axum::response::{IntoResponse, Response};
use futures_util::Stream;
use http::header::{
    ACCESS_CONTROL_ALLOW_HEADERS, ACCESS_CONTROL_ALLOW_METHODS, ACCESS_CONTROL_ALLOW_ORIGIN,
    ACCESS_CONTROL_EXPOSE_HEADERS, ACCESS_CONTROL_REQUEST_HEADERS, ACCESS_CONTROL_REQUEST_METHOD,
    AUTHORIZATION, HOST, ORIGIN, VARY,
};
use http::{HeaderMap, HeaderName, HeaderValue};
use http_body::{Body as HttpBody, Frame};
use http_body_util::{BodyStream, StreamBody};
use proxima_core::{
    AuthPath, Authenticator, Identity, Owner, RevalidationConfig, revalidate_stream,
};

use crate::McpServerError;
use crate::auth::McpEdgeAuth;
use crate::session::{McpSessionBindings, parse_owner_key};

const MCP_SESSION_ID_HEADER: &str = "Mcp-Session-Id";
const PROXIMA_OWNER_HEADER: &str = "X-Proxima-Owner";
const LOOPBACK_HOSTS: [&str; 3] = ["localhost", "127.0.0.1", "::1"];

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
        let mut origins = headers.get_all(ORIGIN).iter();
        let Some(origin) = origins.next() else {
            return false;
        };
        if origins.next().is_some() {
            return false;
        }
        self.allows_origin(origin)
    }

    fn allows_origin(&self, origin: &HeaderValue) -> bool {
        let Some(origin) = origin.to_str().ok() else {
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

/// Inbound `Host` / HTTP/2 `:authority` allowlist for the whole listener.
///
/// Loopback is always present. Additional entries are bare hostnames or
/// `host:port`; an entry without a port accepts that host on any port. The
/// matching rules deliberately mirror rmcp's private DNS-rebinding guard so
/// one value can configure the outer listener and rmcp's inner `/mcp` guard.
#[derive(Clone, Debug)]
pub struct HostAllowlist {
    hosts: Vec<String>,
    authorities: Vec<NormalizedAuthority>,
}

impl HostAllowlist {
    /// Build the listener allowlist from deployment-specific hosts.
    ///
    /// Loopback is added unconditionally and duplicate normalized authorities
    /// are removed. Empty/blank additional entries add nothing; the resulting
    /// allowlist is therefore never empty and can never enter rmcp's
    /// allow-all-on-empty state.
    #[must_use]
    pub fn new<I, S>(additional_hosts: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut hosts = Vec::new();
        let mut authorities = Vec::new();
        for raw in LOOPBACK_HOSTS.into_iter().map(str::to_string).chain(
            additional_hosts
                .into_iter()
                .map(|host| host.as_ref().trim().to_ascii_lowercase()),
        ) {
            let raw = raw.trim();
            if raw.is_empty() {
                continue;
            }
            let Some(authority) = parse_allowed_authority(raw) else {
                continue;
            };
            if authorities.contains(&authority) {
                continue;
            }
            hosts.push(raw.to_ascii_lowercase());
            authorities.push(authority);
        }
        Self { hosts, authorities }
    }

    /// Raw authority entries passed to rmcp's inner `/mcp` guard.
    ///
    /// The slice always contains the loopback defaults and therefore never
    /// activates rmcp's empty-list allow-all behavior.
    #[must_use]
    pub fn hosts(&self) -> &[String] {
        &self.hosts
    }

    fn allows(&self, authority: &NormalizedAuthority) -> bool {
        self.authorities.iter().any(|allowed| {
            allowed.host == authority.host
                && match allowed.port {
                    Some(port) => authority.port == Some(port),
                    None => true,
                }
        })
    }
}

impl Default for HostAllowlist {
    fn default() -> Self {
        Self::new(std::iter::empty::<&str>())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct NormalizedAuthority {
    host: String,
    port: Option<u16>,
}

fn normalize_host(host: &str) -> String {
    host.trim_matches('[')
        .trim_matches(']')
        .to_ascii_lowercase()
}

fn normalize_authority(host: &str, port: Option<u16>) -> NormalizedAuthority {
    NormalizedAuthority {
        host: normalize_host(host),
        port,
    }
}

fn parse_allowed_authority(allowed: &str) -> Option<NormalizedAuthority> {
    let allowed = allowed.trim();
    if allowed.is_empty() {
        return None;
    }
    if let Ok(authority) = http::uri::Authority::try_from(allowed) {
        return Some(normalize_authority(authority.host(), authority.port_u16()));
    }
    Some(normalize_authority(allowed, None))
}

#[derive(Clone, Copy, Debug)]
enum HostRequestError {
    InvalidEncoding,
    InvalidHeader,
    MissingHeader,
}

impl IntoResponse for HostRequestError {
    fn into_response(self) -> Response {
        let message = match self {
            Self::InvalidEncoding => "Bad Request: Invalid Host header encoding",
            Self::InvalidHeader => "Bad Request: Invalid Host header",
            Self::MissingHeader => "Bad Request: missing Host header",
        };
        (StatusCode::BAD_REQUEST, message).into_response()
    }
}

fn request_authority(request: &Request) -> Result<NormalizedAuthority, HostRequestError> {
    if let Some(host) = request.headers().get(HOST) {
        let host = host.to_str().map_err(|_| {
            tracing::warn!(host = ?host, "rejected request with non-UTF-8 Host header");
            HostRequestError::InvalidEncoding
        })?;
        let authority = http::uri::Authority::try_from(host).map_err(|_| {
            tracing::warn!(host, "rejected request with malformed Host header");
            HostRequestError::InvalidHeader
        })?;
        return Ok(normalize_authority(authority.host(), authority.port_u16()));
    }
    let authority = request.uri().authority().ok_or_else(|| {
        tracing::warn!("rejected request with missing Host header and no :authority");
        HostRequestError::MissingHeader
    })?;
    Ok(normalize_authority(authority.host(), authority.port_u16()))
}

/// Concrete return type for [`host_guard_layer`].
pub type HostGuardLayer = FromFnLayer<
    fn(
        State<HostAllowlist>,
        Request,
        Next,
    ) -> Pin<Box<dyn std::future::Future<Output = Response> + Send>>,
    HostAllowlist,
    (State<HostAllowlist>, Request),
>;

/// Listener-wide DNS-rebinding guard.
///
/// Apply this after every route has been merged and outside bearer auth. It
/// protects `/mcp`, `/v1`, host-mounted routes, and public OAuth metadata with
/// one policy; rmcp keeps its own `/mcp` check as defense in depth.
#[must_use = "apply the returned layer to the complete HTTP listener"]
pub fn host_guard_layer(allowlist: HostAllowlist) -> HostGuardLayer {
    fn dispatch(
        state: State<HostAllowlist>,
        request: Request,
        next: Next,
    ) -> Pin<Box<dyn std::future::Future<Output = Response> + Send>> {
        Box::pin(enforce_host(state, request, next))
    }
    middleware::from_fn_with_state(allowlist, dispatch as fn(_, _, _) -> _)
}

async fn enforce_host(
    State(allowlist): State<HostAllowlist>,
    request: Request,
    next: Next,
) -> Response {
    let authority = match request_authority(&request) {
        Ok(authority) => authority,
        Err(error) => return error.into_response(),
    };
    if !allowlist.allows(&authority) {
        tracing::warn!(
            host = ?authority,
            "rejected request with disallowed Host header (possible DNS rebinding attempt)",
        );
        return Response::builder()
            .status(StatusCode::FORBIDDEN)
            .body(Body::from("Forbidden: Host header is not allowed"))
            .expect("static Host rejection response is valid");
    }
    next.run(request).await
}

/// Concrete return type for [`cors_layer`].
pub type CorsLayer = FromFnLayer<
    fn(
        State<OriginAllowlist>,
        Request,
        Next,
    ) -> Pin<Box<dyn std::future::Future<Output = Response> + Send>>,
    OriginAllowlist,
    (State<OriginAllowlist>, Request),
>;

/// Listener-wide browser CORS and Origin guard.
///
/// Apply this after every route has been merged, outside bearer auth, and
/// inside the body-size and Host guards. A native request without `Origin`
/// keeps its normal route and authentication semantics. A browser preflight
/// (`OPTIONS` + `Origin` + `Access-Control-Request-Method`) is answered before
/// auth; malformed preflight fields fail with `400`, and the actual request
/// still requires its normal bearer credentials.
#[must_use = "apply the returned layer to the complete HTTP listener"]
pub fn cors_layer(allowlist: OriginAllowlist) -> CorsLayer {
    fn dispatch(
        state: State<OriginAllowlist>,
        request: Request,
        next: Next,
    ) -> Pin<Box<dyn std::future::Future<Output = Response> + Send>> {
        Box::pin(enforce_cors(state, request, next))
    }
    middleware::from_fn_with_state(allowlist, dispatch as fn(_, _, _) -> _)
}

async fn enforce_cors(
    State(allowlist): State<OriginAllowlist>,
    request: Request,
    next: Next,
) -> Response {
    let mut origins = request.headers().get_all(ORIGIN).iter();
    let Some(origin) = origins.next().cloned() else {
        let mut response = next.run(request).await;
        append_vary(&mut response, "Origin");
        return response;
    };
    if origins.next().is_some() || !allowlist.allows_origin(&origin) {
        tracing::warn!("rejected request with disallowed or malformed Origin header");
        let mut response = (StatusCode::FORBIDDEN, "origin not allowed").into_response();
        append_vary(&mut response, "Origin");
        return response;
    }

    if request.method() == Method::OPTIONS {
        match parse_cors_preflight(request.headers()) {
            Ok(Some(preflight)) => {
                let mut response = StatusCode::NO_CONTENT.into_response();
                response
                    .headers_mut()
                    .insert(ACCESS_CONTROL_ALLOW_ORIGIN, origin);
                response
                    .headers_mut()
                    .insert(ACCESS_CONTROL_ALLOW_METHODS, preflight.method);
                for request_headers in preflight.headers {
                    response
                        .headers_mut()
                        .append(ACCESS_CONTROL_ALLOW_HEADERS, request_headers);
                }
                append_vary(&mut response, "Origin");
                append_vary(&mut response, "Access-Control-Request-Method");
                append_vary(&mut response, "Access-Control-Request-Headers");
                return response;
            }
            Ok(None) => {}
            Err(()) => {
                tracing::warn!("rejected malformed CORS preflight headers");
                let mut response =
                    (StatusCode::BAD_REQUEST, "invalid CORS preflight").into_response();
                append_vary(&mut response, "Origin");
                append_vary(&mut response, "Access-Control-Request-Method");
                append_vary(&mut response, "Access-Control-Request-Headers");
                return response;
            }
        }
    }

    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(ACCESS_CONTROL_ALLOW_ORIGIN, origin);
    response.headers_mut().insert(
        ACCESS_CONTROL_EXPOSE_HEADERS,
        HeaderValue::from_static("Mcp-Session-Id, WWW-Authenticate"),
    );
    append_vary(&mut response, "Origin");
    response
}

struct CorsPreflight {
    method: HeaderValue,
    headers: Vec<HeaderValue>,
}

fn parse_cors_preflight(headers: &HeaderMap) -> Result<Option<CorsPreflight>, ()> {
    let mut methods = headers.get_all(ACCESS_CONTROL_REQUEST_METHOD).iter();
    let Some(method) = methods.next().cloned() else {
        return Ok(None);
    };
    if methods.next().is_some() || Method::from_bytes(method.as_bytes()).is_err() {
        return Err(());
    }

    let request_headers: Vec<_> = headers
        .get_all(ACCESS_CONTROL_REQUEST_HEADERS)
        .iter()
        .cloned()
        .collect();
    if request_headers
        .iter()
        .any(|value| !valid_cors_header_name_list(value))
    {
        return Err(());
    }

    Ok(Some(CorsPreflight {
        method,
        headers: request_headers,
    }))
}

fn valid_cors_header_name_list(value: &HeaderValue) -> bool {
    value.as_bytes().split(|byte| *byte == b',').all(|name| {
        let name = trim_http_ows(name);
        !name.is_empty() && HeaderName::from_bytes(name).is_ok()
    })
}

fn trim_http_ows(mut value: &[u8]) -> &[u8] {
    while value
        .first()
        .is_some_and(|byte| matches!(*byte, b' ' | b'\t'))
    {
        value = &value[1..];
    }
    while value
        .last()
        .is_some_and(|byte| matches!(*byte, b' ' | b'\t'))
    {
        value = &value[..value.len() - 1];
    }
    value
}

fn append_vary(response: &mut Response, value: &'static str) {
    response
        .headers_mut()
        .append(VARY, HeaderValue::from_static(value));
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

/// Concrete return type for [`mcp_auth_layer_with_config`]. The extractor
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
    sessions: McpSessionBindings,
    revalidation: RevalidationConfig,
    www_authenticate: Option<http::HeaderValue>,
}

/// Bearer-token middleware that resolves `Authorization: Bearer
/// <wire-token>` via MCP edge auth and injects the resolved context into
/// request extensions. Accepted bearer material is host-authenticated;
/// reserved local-token prefixes fail closed.
/// Missing or unknown tokens short-circuit with HTTP 401. Browser Origin and
/// preflight handling belongs to the listener-wide [`cors_layer`].
///
/// Returns a [`McpAuthLayer`] (alias of [`FromFnLayer`]) so callers
/// apply it directly with [`axum::Router::layer`].
pub fn mcp_auth_layer_with_config(
    auth: Arc<McpEdgeAuth>,
    revalidation: RevalidationConfig,
) -> McpAuthLayer {
    mcp_auth_layer_with_metadata(auth, revalidation, None)
}

#[must_use = "apply the returned layer to the MCP router"]
pub fn mcp_auth_layer_with_metadata(
    auth: Arc<McpEdgeAuth>,
    revalidation: RevalidationConfig,
    www_authenticate: Option<http::HeaderValue>,
) -> McpAuthLayer {
    mcp_auth_layer_with_sessions(
        auth,
        McpSessionBindings::new(),
        revalidation,
        www_authenticate,
    )
}

#[must_use = "apply the returned layer to the MCP router"]
pub fn mcp_auth_layer_with_sessions(
    auth: Arc<McpEdgeAuth>,
    sessions: McpSessionBindings,
    revalidation: RevalidationConfig,
    www_authenticate: Option<http::HeaderValue>,
) -> McpAuthLayer {
    fn dispatch(
        state: State<McpAuthLayerState>,
        request: Request,
        next: Next,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Response> + Send>> {
        Box::pin(mcp_auth(state, request, next))
    }
    middleware::from_fn_with_state(
        McpAuthLayerState {
            auth,
            sessions,
            revalidation,
            www_authenticate,
        },
        dispatch as fn(_, _, _) -> _,
    )
}

async fn mcp_auth(
    State(state): State<McpAuthLayerState>,
    mut request: Request,
    next: Next,
) -> Response {
    let Some(token) = extract_bearer(&request) else {
        return unauthorized(&state);
    };
    // Validate the bearer BEFORE resolving owner/session so an invalid
    // token always yields 401 regardless of session/owner-header state.
    // This closes the 401/403 oracle: an unauthenticated caller learns
    // nothing about owner or session requirements. Owner narrowing happens
    // in memory after selection, using this retained authentication result.
    let Some(resolved) = state.auth.resolve_unbound(&token).await else {
        return unauthorized(&state);
    };
    let (selected_owner, bind_new_session, via_session) =
        match selected_owner(&state.sessions, request.headers()).await {
            OwnerSelection::Selected {
                owner,
                bind_new,
                via_session,
            } => (owner, bind_new, via_session),
            // An `Mcp-Session-Id` header that is present but not bound
            // (unknown or idle-evicted) answers 404, which standard
            // Streamable-HTTP clients transparently re-initialize on. Only
            // the token-valid path reaches here, so 404 never leaks to an
            // unauthenticated caller.
            OwnerSelection::UnknownSession => return session_not_found(),
            OwnerSelection::Missing => {
                return (StatusCode::FORBIDDEN, "owner selection required or invalid")
                    .into_response();
            }
        };
    let Some(ctx) = resolved.narrowed_to_owner(selected_owner) else {
        return if via_session {
            session_not_found()
        } else {
            unauthorized(&state)
        };
    };
    let identity = ctx.authz.identity_for_revalidation();
    let epoch_source = if ctx.authz.auth_path() == AuthPath::HostBearer {
        state.auth.host_authenticator()
    } else {
        None
    };
    request.extensions_mut().insert(ctx);
    let response = next.run(request).await;
    if bind_new_session
        && let Some(session_id) = response_header_str(response.headers(), MCP_SESSION_ID_HEADER)
    {
        state.sessions.bind(session_id, selected_owner).await;
    }
    revalidate_response(response, identity, epoch_source, state.revalidation)
}

/// Outcome of resolving the caller's owner selection from the
/// `Mcp-Session-Id` / `X-Proxima-Owner` headers.
enum OwnerSelection {
    /// A concrete owner was selected. `bind_new` requests binding the
    /// response's freshly minted session id to this owner. `via_session`
    /// records whether the owner came from an existing session binding.
    Selected {
        owner: Owner,
        bind_new: bool,
        via_session: bool,
    },
    /// An `Mcp-Session-Id` header is present but has no live binding
    /// (never bound, or idle-evicted). Distinct from `Missing` so the
    /// transport can answer 404 and let the client re-initialize.
    UnknownSession,
    /// No usable owner selection: no session id and no/invalid owner
    /// header, or an owner header that conflicts with the bound session.
    Missing,
}

async fn selected_owner(sessions: &McpSessionBindings, headers: &HeaderMap) -> OwnerSelection {
    let owner_header = request_header_str(headers, PROXIMA_OWNER_HEADER);
    let session_id = request_header_str(headers, MCP_SESSION_ID_HEADER);
    if let Some(session_id) = session_id {
        let Some(owner) = sessions.owner_for(session_id).await else {
            return OwnerSelection::UnknownSession;
        };
        if let Some(raw_owner) = owner_header {
            match parse_owner_key(raw_owner) {
                Some(requested) if requested == owner => {}
                _ => return OwnerSelection::Missing,
            }
        }
        return OwnerSelection::Selected {
            owner,
            bind_new: false,
            via_session: true,
        };
    }

    match owner_header.and_then(parse_owner_key) {
        Some(owner) => OwnerSelection::Selected {
            owner,
            bind_new: true,
            via_session: false,
        },
        None => OwnerSelection::Missing,
    }
}

/// 404 for a present-but-unbound `Mcp-Session-Id`. Matches rmcp's
/// unknown-session behavior: standard Streamable-HTTP clients auto-issue
/// a fresh `initialize` on 404.
fn session_not_found() -> Response {
    (StatusCode::NOT_FOUND, "unknown or expired MCP session").into_response()
}

fn unauthorized(state: &McpAuthLayerState) -> Response {
    let mut resp = StatusCode::UNAUTHORIZED.into_response();
    if let Some(value) = &state.www_authenticate {
        resp.headers_mut()
            .insert(http::header::WWW_AUTHENTICATE, value.clone());
    }
    resp
}

fn revalidate_response(
    response: Response,
    identity: Identity,
    authenticator: Option<Arc<dyn Authenticator>>,
    config: RevalidationConfig,
) -> Response {
    response.map(|body| Body::new(RevalidatedBody::new(body, identity, authenticator, config)))
}

fn request_header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

fn response_header_str(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned)
}

type FrameStream<D, E> = Pin<Box<dyn Stream<Item = Result<Frame<D>, E>> + Send>>;
type RevalidatedInner<D, E> = Pin<Box<StreamBody<FrameStream<D, E>>>>;

struct RevalidatedBody<B>
where
    B: HttpBody + Send + 'static,
    B::Data: Send + 'static,
    B::Error: Send + 'static,
{
    inner: RevalidatedInner<B::Data, B::Error>,
}

impl<B> RevalidatedBody<B>
where
    B: HttpBody + Send + 'static,
    B::Data: Send + 'static,
    B::Error: Send + 'static,
{
    fn new(
        body: B,
        identity: Identity,
        authenticator: Option<Arc<dyn Authenticator>>,
        config: RevalidationConfig,
    ) -> Self {
        Self {
            inner: Box::pin(StreamBody::new(revalidate_stream(
                BodyStream::new(body),
                identity,
                authenticator,
                config,
            ))),
        }
    }
}

impl<B> std::fmt::Debug for RevalidatedBody<B>
where
    B: HttpBody + Send + 'static,
    B::Data: Send + 'static,
    B::Error: Send + 'static,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RevalidatedBody").finish_non_exhaustive()
    }
}

impl<B> HttpBody for RevalidatedBody<B>
where
    B: HttpBody + Send + 'static,
    B::Data: Send + 'static,
    B::Error: Send + 'static,
{
    type Data = B::Data;
    type Error = B::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        self.inner.as_mut().poll_frame(cx)
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        HttpBody::size_hint(&self.inner)
    }
}

fn extract_bearer(request: &Request) -> Option<String> {
    let header_value = request.headers().get(AUTHORIZATION)?;
    let raw = header_value.to_str().ok()?;
    Some(strip_bearer_scheme(raw)?.to_string())
}

/// Strip the `Bearer` auth scheme, returning the trimmed credential.
///
/// RFC 9110 §11.1 / RFC 6750 §2.1: the scheme token is
/// case-insensitive, so a spec-compliant `bearer <token>` must be
/// accepted alongside `Bearer <token>`. Matching is ASCII-only; the
/// whitespace contract is unchanged from the original (a single space
/// after the scheme), and an empty credential fails closed to `None`.
fn strip_bearer_scheme(raw: &str) -> Option<&str> {
    let (scheme, token) = raw.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let token = token.trim();
    (!token.is_empty()).then_some(token)
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
    use std::convert::Infallible;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, SystemTime};

    use async_trait::async_trait;
    use axum::Router;
    use axum::body::{Body, Bytes, to_bytes};
    use axum::http::{Method, Request, StatusCode, header};
    use axum::response::Response;
    use axum::routing::any;
    use futures_util::stream;
    use proxima_core::{
        AuthError, AuthPath, Authenticator, AuthzContext, Credentials, Identity, Owner, OwnerRef,
        RevalidationConfig, UserId,
    };
    use tokio::sync::mpsc;
    use tokio::time;
    use tower::ServiceExt;

    use super::{
        HostAllowlist, OriginAllowlist, RevalidatedBody, cors_layer, default_allowlist,
        host_guard_layer, mcp_auth_layer_with_sessions,
    };
    use crate::McpServerError;
    use crate::auth::McpEdgeAuth;
    use crate::session::{McpSessionBindings, owner_key};

    /// Host authenticator that accepts exactly `good-token` for `owner`.
    struct TokenAuth {
        owner: Owner,
    }

    #[async_trait]
    impl Authenticator for TokenAuth {
        async fn authenticate(&self, creds: &Credentials) -> Result<AuthzContext, AuthError> {
            match creds {
                Credentials::Bearer(token) if token == "good-token" => Ok(
                    AuthzContext::single_owner(&self.owner, AuthPath::HostBearer),
                ),
                Credentials::Bearer(_) => Err(AuthError::InvalidCredentials),
            }
        }
    }

    struct CountingTokenAuth {
        owner: Owner,
        calls: Arc<AtomicU64>,
    }

    #[async_trait]
    impl Authenticator for CountingTokenAuth {
        async fn authenticate(&self, creds: &Credentials) -> Result<AuthzContext, AuthError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            match creds {
                Credentials::Bearer(token) if token == "good-token" => Ok(
                    AuthzContext::single_owner(&self.owner, AuthPath::HostBearer),
                ),
                Credentials::Bearer(_) => Err(AuthError::InvalidCredentials),
            }
        }
    }

    fn user_owner() -> Owner {
        OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()))
    }

    /// Router mirroring the production stack: the auth layer over an OK
    /// `/mcp` stub, seeded with `sessions` and a `TokenAuth` for `owner`.
    fn auth_app(owner: Owner, sessions: McpSessionBindings) -> Router {
        auth_app_with_auth(Arc::new(TokenAuth { owner }), sessions)
    }

    fn auth_app_with_auth(
        authenticator: Arc<dyn Authenticator>,
        sessions: McpSessionBindings,
    ) -> Router {
        let auth = McpEdgeAuth::headless().with_host(authenticator);
        Router::new()
            .route("/mcp", any(|| async { StatusCode::OK }))
            .layer(mcp_auth_layer_with_sessions(
                Arc::new(auth),
                sessions,
                RevalidationConfig::default(),
                None,
            ))
    }

    fn mcp_request(bearer: &str) -> axum::http::request::Builder {
        Request::builder()
            .uri("/mcp")
            .header("Origin", "http://localhost")
            .header("Authorization", bearer)
    }

    async fn status_of(app: Router, request: Request<Body>) -> StatusCode {
        app.oneshot(request).await.unwrap().status()
    }

    fn cors_app(calls: Arc<AtomicU64>) -> Router {
        Router::new()
            .route(
                "/ok",
                any(move || {
                    let calls = Arc::clone(&calls);
                    async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        StatusCode::OK
                    }
                }),
            )
            .layer(cors_layer(default_allowlist()))
    }

    #[tokio::test]
    async fn allowed_preflight_is_no_content_and_skips_the_route() {
        let calls = Arc::new(AtomicU64::new(0));
        let response = cors_app(Arc::clone(&calls))
            .oneshot(
                Request::builder()
                    .method(Method::OPTIONS)
                    .uri("/ok")
                    .header(header::ORIGIN, "http://localhost:5173")
                    .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
                    .header(
                        header::ACCESS_CONTROL_REQUEST_HEADERS,
                        "authorization,content-type,x-proxima-owner",
                    )
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            response.headers().get(header::ACCESS_CONTROL_ALLOW_ORIGIN),
            Some(&header::HeaderValue::from_static("http://localhost:5173"))
        );
        assert_eq!(
            response.headers().get(header::ACCESS_CONTROL_ALLOW_METHODS),
            Some(&header::HeaderValue::from_static("POST"))
        );
        assert_eq!(
            response.headers().get(header::ACCESS_CONTROL_ALLOW_HEADERS),
            Some(&header::HeaderValue::from_static(
                "authorization,content-type,x-proxima-owner"
            ))
        );
        assert!(
            !response
                .headers()
                .contains_key(header::ACCESS_CONTROL_ALLOW_CREDENTIALS)
        );
        let vary: Vec<_> = response
            .headers()
            .get_all(header::VARY)
            .iter()
            .map(|value| value.to_str().unwrap())
            .collect();
        assert_eq!(
            vary,
            [
                "Origin",
                "Access-Control-Request-Method",
                "Access-Control-Request-Headers"
            ]
        );
    }

    #[tokio::test]
    async fn repeated_request_header_lists_are_all_reflected() {
        let calls = Arc::new(AtomicU64::new(0));
        let mut request = Request::builder()
            .method(Method::OPTIONS)
            .uri("/ok")
            .header(header::ORIGIN, "http://localhost")
            .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
            .header(header::ACCESS_CONTROL_REQUEST_HEADERS, "authorization")
            .body(Body::empty())
            .unwrap();
        request.headers_mut().append(
            header::ACCESS_CONTROL_REQUEST_HEADERS,
            header::HeaderValue::from_static("content-type, x-proxima-owner"),
        );

        let response = cors_app(Arc::clone(&calls)).oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let allowed_headers: Vec<_> = response
            .headers()
            .get_all(header::ACCESS_CONTROL_ALLOW_HEADERS)
            .iter()
            .map(|value| value.to_str().unwrap())
            .collect();
        assert_eq!(
            allowed_headers,
            ["authorization", "content-type, x-proxima-owner"]
        );
    }

    #[tokio::test]
    async fn malformed_or_repeated_preflight_fields_fail_closed() {
        let calls = Arc::new(AtomicU64::new(0));
        let app = cors_app(Arc::clone(&calls));

        let mut repeated_method = Request::builder()
            .method(Method::OPTIONS)
            .uri("/ok")
            .header(header::ORIGIN, "http://localhost")
            .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
            .body(Body::empty())
            .unwrap();
        repeated_method.headers_mut().append(
            header::ACCESS_CONTROL_REQUEST_METHOD,
            header::HeaderValue::from_static("DELETE"),
        );
        let malformed_method = Request::builder()
            .method(Method::OPTIONS)
            .uri("/ok")
            .header(header::ORIGIN, "http://localhost")
            .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST, DELETE")
            .body(Body::empty())
            .unwrap();
        let malformed_headers = Request::builder()
            .method(Method::OPTIONS)
            .uri("/ok")
            .header(header::ORIGIN, "http://localhost")
            .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
            .header(
                header::ACCESS_CONTROL_REQUEST_HEADERS,
                "authorization, not a header",
            )
            .body(Body::empty())
            .unwrap();

        for request in [repeated_method, malformed_method, malformed_headers] {
            let response = app.clone().oneshot(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            assert!(
                !response
                    .headers()
                    .contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            );
            assert_eq!(
                to_bytes(response.into_body(), usize::MAX).await.unwrap(),
                Bytes::from_static(b"invalid CORS preflight")
            );
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn allowed_actual_response_echoes_origin_and_exposes_browser_headers() {
        let calls = Arc::new(AtomicU64::new(0));
        let response = cors_app(Arc::clone(&calls))
            .oneshot(
                Request::builder()
                    .uri("/ok")
                    .header(header::ORIGIN, "http://127.0.0.1:5173")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            response.headers().get(header::ACCESS_CONTROL_ALLOW_ORIGIN),
            Some(&header::HeaderValue::from_static("http://127.0.0.1:5173"))
        );
        assert_eq!(
            response
                .headers()
                .get(header::ACCESS_CONTROL_EXPOSE_HEADERS),
            Some(&header::HeaderValue::from_static(
                "Mcp-Session-Id, WWW-Authenticate"
            ))
        );
        assert_eq!(
            response.headers().get(header::VARY),
            Some(&header::HeaderValue::from_static("Origin"))
        );
    }

    #[tokio::test]
    async fn cors_appends_vary_without_replacing_route_values() {
        let app = Router::new()
            .route(
                "/ok",
                any(|| async {
                    Response::builder()
                        .status(StatusCode::OK)
                        .header(header::VARY, "Accept-Encoding")
                        .body(Body::empty())
                        .unwrap()
                }),
            )
            .layer(cors_layer(default_allowlist()));
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/ok")
                    .header(header::ORIGIN, "http://localhost")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let vary: Vec<_> = response
            .headers()
            .get_all(header::VARY)
            .iter()
            .map(|value| value.to_str().unwrap())
            .collect();
        assert_eq!(vary, ["Accept-Encoding", "Origin"]);
    }

    #[tokio::test]
    async fn disallowed_or_repeated_origin_is_rejected_before_the_route() {
        let calls = Arc::new(AtomicU64::new(0));
        let app = cors_app(Arc::clone(&calls));
        let disallowed = Request::builder()
            .uri("/ok")
            .header(header::ORIGIN, "https://evil.test")
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            app.clone().oneshot(disallowed).await.unwrap().status(),
            StatusCode::FORBIDDEN
        );

        let mut repeated = Request::builder()
            .uri("/ok")
            .header(header::ORIGIN, "http://localhost")
            .body(Body::empty())
            .unwrap();
        repeated.headers_mut().append(
            header::ORIGIN,
            header::HeaderValue::from_static("http://127.0.0.1"),
        );
        assert_eq!(
            app.oneshot(repeated).await.unwrap().status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn no_origin_and_non_preflight_options_keep_route_semantics() {
        let calls = Arc::new(AtomicU64::new(0));
        let app = cors_app(Arc::clone(&calls));
        let native = app
            .clone()
            .oneshot(Request::builder().uri("/ok").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(native.status(), StatusCode::OK);
        assert!(
            !native
                .headers()
                .contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN)
        );
        assert_eq!(
            native.headers().get(header::VARY),
            Some(&header::HeaderValue::from_static("Origin"))
        );

        let options = app
            .oneshot(
                Request::builder()
                    .method(Method::OPTIONS)
                    .uri("/ok")
                    .header(header::ORIGIN, "http://localhost")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(options.status(), StatusCode::OK);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(
            options.headers().get(header::ACCESS_CONTROL_ALLOW_ORIGIN),
            Some(&header::HeaderValue::from_static("http://localhost"))
        );
    }

    fn host_guard_app(additional_hosts: &[&str]) -> Router {
        Router::new()
            .route("/ok", any(|| async { StatusCode::OK }))
            .layer(host_guard_layer(HostAllowlist::new(additional_hosts)))
    }

    fn request_with_host(uri: &str, host: &str) -> Request<Body> {
        Request::builder()
            .uri(uri)
            .header(http::header::HOST, host)
            .body(Body::empty())
            .unwrap()
    }

    #[tokio::test]
    async fn host_guard_default_is_loopback_only_and_port_agnostic() {
        let app = host_guard_app(&[]);
        for host in [
            "localhost",
            "LOCALHOST:31415",
            "127.0.0.1:31415",
            "[::1]",
            "[::1]:31415",
        ] {
            assert_eq!(
                status_of(app.clone(), request_with_host("/ok", host)).await,
                StatusCode::OK,
                "loopback Host {host:?} must be accepted"
            );
        }
        for host in ["localhost.evil", "evil.test"] {
            assert_eq!(
                status_of(app.clone(), request_with_host("/ok", host)).await,
                StatusCode::FORBIDDEN,
                "foreign Host {host:?} must be rejected"
            );
        }
    }

    #[tokio::test]
    async fn host_guard_matches_bare_hosts_and_explicit_ports_like_rmcp() {
        let app = host_guard_app(&["example.test", "port.test:8443"]);
        for host in ["example.test", "example.test:443", "port.test:8443"] {
            assert_eq!(
                status_of(app.clone(), request_with_host("/ok", host)).await,
                StatusCode::OK,
                "allowed Host {host:?} must be accepted"
            );
        }
        for host in ["port.test", "port.test:443"] {
            assert_eq!(
                status_of(app.clone(), request_with_host("/ok", host)).await,
                StatusCode::FORBIDDEN,
                "wrong explicit port for {host:?} must be rejected"
            );
        }
    }

    #[tokio::test]
    async fn host_guard_normalizes_ipv6_and_preserves_explicit_ports() {
        let app = host_guard_app(&["2001:db8::1", "[2001:db8::2]:8443"]);
        for host in ["[2001:DB8::1]", "[2001:db8::1]:443", "[2001:db8::2]:8443"] {
            assert_eq!(
                status_of(app.clone(), request_with_host("/ok", host)).await,
                StatusCode::OK,
                "allowed IPv6 authority {host:?} must be accepted"
            );
        }
        for host in ["[2001:db8::2]", "[2001:db8::2]:443"] {
            assert_eq!(
                status_of(app.clone(), request_with_host("/ok", host)).await,
                StatusCode::FORBIDDEN,
                "wrong IPv6 port for {host:?} must be rejected"
            );
        }
    }

    #[tokio::test]
    async fn host_header_wins_and_uri_authority_is_the_http2_fallback() {
        let app = host_guard_app(&[]);
        assert_eq!(
            status_of(
                app.clone(),
                request_with_host("http://localhost/ok", "evil.test"),
            )
            .await,
            StatusCode::FORBIDDEN
        );
        let authority_only = Request::builder()
            .uri("http://LOCALHOST:31415/ok")
            .body(Body::empty())
            .unwrap();
        assert_eq!(status_of(app.clone(), authority_only).await, StatusCode::OK);
        let foreign_authority = Request::builder()
            .uri("http://evil.test/ok")
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            status_of(app, foreign_authority).await,
            StatusCode::FORBIDDEN
        );
    }

    #[tokio::test]
    async fn missing_or_malformed_host_is_bad_request_with_rmcp_text() {
        let app = host_guard_app(&[]);
        let missing = Request::builder().uri("/ok").body(Body::empty()).unwrap();
        let missing = app.clone().oneshot(missing).await.unwrap();
        assert_eq!(missing.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            missing.headers().get(http::header::CONTENT_TYPE).unwrap(),
            "text/plain; charset=utf-8"
        );
        assert_eq!(
            to_bytes(missing.into_body(), usize::MAX).await.unwrap(),
            Bytes::from_static(b"Bad Request: missing Host header")
        );

        let malformed = request_with_host("http://localhost/ok", "bad host");
        let malformed = app.clone().oneshot(malformed).await.unwrap();
        assert_eq!(malformed.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            to_bytes(malformed.into_body(), usize::MAX).await.unwrap(),
            Bytes::from_static(b"Bad Request: Invalid Host header")
        );

        let mut non_utf8 = Request::builder()
            .uri("http://localhost/ok")
            .body(Body::empty())
            .unwrap();
        non_utf8.headers_mut().insert(
            http::header::HOST,
            http::HeaderValue::from_bytes(&[0xff]).unwrap(),
        );
        let non_utf8 = app.oneshot(non_utf8).await.unwrap();
        assert_eq!(non_utf8.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            to_bytes(non_utf8.into_body(), usize::MAX).await.unwrap(),
            Bytes::from_static(b"Bad Request: Invalid Host header encoding")
        );
    }

    #[tokio::test]
    async fn disallowed_host_uses_rmcp_forbidden_response_shape() {
        let response = host_guard_app(&[])
            .oneshot(request_with_host("/ok", "evil.test"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert!(!response.headers().contains_key(http::header::CONTENT_TYPE));
        assert_eq!(
            to_bytes(response.into_body(), usize::MAX).await.unwrap(),
            Bytes::from_static(b"Forbidden: Host header is not allowed")
        );
    }

    // A bad token yields 401 even when a valid owner is selected.
    #[tokio::test]
    async fn bad_token_with_valid_owner_is_unauthorized() {
        let owner = user_owner();
        let app = auth_app(owner, McpSessionBindings::new());
        let request = mcp_request("Bearer bad-token")
            .header("X-Proxima-Owner", owner_key(owner))
            .body(Body::empty())
            .unwrap();
        assert_eq!(status_of(app, request).await, StatusCode::UNAUTHORIZED);
    }

    // A bad token with a present-but-unbound session id yields
    // 401 (not 404 and not 403) — token validity is checked first.
    #[tokio::test]
    async fn bad_token_with_unbound_session_is_unauthorized() {
        let owner = user_owner();
        let app = auth_app(owner, McpSessionBindings::new());
        let request = mcp_request("Bearer bad-token")
            .header("Mcp-Session-Id", "never-bound")
            .body(Body::empty())
            .unwrap();
        assert_eq!(status_of(app, request).await, StatusCode::UNAUTHORIZED);
    }

    // A valid token with a present-but-unbound session id yields 404
    // so a standard Streamable-HTTP client re-initializes.
    #[tokio::test]
    async fn valid_token_with_unbound_session_is_not_found() {
        let owner = user_owner();
        let app = auth_app(owner, McpSessionBindings::new());
        let request = mcp_request("Bearer good-token")
            .header("Mcp-Session-Id", "never-bound")
            .body(Body::empty())
            .unwrap();
        assert_eq!(status_of(app, request).await, StatusCode::NOT_FOUND);
    }

    // A valid token with no session id and no owner header yields 403:
    // owner selection is required once the token is known-valid.
    #[tokio::test]
    async fn valid_token_without_owner_selection_is_forbidden() {
        let owner = user_owner();
        let app = auth_app(owner, McpSessionBindings::new());
        let request = mcp_request("Bearer good-token")
            .body(Body::empty())
            .unwrap();
        assert_eq!(status_of(app, request).await, StatusCode::FORBIDDEN);
    }

    // A valid token + valid owner header authorizes (no session yet).
    #[tokio::test]
    async fn valid_token_with_owner_header_authorizes() {
        let owner = user_owner();
        let app = auth_app(owner, McpSessionBindings::new());
        let request = mcp_request("Bearer good-token")
            .header("X-Proxima-Owner", owner_key(owner))
            .body(Body::empty())
            .unwrap();
        assert_eq!(status_of(app, request).await, StatusCode::OK);
    }

    // A valid token against an already-bound session authorizes without an
    // owner header.
    #[tokio::test]
    async fn valid_token_with_bound_session_authorizes() {
        let owner = user_owner();
        let sessions = McpSessionBindings::new();
        sessions.bind("sess-1", owner).await;
        let app = auth_app(owner, sessions);
        let request = mcp_request("Bearer good-token")
            .header("Mcp-Session-Id", "sess-1")
            .body(Body::empty())
            .unwrap();
        assert_eq!(status_of(app, request).await, StatusCode::OK);
    }

    #[tokio::test]
    async fn successful_requests_authenticate_bearer_exactly_once_each() {
        let owner = user_owner();
        let calls = Arc::new(AtomicU64::new(0));
        let app = auth_app_with_auth(
            Arc::new(CountingTokenAuth {
                owner,
                calls: calls.clone(),
            }),
            McpSessionBindings::new(),
        );

        for expected_calls in 1..=2 {
            let request = mcp_request("Bearer good-token")
                .header("X-Proxima-Owner", owner_key(owner))
                .body(Body::empty())
                .unwrap();
            assert_eq!(status_of(app.clone(), request).await, StatusCode::OK);
            assert_eq!(calls.load(Ordering::SeqCst), expected_calls);
        }
    }

    #[tokio::test]
    async fn foreign_owner_session_matches_unknown_session_response() {
        let authorized_owner = user_owner();
        let foreign_owner = user_owner();
        let sessions = McpSessionBindings::new();
        sessions.bind("foreign-session", foreign_owner).await;
        let app = auth_app(authorized_owner, sessions);

        let foreign_request = mcp_request("Bearer good-token")
            .header("Mcp-Session-Id", "foreign-session")
            .body(Body::empty())
            .unwrap();
        let foreign_response = app.clone().oneshot(foreign_request).await.unwrap();
        let unknown_request = mcp_request("Bearer good-token")
            .header("Mcp-Session-Id", "never-bound")
            .body(Body::empty())
            .unwrap();
        let unknown_response = app.oneshot(unknown_request).await.unwrap();

        assert_eq!(foreign_response.status(), StatusCode::NOT_FOUND);
        assert_eq!(foreign_response.status(), unknown_response.status());
        assert_eq!(foreign_response.headers(), unknown_response.headers());
        let foreign_body = to_bytes(foreign_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let unknown_body = to_bytes(unknown_response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(foreign_body, unknown_body);
        assert_eq!(
            foreign_body,
            Bytes::from_static(b"unknown or expired MCP session")
        );
    }

    #[tokio::test]
    async fn foreign_owner_header_remains_unauthorized() {
        let authorized_owner = user_owner();
        let foreign_owner = user_owner();
        let app = auth_app(authorized_owner, McpSessionBindings::new());
        let request = mcp_request("Bearer good-token")
            .header("X-Proxima-Owner", owner_key(foreign_owner))
            .body(Body::empty())
            .unwrap();

        assert_eq!(status_of(app, request).await, StatusCode::UNAUTHORIZED);
    }

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

    #[test]
    fn strip_bearer_scheme_is_case_insensitive_per_rfc() {
        use super::strip_bearer_scheme;
        // RFC 9110 §11.1: the scheme token is case-insensitive.
        assert_eq!(strip_bearer_scheme("Bearer tok"), Some("tok"));
        assert_eq!(strip_bearer_scheme("bearer tok"), Some("tok"));
        assert_eq!(strip_bearer_scheme("BEARER tok"), Some("tok"));
        assert_eq!(strip_bearer_scheme("BeArEr tok"), Some("tok"));
        // Extra space between scheme and credential is tolerated.
        assert_eq!(strip_bearer_scheme("Bearer   tok"), Some("tok"));
        // Non-bearer schemes and empty credentials fail closed.
        assert_eq!(strip_bearer_scheme("Basic tok"), None);
        assert_eq!(strip_bearer_scheme("tok"), None);
        assert_eq!(strip_bearer_scheme("Bearer "), None);
        assert_eq!(strip_bearer_scheme("Bearer    "), None);
    }

    fn assert_invalid_origin(pattern: &str) {
        let err = OriginAllowlist::parse([pattern]).unwrap_err();
        let McpServerError::InvalidOrigin(message) = err else {
            panic!("expected invalid origin");
        };
        assert!(message.contains(pattern));
    }

    fn identity(expires_at: Option<SystemTime>, auth_epoch: u64) -> Identity {
        let principal = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));
        let OwnerRef::Personal(subject) = principal else {
            unreachable!("principal is personal");
        };
        proxima_core::AuthzContext::for_subject(subject, proxima_core::AuthPath::HostBearer)
            .with_expires_at(expires_at)
            .with_auth_epoch(auth_epoch)
            .identity_for_revalidation()
    }

    fn receiver_body(receiver: mpsc::UnboundedReceiver<Result<Bytes, Infallible>>) -> Body {
        Body::from_stream(stream::unfold(receiver, |mut receiver| async {
            receiver.recv().await.map(|item| (item, receiver))
        }))
    }

    fn collect_revalidated(
        body: Body,
        identity: Identity,
        authenticator: Option<Arc<dyn Authenticator>>,
        config: RevalidationConfig,
    ) -> tokio::task::JoinHandle<Bytes> {
        tokio::spawn(async move {
            to_bytes(
                Body::new(RevalidatedBody::new(body, identity, authenticator, config)),
                usize::MAX,
            )
            .await
            .expect("body collection succeeds")
        })
    }

    #[derive(Debug, Default)]
    struct EpochAuthenticator {
        epoch: AtomicU64,
    }

    #[async_trait]
    impl Authenticator for EpochAuthenticator {
        async fn authenticate(&self, _creds: &Credentials) -> Result<AuthzContext, AuthError> {
            Err(AuthError::AuthRequired)
        }

        async fn current_auth_epoch(&self, _principal: &OwnerRef) -> u64 {
            self.epoch.load(Ordering::SeqCst)
        }
    }

    #[tokio::test(start_paused = true)]
    async fn revalidated_body_expiry_ends_active_body() {
        let config = RevalidationConfig {
            max_stream_lifetime: Duration::from_mins(1),
            epoch_check_interval: Duration::from_secs(5),
        };
        let (sender, receiver) = mpsc::unbounded_channel();
        sender
            .send(Ok(Bytes::from_static(b"before-expiry")))
            .expect("receiver is alive");
        let task = collect_revalidated(
            receiver_body(receiver),
            identity(Some(SystemTime::now() + Duration::from_secs(2)), 0),
            None,
            config,
        );

        tokio::task::yield_now().await;
        time::advance(Duration::from_secs(2)).await;

        assert_eq!(task.await.unwrap(), Bytes::from_static(b"before-expiry"));
    }

    #[tokio::test(start_paused = true)]
    async fn revalidated_body_epoch_bump_ends_within_interval() {
        let config = RevalidationConfig {
            max_stream_lifetime: Duration::from_mins(1),
            epoch_check_interval: Duration::from_secs(5),
        };
        let authenticator = Arc::new(EpochAuthenticator::default());
        let (_sender, receiver) = mpsc::unbounded_channel();
        let task = collect_revalidated(
            receiver_body(receiver),
            identity(None, 0),
            Some(authenticator.clone()),
            config,
        );

        tokio::task::yield_now().await;
        authenticator.epoch.store(1, Ordering::SeqCst);
        time::advance(Duration::from_secs(5)).await;

        assert!(task.await.unwrap().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn revalidated_body_max_lifetime_ends_active_body() {
        let config = RevalidationConfig {
            max_stream_lifetime: Duration::from_secs(3),
            epoch_check_interval: Duration::from_secs(5),
        };
        let (_sender, receiver) = mpsc::unbounded_channel();
        let task = collect_revalidated(receiver_body(receiver), identity(None, 0), None, config);

        tokio::task::yield_now().await;
        time::advance(Duration::from_secs(3)).await;

        assert!(task.await.unwrap().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn revalidated_body_leaves_short_response_unchanged() {
        let config = RevalidationConfig {
            max_stream_lifetime: Duration::from_mins(1),
            epoch_check_interval: Duration::from_secs(5),
        };
        let bytes = to_bytes(
            Body::new(RevalidatedBody::new(
                Body::from("short"),
                identity(None, 0),
                None,
                config,
            )),
            usize::MAX,
        )
        .await
        .unwrap();

        assert_eq!(bytes, Bytes::from_static(b"short"));
    }
}
