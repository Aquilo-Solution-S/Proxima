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
use proxima_core::{AuthPath, Authenticator, Identity, RevalidationConfig, revalidate_stream};

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
    revalidation: RevalidationConfig,
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
    mcp_auth_layer_with_config(auth, allowlist, RevalidationConfig::default())
}

/// Bearer-token middleware with explicit stream revalidation config.
pub fn mcp_auth_layer_with_config(
    auth: Arc<McpEdgeAuth>,
    allowlist: OriginAllowlist,
    revalidation: RevalidationConfig,
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
            allowlist,
            revalidation,
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
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let Some(ctx) = state.auth.resolve(&token).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    if request.headers().contains_key(ORIGIN) && !state.allowlist.allows(request.headers()) {
        return (StatusCode::FORBIDDEN, "origin not allowed").into_response();
    }
    let identity = ctx.authz.identity.clone();
    let epoch_source = if ctx.authz.auth_path == AuthPath::HostBearer {
        state.auth.host_authenticator()
    } else {
        None
    };
    request.extensions_mut().insert(ctx);
    revalidate_response(
        next.run(request).await,
        identity,
        epoch_source,
        state.revalidation,
    )
}

fn revalidate_response(
    response: Response,
    identity: Identity,
    authenticator: Option<Arc<dyn Authenticator>>,
    config: RevalidationConfig,
) -> Response {
    response.map(|body| Body::new(RevalidatedBody::new(body, identity, authenticator, config)))
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
    use std::collections::HashSet;
    use std::convert::Infallible;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, SystemTime};

    use async_trait::async_trait;
    use axum::body::{Body, Bytes, to_bytes};
    use futures_util::stream;
    use proxima_core::{
        AuthError, Authenticator, AuthzContext, Credentials, Identity, Principal,
        RevalidationConfig, UserId,
    };
    use tokio::sync::mpsc;
    use tokio::time;

    use super::{OriginAllowlist, RevalidatedBody};
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

    fn identity(expires_at: Option<SystemTime>, auth_epoch: u64) -> Identity {
        let principal = Principal::User(UserId::new(uuid::Uuid::now_v7()));
        let mut accessible_principals = HashSet::with_capacity(1);
        accessible_principals.insert(principal.clone());
        Identity {
            principal,
            accessible_principals,
            expires_at,
            auth_epoch,
        }
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

        async fn current_auth_epoch(&self, _principal: &Principal) -> u64 {
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
