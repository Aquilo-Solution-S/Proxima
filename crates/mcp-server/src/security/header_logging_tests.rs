//! Assert boundary event fields before any text/Debug log formatter sees them.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::any;
use http::HeaderValue;
use http::header::{AUTHORIZATION, HOST, ORIGIN};
use tower::ServiceExt;
use tracing::field::{Field, Visit};
use tracing::instrument::WithSubscriber;
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Level, Metadata, Subscriber};

use super::{
    HostAllowlist, HostRequestError, cors_layer, default_allowlist, host_guard_layer,
    mcp_auth_layer_with_config, parse_request_host,
};
use crate::McpEdgeAuth;

const HOST_SENTINEL: &str = "host-sentinel.invalid";
const URI_SENTINEL: &str = "uri-sentinel";
const AUTH_SENTINEL: &str = "auth-sentinel";
const PAYLOAD_SENTINEL: &str = "payload-sentinel";
const ORIGIN_SENTINEL: &str = "origin-sentinel.invalid";
const MALFORMED_HOST: &str = "rejected request with malformed Host header";
const DISALLOWED_HOST: &str =
    "rejected request with disallowed Host header (possible DNS rebinding attempt)";
const ORIGIN_REFUSAL: &str = "rejected request with disallowed or malformed Origin header";

#[derive(Debug, Clone, PartialEq, Eq)]
struct Warning {
    level: Level,
    target: &'static str,
    fields: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default)]
struct Warnings(Arc<Mutex<Vec<Warning>>>);

#[derive(Default)]
struct Fields(BTreeMap<String, String>);

impl Visit for Fields {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.0.insert(field.name().to_owned(), value.to_owned());
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.0.insert(field.name().to_owned(), format!("{value:?}"));
    }
}

impl Subscriber for Warnings {
    fn enabled(&self, _: &Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, _: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }

    fn record(&self, _: &Id, _: &Record<'_>) {}

    fn record_follows_from(&self, _: &Id, _: &Id) {}

    fn event(&self, event: &Event<'_>) {
        let mut fields = Fields::default();
        event.record(&mut fields);
        self.0.lock().unwrap().push(Warning {
            level: *event.metadata().level(),
            target: event.metadata().target(),
            fields: fields.0,
        });
    }

    fn enter(&self, _: &Id) {}

    fn exit(&self, _: &Id) {}
}

fn assert_warning(warnings: &[Warning], message: &str, category: Option<&str>) {
    let mut fields = BTreeMap::from([("message".to_owned(), message.to_owned())]);
    if let Some(category) = category {
        fields.insert("category".to_owned(), category.to_owned());
    }
    assert_eq!(
        warnings,
        [Warning {
            level: Level::WARN,
            target: "proxima_mcp_server::security",
            fields,
        }]
    );
    let formatted = format!("{warnings:?}");
    for sentinel in [
        HOST_SENTINEL,
        URI_SENTINEL,
        AUTH_SENTINEL,
        PAYLOAD_SENTINEL,
        ORIGIN_SENTINEL,
    ] {
        assert!(
            !formatted.contains(sentinel),
            "request data leaked: {formatted}"
        );
    }
}

fn guarded_app() -> Router {
    Router::new()
        .route("/mcp", any(|| async { StatusCode::OK }))
        .layer(mcp_auth_layer_with_config(
            Arc::new(McpEdgeAuth::headless()),
            proxima_core::RevalidationConfig::default(),
        ))
        .layer(cors_layer(default_allowlist()))
        .layer(host_guard_layer(HostAllowlist::default()))
}

fn request(uri: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header(AUTHORIZATION, format!("Bearer {AUTH_SENTINEL}"))
        .body(Body::from(PAYLOAD_SENTINEL))
        .unwrap()
}

async fn observe(request: Request<Body>) -> (StatusCode, Vec<Warning>) {
    let warnings = Warnings::default();
    let response = guarded_app()
        .oneshot(request)
        .with_subscriber(warnings.clone())
        .await
        .unwrap();
    let events = warnings.0.lock().unwrap().clone();
    (response.status(), events)
}

#[tokio::test(flavor = "current_thread")]
async fn host_header_refusals_log_only_fixed_messages_and_categories() {
    for (host, status, message, category) in [
        (
            HeaderValue::from_bytes(b"host-sentinel.invalid-\xff").unwrap(),
            StatusCode::BAD_REQUEST,
            "rejected request with non-UTF-8 Host header",
            "non_utf8",
        ),
        (
            HeaderValue::from_static("host-sentinel.invalid/forbidden-path"),
            StatusCode::BAD_REQUEST,
            MALFORMED_HOST,
            "malformed",
        ),
        (
            HeaderValue::from_bytes(b"host-sentinel.invalid\tinjected").unwrap(),
            StatusCode::BAD_REQUEST,
            MALFORMED_HOST,
            "malformed",
        ),
        (
            HeaderValue::from_static("host-sentinel.invalid:1234"),
            StatusCode::FORBIDDEN,
            DISALLOWED_HOST,
            "not_allowed",
        ),
    ] {
        let mut request = request("http://localhost/mcp?uri-sentinel");
        request.headers_mut().insert(HOST, host);
        let (actual, warnings) = observe(request).await;
        assert_eq!(actual, status);
        assert_warning(&warnings, message, Some(category));
    }
}

#[test]
fn crlf_host_values_use_the_authority_parser_because_header_values_reject_them() {
    // Safe HTTP header APIs cannot construct CR/LF values. Exercise the same
    // production authority parser directly, without unsafe header construction.
    for host in [
        "host-sentinel.invalid\r\nInjected: payload-sentinel",
        "host-sentinel.invalid\nInjected: payload-sentinel",
    ] {
        assert!(HeaderValue::from_bytes(host.as_bytes()).is_err());
        let warnings = Warnings::default();
        let parsed =
            tracing::subscriber::with_default(warnings.clone(), || parse_request_host(host));
        assert!(matches!(parsed, Err(HostRequestError::InvalidHeader)));
        assert_warning(
            &warnings.0.lock().unwrap(),
            MALFORMED_HOST,
            Some("malformed"),
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn missing_host_and_uri_authority_fallback_preserve_redaction_and_authentication() {
    let (status, warnings) = observe(request("/mcp?uri-sentinel")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_warning(
        &warnings,
        "rejected request with missing Host header and no :authority",
        None,
    );

    let (status, warnings) =
        observe(request("http://uri-sentinel.invalid:1234/mcp?uri-sentinel")).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_warning(&warnings, DISALLOWED_HOST, Some("not_allowed"));

    let (status, warnings) = observe(request("http://localhost:31415/mcp?uri-sentinel")).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "an allowed authority still requires valid auth"
    );
    assert!(warnings.is_empty(), "{warnings:?}");
}

#[tokio::test(flavor = "current_thread")]
async fn origin_refusals_log_only_the_static_message() {
    for origins in [
        vec![HeaderValue::from_static("https://origin-sentinel.invalid")],
        vec![HeaderValue::from_static("https://[origin-sentinel.invalid")],
        vec![HeaderValue::from_bytes(b"origin-sentinel.invalid-\xff").unwrap()],
        vec![
            HeaderValue::from_static("http://localhost"),
            HeaderValue::from_static("https://origin-sentinel.invalid"),
        ],
    ] {
        let mut request = request("/mcp?uri-sentinel");
        request
            .headers_mut()
            .insert(HOST, HeaderValue::from_static("localhost"));
        for origin in origins {
            request.headers_mut().append(ORIGIN, origin);
        }
        let (status, warnings) = observe(request).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_warning(&warnings, ORIGIN_REFUSAL, None);
    }
}
