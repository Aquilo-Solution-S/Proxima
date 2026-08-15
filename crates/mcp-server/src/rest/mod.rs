//! The `/v1` REST surface: a rendering of the frozen tool manifest.
//!
//! Not a second API. Every route is derived at startup from
//! `FlavorRegistryFrozen`; no route is hand-written per tool; every route
//! terminates in [`McpToolHost::call_tool`] or
//! [`McpToolHost::read_resource`], which is what keeps `ScopeGateBehavior`
//! and the shared edge auth layer the only gates. A tool added to a flavor
//! crate appears here with no edit in this module, and cannot appear here
//! without appearing on MCP.
//!
//! REST grants no authority MCP does not already grant. A token that
//! cannot call `core_publish` over MCP cannot call it over REST, because
//! the gate runs below the seam — not beside it.
//!
//! See `docs/17-rest-surface.md`.

pub mod context;
pub mod error;
pub mod openapi;

use std::collections::BTreeSet;
use std::sync::Arc;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path, RawPathParams, State};
use axum::http::{HeaderValue, Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get};
use proxima_core::CoreResourceMeta;
use proxima_core::mcp::{McpToolDescriptor, McpToolOrigin, all_core_resources, tool_name_matches};

use crate::handler::{
    action_allowed_for_auth, advertised_resource_scope_keys, annotations_for_auth,
    project_dispatcher_actions_for_auth, resource_scope_allows, tool_allowed_for_auth,
};
use crate::rest::context::{RestAuth, author_from_headers, reject_reserved_arguments};
use crate::rest::error::{Problem, problem_for};
use crate::selfdoc;
use crate::server::McpToolHost;

/// Every answer is owner- and token-scoped, so nothing on this surface is
/// shared-cacheable — including `QUERY` reads, whose cacheability is
/// deliberately unused. What `QUERY` buys here is retry safety and correct
/// modelling, not caching.
pub const NO_STORE: &str = "private, no-store";

/// Path prefix of the whole surface. Baked into the route strings rather
/// than applied with `Router::nest`, because `nest` rewrites the inner
/// request URI and every problem document's `instance` would then be
/// missing its prefix.
pub const PREFIX: &str = "/v1";

/// Scheme of the engine's resource URIs. `/v1/resources/{*path}?{query}`
/// reconstructs `proxima://{path}?{query}` verbatim; all parsing,
/// validation, clamping and cursor handling stay in `server.rs`, which
/// already does every bit of it.
const RESOURCE_SCHEME: &str = "proxima://";

#[derive(Clone, Debug)]
struct RestState {
    host: McpToolHost,
    public_url: Option<Arc<str>>,
}

/// Build the `/v1` router.
///
/// Merge it into the listener *inside* the shared auth and body-limit
/// layers: it reads the [`crate::McpAuthContext`] those layers inject and
/// authenticates nothing itself.
#[must_use = "merge the returned router into the MCP HTTP surface"]
pub fn router(host: McpToolHost, public_url: Option<String>) -> Router {
    Router::new()
        .route("/v1/tools", get(list_tools))
        .route("/v1/tools/{tool}", any(tool_route))
        .route("/v1/tools/{tool}/{action}", any(action_route))
        .route("/v1/resources", get(list_resources))
        .route("/v1/resources/{*path}", get(read_resource))
        .route("/v1/how-to", get(how_to))
        .route("/v1/openapi.json", get(openapi))
        .with_state(RestState {
            host,
            public_url: public_url.map(Arc::from),
        })
}

// ---------------------------------------------------------------- catalog

/// Scope-filtered manifest. Same filter `tools/list` applies — literally
/// the same function — so the two surfaces cannot drift by omission.
#[allow(clippy::unused_async, reason = "axum handlers must be futures")]
async fn list_tools(State(state): State<RestState>, RestAuth(auth): RestAuth) -> Response {
    let tools: Vec<serde_json::Value> = state
        .host
        .registry()
        .list_mcp_tools()
        .iter()
        .filter(|descriptor| tool_allowed_for_auth(Some(&auth), descriptor))
        .map(|descriptor| tool_json(descriptor, Some(&auth)))
        .collect();
    json_ok(&serde_json::json!({ "tools": tools }))
}

