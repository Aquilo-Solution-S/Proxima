use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::{self, FromFnLayer, Next};
use axum::response::{IntoResponse, Response};
use futures_util::Stream;
use http::HeaderMap;
use http::header::{AUTHORIZATION, ORIGIN};
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
    allowlist: OriginAllowlist,
    revalidation: RevalidationConfig,
    www_authenticate: Option<http::HeaderValue>,
}

/// Bearer-token middleware that resolves `Authorization: Bearer
/// <wire-token>` via MCP edge auth and injects the resolved context into
/// request extensions. Accepted bearer material is host-authenticated;
/// reserved legacy local-token prefixes fail closed.
/// Missing or unknown tokens short-circuit with HTTP 401. A present but
/// disallowed `Origin` short-circuits with 403; missing `Origin` is
/// allowed after bearer auth for native CLI clients.
///
/// Returns a [`McpAuthLayer`] (alias of [`FromFnLayer`]) so callers
/// apply it directly with [`axum::Router::layer`].
pub fn mcp_auth_layer_with_config(
    auth: Arc<McpEdgeAuth>,
    allowlist: OriginAllowlist,
    revalidation: RevalidationConfig,
) -> McpAuthLayer {
    mcp_auth_layer_with_metadata(auth, allowlist, revalidation, None)
}

#[must_use = "apply the returned layer to the MCP router"]
pub fn mcp_auth_layer_with_metadata(
    auth: Arc<McpEdgeAuth>,
    allowlist: OriginAllowlist,
    revalidation: RevalidationConfig,
    www_authenticate: Option<http::HeaderValue>,
) -> McpAuthLayer {
    mcp_auth_layer_with_sessions(
        auth,
        McpSessionBindings::new(),
        allowlist,
        revalidation,
        www_authenticate,
    )
}

#[must_use = "apply the returned layer to the MCP router"]
pub fn mcp_auth_layer_with_sessions(
    auth: Arc<McpEdgeAuth>,
    sessions: McpSessionBindings,
    allowlist: OriginAllowlist,
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
            allowlist,
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
    if request.headers().contains_key(ORIGIN) && !state.allowlist.allows(request.headers()) {
        return (StatusCode::FORBIDDEN, "origin not allowed").into_response();
    }
    // Validate the bearer BEFORE resolving owner/session so an invalid
    // token always yields 401 regardless of session/owner-header state.
    // This closes the 401/403 oracle: an unauthenticated caller learns
    // nothing about owner or session requirements. Owner narrowing still
    // happens in `resolve` below.
    if !state.auth.accepts_token(&token).await {
        return unauthorized(&state);
    }
    let (selected_owner, bind_new_session) =
        match selected_owner(&state.sessions, request.headers()).await {
            OwnerSelection::Selected { owner, bind_new } => (owner, bind_new),
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
    let Some(ctx) = state.auth.resolve(&token, selected_owner).await else {
        return unauthorized(&state);
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
    /// response's freshly minted session id to this owner.
    Selected { owner: Owner, bind_new: bool },
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
        };
    }

    match owner_header.and_then(parse_owner_key) {
        Some(owner) => OwnerSelection::Selected {
            owner,
            bind_new: true,
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
    use axum::http::{Request, StatusCode};
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
        OriginAllowlist, RevalidatedBody, default_allowlist, mcp_auth_layer_with_sessions,
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

    fn user_owner() -> Owner {
        OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()))
    }

    /// Router mirroring the production stack: the auth layer over an OK
    /// `/mcp` stub, seeded with `sessions` and a `TokenAuth` for `owner`.
    fn auth_app(owner: Owner, sessions: McpSessionBindings) -> Router {
        let auth = McpEdgeAuth::headless().with_host(Arc::new(TokenAuth { owner }));
        Router::new()
            .route("/mcp", any(|| async { StatusCode::OK }))
            .layer(mcp_auth_layer_with_sessions(
                Arc::new(auth),
                sessions,
                default_allowlist(),
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