/// Single descriptor. `404` — not `403` — when the tool is outside the
/// caller's scope: this route answers "what is in your catalog", and a tool
/// that is not in it is absent rather than refused. Invocation is where
/// denial is `403` (see [`dispatch`]), because there the caller has asserted
/// the tool exists and is owed the reason.
fn tool_descriptor(state: &RestState, auth: &crate::McpAuthContext, tool: &str) -> Response {
    match find_descriptor(state, tool).filter(|d| tool_allowed_for_auth(Some(auth), d)) {
        Some(descriptor) => json_ok(&tool_json(descriptor, Some(auth))),
        None => Problem::not_found(format!("tool {tool} not found"), &tool_instance(tool))
            .into_response(),
    }
}

#[allow(clippy::unused_async, reason = "axum handlers must be futures")]
async fn list_resources(RestAuth(auth): RestAuth) -> Response {
    let scope = Some(auth.authz.tool_scope());
    let resources: Vec<serde_json::Value> = all_core_resources()
        .filter(|resource| resource_scope_allows(scope, resource.scope_key))
        .map(resource_json)
        .collect();
    json_ok(&serde_json::json!({ "resources": resources }))
}

/// `proxima://how-to` is the one resource not reachable through the
/// dispatch seam: it is synthesized per request from the caller's advertised
/// surface, so it gets its own route and `/v1/resources/how-to` stays a
/// `404`.
#[allow(clippy::unused_async, reason = "axum handlers must be futures")]
async fn how_to(State(state): State<RestState>, RestAuth(auth): RestAuth) -> Response {
    let advertised_tools = advertised_tool_ids(&state, &auth);
    let advertised_resources = advertised_resource_scope_keys(Some(auth.authz.tool_scope()));
    let body = selfdoc::how_to_markdown(&advertised_tools, &advertised_resources);
    let mut response = body.into_response();
    set_headers(&mut response, "text/markdown; charset=utf-8");
    response
}

/// The document reflects this caller's `ToolScope`, exactly as `tools/list`
/// does, which is why it is token-specific and never shared-cacheable.
#[allow(clippy::unused_async, reason = "axum handlers must be futures")]
async fn openapi(State(state): State<RestState>, RestAuth(auth): RestAuth) -> Response {
    json_ok(&openapi::document_from_registry(
        state.host.registry(),
        state.public_url.as_deref(),
        Some(&auth),
    ))
}

// ------------------------------------------------------------ invocation

/// `/v1/tools/{tool}` for every method.
///
/// `axum` 0.8.9's `MethodFilter` is closed over the nine classic methods and
/// its `TryFrom<Method>` rejects `QUERY`, so generated routes cannot use
/// `on(MethodFilter::…)`. This one catch-all matches on [`Method`] directly
/// and emits `Allow` by hand, since `any()` calls `skip_allow_header()` and
/// suppresses axum's own. Scaffolding with a known removal condition: when a
/// later 0.8.x adds `MethodFilter::QUERY`, the caret pin picks it up and this
/// collapses into ordinary routing.
async fn tool_route(
    State(state): State<RestState>,
    method: Method,
    Path(tool): Path<String>,
    headers: axum::http::HeaderMap,
    RestAuth(auth): RestAuth,
    body: Bytes,
) -> Response {
    let instance = tool_instance(&tool);
    let Some(descriptor) = find_descriptor(&state, &tool) else {
        return Problem::not_found(format!("tool {tool} not found"), &instance).into_response();
    };
    if method == Method::GET {
        return tool_descriptor(&state, &auth, &tool);
    }
    if method != Method::POST && method != Method::QUERY {
        let admits_query = if descriptor.action_arg_specs.is_empty() {
            descriptor.is_read_only()
        } else {
            descriptor.action_arg_specs.iter().any(|spec| {
                descriptor.action_is_read_only(spec.action)
                    && action_allowed_for_auth(Some(&auth), descriptor, spec.action)
            })
        };
        let allow = if admits_query {
            "GET, POST, QUERY"
        } else {
            "GET, POST"
        };
        return gate_method(&method, false, allow, &instance)
            .expect_err("non-invocation method is rejected")
            .into_response();
    }
    if descriptor.action_arg_specs.is_empty() {
        let read_only = descriptor.is_read_only();
        let allow = if read_only {
            "GET, POST, QUERY"
        } else {
            "GET, POST"
        };
        if let Err(problem) = gate_method(&method, read_only, allow, &instance) {
            return problem.into_response();
        }
    }
    let args = match request_arguments(&body, &instance) {
        Ok(args) => args,
        Err(problem) => return problem.into_response(),
    };
    // A whole-dispatcher invocation still names its action in the body, so
    // method safety resolves from that action spec just like the narrowed
    // route. Missing/unknown actions are writes here and are classified by
    // the shared dispatch validator after POST reaches it.
    if !descriptor.action_arg_specs.is_empty() {
        let read_only = args
            .get("action")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|action| descriptor.action_is_read_only(action));
        let allow = if read_only {
            "GET, POST, QUERY"
        } else {
            "GET, POST"
        };
        if let Err(problem) = gate_method(&method, read_only, allow, &instance) {
            return problem.into_response();
        }
    }
    dispatch(&state, &auth, &headers, descriptor.name, args, &instance).await
}

/// `/v1/tools/{tool}/{action}` — the narrowed dispatcher form.
async fn action_route(
    State(state): State<RestState>,
    method: Method,
    Path((tool, action)): Path<(String, String)>,
    headers: axum::http::HeaderMap,
    RestAuth(auth): RestAuth,
    body: Bytes,
) -> Response {
    let instance = format!("{PREFIX}/tools/{tool}/{action}");
    let Some(descriptor) = find_descriptor(&state, &tool) else {
        return Problem::not_found(format!("tool {tool} not found"), &instance).into_response();
    };
    // Unknown action is a route-layer `404`, before dispatch, so it reads as
    // "no such route" rather than as an argument error. Checked against the
    // registry's FULL action set, never the scope-narrowed one: an action the
    // caller may not invoke exists, and hiding it here would turn the gate's
    // `403` into a `404`.
    if !descriptor
        .action_arg_specs
        .iter()
        .any(|spec| spec.action == action)
    {
        return Problem::not_found(format!("tool {tool} has no action {action}"), &instance)
            .into_response();
    }
    let read_only = descriptor.action_is_read_only(&action);
    let allow = if read_only { "POST, QUERY" } else { "POST" };
    if let Err(problem) = gate_method(&method, read_only, allow, &instance) {
        return problem.into_response();
    }
    let args = match request_arguments(&body, &instance) {
        Ok(args) => args,
        Err(problem) => return problem.into_response(),
    };
    let args = match inject_action(args, &action, &instance) {
        Ok(args) => args,
        Err(problem) => return problem.into_response(),
    };
    dispatch(&state, &auth, &headers, descriptor.name, args, &instance).await
}

/// The one place a REST request becomes a tool call. R1: no route reaches
/// `Engine` or storage directly.
async fn dispatch(
    state: &RestState,
    auth: &crate::McpAuthContext,
    headers: &axum::http::HeaderMap,
    tool: &str,
    args: serde_json::Value,
    instance: &str,
) -> Response {
    let author = match author_from_headers(headers, auth, instance) {
        Ok(author) => author,
        Err(problem) => return problem.into_response(),
    };
    match state
        .host
        .call_tool(tool, args, author, Some(auth.clone()))
        .await
    {
        Ok(value) => json_ok(&value),
        Err(err) => problem_for(&err, instance).into_response(),
    }
}

// -------------------------------------------------------------- resources

/// Total and mechanical: the wildcard path and the raw query string are
/// pasted back onto `proxima://` and handed to the seam. REST adds no
/// parser, so a malformed parameter fails in exactly the place — and with
/// exactly the class — an MCP resource read fails.
async fn read_resource(
    State(state): State<RestState>,
    uri: Uri,
    raw_params: RawPathParams,
    headers: axum::http::HeaderMap,
    RestAuth(auth): RestAuth,
) -> Response {
    // The percent-encoded value, not `Path`'s decoded one: "verbatim" means
    // the seam sees the bytes the client sent.
    let path = raw_params
        .iter()
        .find_map(|(name, value)| (name == "path").then_some(value))
        .unwrap_or_default();
    let instance = format!("{PREFIX}/resources/{path}");
    let mut target = format!("{RESOURCE_SCHEME}{path}");
    if let Some(query) = uri.query().filter(|query| !query.is_empty()) {
        target.push('?');
        target.push_str(query);
    }
    let author = match author_from_headers(&headers, &auth, &instance) {
        Ok(author) => author,
        Err(problem) => return problem.into_response(),
    };
    match state.host.read_resource(&target, author, Some(auth)).await {
        Ok(value) => json_ok(&value),
        Err(err) => problem_for(&err, &instance).into_response(),
    }
}

// ---------------------------------------------------------------- helpers

fn find_descriptor<'a>(state: &'a RestState, tool: &str) -> Option<&'a McpToolDescriptor> {
    state
        .host
        .registry()
        .list_mcp_tools()
        .iter()
        .find(|descriptor| tool_name_matches(descriptor.name, tool))
}

/// Read-only tools are reachable by both `QUERY` and `POST`; write tools
/// accept `POST` only.
///
/// `POST` is retained alongside `QUERY` rather than replaced: middleboxes
/// routinely reject unrecognized methods, and a read that is unreachable
/// through a customer proxy is worse than a read with imprecise semantics.
fn gate_method(
    method: &Method,
    read_only: bool,
    allow: &'static str,
    instance: &str,
) -> Result<(), Problem> {
    if *method == Method::POST || (read_only && *method == Method::QUERY) {
        return Ok(());
    }
    Err(Problem::new(
        StatusCode::METHOD_NOT_ALLOWED,
        "method-not-allowed",
        "Method not allowed",
        format!("{method} is not allowed here; allowed: {allow}"),
        instance,
    )
    .with_allow(allow))
}

/// Parse the request body into the tool's arguments object.
fn request_arguments(body: &Bytes, instance: &str) -> Result<serde_json::Value, Problem> {
    let args = if body.is_empty() {
        serde_json::json!({})
    } else {
        serde_json::from_slice::<serde_json::Value>(body).map_err(|err| {
            Problem::invalid_input(format!("request body is not JSON: {err}"), instance)
        })?
    };
    if !args.is_object() {
        return Err(Problem::invalid_input(
            "request body must be a JSON object of the tool's arguments",
            instance,
        ));
    }
    reject_reserved_arguments(&args, instance)?;
    Ok(args)
}

/// Inject the route's action into the body.
///
/// A body that also carries `action` is rejected even when the values agree.
/// Silent agreement invites a client that sets only the body field and
/// breaks when the route changes.
fn inject_action(
    mut args: serde_json::Value,
    action: &str,
    instance: &str,
) -> Result<serde_json::Value, Problem> {
    let Some(object) = args.as_object_mut() else {
        return Err(Problem::invalid_input(
            "request body must be a JSON object of the tool's arguments",
            instance,
        ));
    };
    if object.contains_key("action") {
        return Err(Problem::new(
            StatusCode::BAD_REQUEST,
            "action-conflict",
            "Action in request body",
            "`action` is carried by the route on this surface; remove it from the request body",
            instance,
        ));
    }
    object.insert("action".to_string(), serde_json::json!(action));
    Ok(args)
}

fn advertised_tool_ids(state: &RestState, auth: &crate::McpAuthContext) -> BTreeSet<&'static str> {
    state
        .host
        .registry()
        .list_mcp_tools()
        .iter()
        .filter(|descriptor| tool_allowed_for_auth(Some(auth), descriptor))
        .map(|descriptor| descriptor.name)
        .collect()
}

fn tool_json(
    descriptor: &McpToolDescriptor,
    auth: Option<&crate::McpAuthContext>,
) -> serde_json::Value {
    serde_json::json!({
        "id": descriptor.name,
        "description": descriptor.description,
        "origin": match &descriptor.origin {
            McpToolOrigin::Substrate => serde_json::json!("substrate"),
            McpToolOrigin::Flavor(flavor) => serde_json::json!({ "flavor": flavor }),
        },
        "annotations": annotations_for_auth(auth, descriptor),
        // Narrowed per action by the same projection `tools/list` applies, so
        // a palette that permits one leaf of a dispatcher advertises one leaf
        // on both surfaces (R6).
        "args_schema": project_dispatcher_actions_for_auth(descriptor, auth),
    })
}

fn resource_json(meta: &CoreResourceMeta) -> serde_json::Value {
    serde_json::json!({
        "name": meta.name,
        "title": meta.title,
        "description": meta.description,
        "uri_template": meta.uri_template,
        "scope_key": meta.scope_key,
        "is_template": meta.is_template,
        "path": meta
            .uri_template
            .strip_prefix(RESOURCE_SCHEME)
            .map(|rest| format!("{PREFIX}/resources/{rest}")),
    })
}

fn tool_instance(tool: &str) -> String {
    format!("{PREFIX}/tools/{tool}")
}

fn json_ok(value: &serde_json::Value) -> Response {
    let mut response = value.to_string().into_response();
    set_headers(&mut response, "application/json");
    response
}

fn set_headers(response: &mut Response, content_type: &'static str) {
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(NO_STORE));
}

#[cfg(test)]
mod tests {
    use super::*;
    use proxima_core::protocol::tool as protocol_tool;

    #[test]
    fn method_gate_admits_query_only_for_reads() {
        assert!(gate_method(&Method::POST, false, "GET, POST", "/v1/tools/x").is_ok());
        assert!(gate_method(&Method::POST, true, "GET, POST, QUERY", "/v1/tools/x").is_ok());
        assert!(gate_method(&Method::QUERY, true, "GET, POST, QUERY", "/v1/tools/x").is_ok());

        let denied = gate_method(&Method::QUERY, false, "POST", "/v1/tools/x/y")
            .expect_err("QUERY on a write must be refused");
        let json = denied.to_json();
        assert_eq!(json["status"], 405);
        assert_eq!(
            json["type"],
            "https://proxima.dev/errors/method-not-allowed"
        );

        assert!(gate_method(&Method::DELETE, true, "GET, POST, QUERY", "/v1/tools/x").is_err());
    }

    #[test]
    fn action_injection_refuses_a_body_that_agrees() {
        let injected = inject_action(
            serde_json::json!({ "title": "t" }),
            "set",
            "/v1/tools/core_goal/set",
        )
        .expect("clean body");
        assert_eq!(injected["action"], "set");

        for body in [
            serde_json::json!({ "action": "set" }),
            serde_json::json!({ "action": "transition" }),
        ] {
            let err = inject_action(body, "set", "/v1/tools/core_goal/set")
                .expect_err("a body action must be refused, agreeing or not");
            assert_eq!(err.to_json()["status"], 400);
            assert_eq!(
                err.to_json()["type"],
                "https://proxima.dev/errors/action-conflict"
            );
        }
    }

    #[test]
    fn an_empty_body_is_an_empty_argument_object() {
        let args = request_arguments(&Bytes::new(), "/v1/tools/core_memory_spaces")
            .expect("empty body is legal");
        assert_eq!(args, serde_json::json!({}));

        assert!(request_arguments(&Bytes::from_static(b"[1,2]"), "/v1/tools/x").is_err());
        assert!(request_arguments(&Bytes::from_static(b"{oops"), "/v1/tools/x").is_err());
    }

    /// Read/write for an action route is resolved per action, never from the
    /// parent tool. `core_membership` and `core_upload` are write-annotated
    /// parents with a read-only leaf each; `core_fact` is the mirror case, a
    /// read-only parent whose leaves must be re-checked rather than assumed.
    #[test]
    fn action_read_only_is_resolved_per_action_not_per_tool() {
        let registry = proxima_core::FlavorRegistry::new().freeze_or_panic_for_tests();
        let descriptor = |name: &str| {
            registry
                .list_mcp_tools()
                .iter()
                .find(|descriptor| descriptor.name == name)
                .unwrap_or_else(|| panic!("{name} is registered"))
        };

        let membership = descriptor(protocol_tool::CORE_MEMBERSHIP);
        assert!(!membership.is_read_only(), "parent is a write tool");
        assert!(
            membership.action_is_read_only("list_members"),
            "a read-only leaf must keep QUERY under a write parent"
        );
        assert!(!membership.action_is_read_only("add_member"));
        assert!(!membership.action_is_read_only("remove_member"));

        let upload = descriptor(protocol_tool::CORE_UPLOAD);
        assert!(!upload.is_read_only(), "parent is a write tool");
        assert!(upload.action_is_read_only("read_url"));
        assert!(!upload.action_is_read_only("prepare"));

        // The direction that matters for safety: every leaf of a read-only
        // parent is checked on its own, so adding a write action here cannot
        // inherit `QUERY` from the tool annotation.
        let fact = descriptor(protocol_tool::CORE_FACT);
        assert!(fact.is_read_only());
        for spec in fact.action_arg_specs {
            assert_eq!(
                fact.action_is_read_only(spec.action),
                spec.annotations
                    .and_then(|annotations| annotations.read_only)
                    .unwrap_or(false),
                "{} must answer from its own descriptor spec",
                spec.action
            );
        }
    }

    /// Every resource in the catalog maps onto a `/v1/resources/…` path;
    /// the mapping is total, so a `None` here would be a resource REST
    /// cannot reach.
    #[test]
    fn every_core_resource_has_a_rest_path() {
        for meta in all_core_resources() {
            let json = resource_json(meta);
            let path = json["path"]
                .as_str()
                .unwrap_or_else(|| panic!("{} has no REST path: {}", meta.name, meta.uri_template));
            assert!(path.starts_with("/v1/resources/"), "{path}");
        }
    }
}
