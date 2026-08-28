//! The `/v1` surface's obligations from docs/17 §Contract Tests.
//!
//! These are what make "REST is a rendering of the manifest, not a second
//! API" checkable rather than aspirational. Everything here runs against a
//! registry-only [`McpToolHost`] with no engine: the claims under test are
//! about surface derivation, gating, and error class — none of which is
//! allowed to depend on storage, and all of which would be untestable if it
//! did.
#![cfg(feature = "rest")]

use std::collections::BTreeSet;
use std::sync::Arc;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::http::{HeaderName, HeaderValue, Method, Request, StatusCode, header};
use proxima_core::FlavorServices;
use proxima_core::mcp::McpAuthorContext;
use proxima_core::protocol::{
    action as protocol_action, resource as protocol_resource, tool as protocol_tool,
};
use proxima_core::{AuthPath, AuthzContext, FlavorRegistry, Owner, OwnerRef, ToolScope, UserId};
use proxima_core::{GroupId, access::Role};
use proxima_mcp_server::{McpAuthContext, McpToolHost};
use tower::ServiceExt;

fn host() -> McpToolHost {
    McpToolHost::from_parts(
        Arc::new(FlavorRegistry::new().freeze_or_panic_for_tests()),
        FlavorServices::default(),
    )
}

const FLAVOR_DISPATCH: &str = "proxima-stub_dispatch";
const FLAVOR_ARGV: &str = "proxima-stub_cli";
const CALLER_CONTEXT_TOOL: &str = "proxima-stub_caller_context";

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct CallerContextArgs {}

#[derive(Debug, PartialEq, Eq, serde::Serialize, schemars::JsonSchema)]
struct CallerContextOutput {
    model_id: String,
    client_name: String,
    client_version: String,
    caller_self_perspective: Option<String>,
}

struct CallerContextTool;

impl proxima_core::Tool for CallerContextTool {
    const NAME: &'static str = CALLER_CONTEXT_TOOL;
    const DESCRIPTION: &'static str = "Echo transport-neutral caller context.";
    const ANNOTATIONS: Option<proxima_core::mcp::McpToolAnnotations> = Some(
        proxima_core::mcp::McpToolAnnotations::new()
            .read_only(true)
            .open_world(false),
    );

    type Args = CallerContextArgs;
    type Output = CallerContextOutput;

    fn call(
        ctx: proxima_core::ToolCtx,
        _args: Self::Args,
    ) -> futures_util::future::BoxFuture<'static, Result<Self::Output, proxima_core::ToolError>>
    {
        Box::pin(async move {
            let caller = ctx
                .caller()
                .ok_or_else(|| proxima_core::ToolError::Other("caller metadata missing".into()))?;
            Ok(CallerContextOutput {
                model_id: caller.model_id.clone(),
                client_name: caller.client_name.clone(),
                client_version: caller.client_version.clone(),
                caller_self_perspective: ctx
                    .caller_self_perspective()
                    .map(|id| id.into_inner().to_string()),
            })
        })
    }
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case")]
#[expect(
    dead_code,
    reason = "the derived schema is the subject, not the values"
)]
enum StubDispatchArgs {
    /// Inspect one thing without changing it.
    Look {
        #[schemars(description = "Which thing to look at.")]
        id: String,
    },
    /// Change one thing.
    Touch {
        #[schemars(description = "Which thing to touch.")]
        id: String,
    },
}

/// A flavor dispatcher: an internally tagged `Args` plus the specs that
/// enumerate its actions. Before both registration paths filled the specs,
/// a tool shaped like this was advertised as a dispatcher and served no
/// action route at all.
#[derive(Debug)]
struct StubDispatchTool;

impl proxima_core::mcp::McpTool for StubDispatchTool {
    const NAME: &'static str = FLAVOR_DISPATCH;
    const DESCRIPTION: &'static str = "A flavor dispatcher.";
    const ANNOTATIONS: Option<proxima_core::mcp::McpToolAnnotations> = Some(
        proxima_core::mcp::McpToolAnnotations::new()
            .read_only(true)
            .open_world(false),
    );
    const ACTION_ARG_SPECS: &'static [proxima_core::mcp::McpActionArgSpec] = &[
        proxima_core::mcp::McpActionArgSpec {
            action: "look",
            allowed_fields: &["id"],
            required_fields: &["id"],
            annotations: Some(
                proxima_core::mcp::McpToolAnnotations::new()
                    .read_only(true)
                    .open_world(false),
            ),
            audience: proxima_core::mcp::McpToolAudience::Shared,
        },
        proxima_core::mcp::McpActionArgSpec {
            action: "touch",
            allowed_fields: &["id"],
            required_fields: &["id"],
            annotations: Some(
                proxima_core::mcp::McpToolAnnotations::new()
                    .read_only(false)
                    .open_world(false),
            ),
            audience: proxima_core::mcp::McpToolAudience::Shared,
        },
    ];
    type Args = StubDispatchArgs;
    type Output = ();

    fn call(
        _ctx: proxima_core::mcp::McpToolCtx,
        _args: Self::Args,
    ) -> futures_util::future::BoxFuture<'static, Result<(), proxima_core::mcp::McpToolError>> {
        Box::pin(async { Ok(()) })
    }
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct StubArgvArgs {
    #[schemars(description = "Command words followed by flags.")]
    argv: Vec<String>,
}

/// An argv-keyed dispatcher in the shape the per-action annotation exists
/// for: the tool must declare itself writable because `approval decide`
/// writes, and `approval` states for itself that it only reads. The write
/// command declares nothing, so it classifies from the tool.
#[derive(Debug)]
struct StubArgvTool;

impl proxima_core::mcp::McpTool for StubArgvTool {
    const NAME: &'static str = FLAVOR_ARGV;
    const DESCRIPTION: &'static str = "An argv-keyed flavor dispatcher.";
    const ANNOTATIONS: Option<proxima_core::mcp::McpToolAnnotations> = Some(
        proxima_core::mcp::McpToolAnnotations::new()
            .read_only(false)
            .open_world(false),
    );
    const ARGV_ACTION_SPECS: &'static [proxima_core::mcp::McpArgvActionSpec] = &[
        proxima_core::mcp::McpArgvActionSpec {
            action: "approval",
            argv_prefix: &["approval"],
            annotations: Some(
                proxima_core::mcp::McpToolAnnotations::new()
                    .read_only(true)
                    .open_world(false),
            ),
            audience: proxima_core::mcp::McpToolAudience::Shared,
        },
        proxima_core::mcp::McpArgvActionSpec {
            action: "approval-decide",
            argv_prefix: &["approval", "decide"],
            annotations: None,
            audience: proxima_core::mcp::McpToolAudience::Shared,
        },
    ];
    type Args = StubArgvArgs;
    type Output = Vec<String>;

    fn call(
        _ctx: proxima_core::mcp::McpToolCtx,
        args: Self::Args,
    ) -> futures_util::future::BoxFuture<
        'static,
        Result<Vec<String>, proxima_core::mcp::McpToolError>,
    > {
        Box::pin(async move { Ok(args.argv) })
    }
}

/// The same registry-only host, with one flavor dispatcher added.
fn flavor_host() -> McpToolHost {
    let mut registry = FlavorRegistry::new();
    registry.add_mcp_tool_or_panic_for_tests::<StubDispatchTool>("proxima-stub");
    McpToolHost::from_parts(
        Arc::new(registry.freeze_or_panic_for_tests()),
        FlavorServices::default(),
    )
}

fn argv_host() -> McpToolHost {
    let mut registry = FlavorRegistry::new();
    registry.add_mcp_tool_or_panic_for_tests::<StubArgvTool>("proxima-stub");
    McpToolHost::from_parts(
        Arc::new(registry.freeze_or_panic_for_tests()),
        FlavorServices::default(),
    )
}

fn caller_context_host() -> McpToolHost {
    let mut registry = FlavorRegistry::new();
    registry.add_tool_or_panic_for_tests::<CallerContextTool>("proxima-stub");
    McpToolHost::from_parts(
        Arc::new(registry.freeze_or_panic_for_tests()),
        FlavorServices::default(),
    )
}

/// A principal with full owner rights, so the owner-role gate never
/// confounds a test about tool scope.
fn auth(scope: ToolScope) -> McpAuthContext {
    let owner: Owner = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));
    McpAuthContext {
        owner,
        authz: AuthzContext::single_owner(&owner, AuthPath::HostBearer).with_tool_scope(scope),
        model_id: None,
    }
}

fn viewer_auth(scope: ToolScope) -> McpAuthContext {
    let owner = OwnerRef::Group(GroupId::new(uuid::Uuid::now_v7()));
    McpAuthContext {
        owner,
        authz: AuthzContext::for_subject_with_role(
            UserId::new(uuid::Uuid::now_v7()),
            [(owner, Role::viewer())],
            AuthPath::HostBearer,
        )
        .with_tool_scope(scope),
        model_id: None,
    }
}

fn author() -> McpAuthorContext {
    McpAuthorContext {
        model_id: "test-model".into(),
        client_name: "test".into(),
        client_version: "0".into(),
        caller_self_perspective: None,
    }
}

fn app(host: McpToolHost) -> Router {
    proxima_mcp_server::rest::router(host, None)
}

struct Answer {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    body: Bytes,
}

impl Answer {
    fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).unwrap_or_else(|err| {
            panic!(
                "body is not JSON ({err}): {}",
                String::from_utf8_lossy(&self.body)
            )
        })
    }

    fn header(&self, name: header::HeaderName) -> Option<&str> {
        self.headers.get(name).and_then(|value| value.to_str().ok())
    }
}

/// Send one request with the auth context the shared `mcp_auth` layer would
/// have injected. Nothing here authenticates — that layer is not under test,
/// and re-implementing authentication here is forbidden.
async fn call(
    router: &Router,
    method: Method,
    uri: &str,
    ctx: &McpAuthContext,
    body: Option<serde_json::Value>,
) -> Answer {
    call_with_headers(router, method, uri, ctx, body, &[]).await
}

async fn call_with_headers(
    router: &Router,
    method: Method,
    uri: &str,
    ctx: &McpAuthContext,
    body: Option<serde_json::Value>,
    headers: &[(&str, &str)],
) -> Answer {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(body.map_or_else(Body::empty, |value| Body::from(value.to_string())))
        .expect("request builds");
    for &(name, value) in headers {
        request.headers_mut().insert(
            HeaderName::from_bytes(name.as_bytes()).expect("header name is valid"),
            HeaderValue::from_str(value).expect("header value is valid"),
        );
    }
    request.extensions_mut().insert(ctx.clone());
    let response = router
        .clone()
        .oneshot(request)
        .await
        .expect("router is infallible");
    let status = response.status();
    let headers = response.headers().clone();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body collects");
    Answer {
        status,
        headers,
        body,
    }
}

#[tokio::test]
async fn rest_projects_headers_into_generic_tool_caller_context() {
    let router = app(caller_context_host());
    let ctx = auth(ToolScope::All);
    let caller_self_perspective = uuid::Uuid::now_v7();
    let self_header = format!("P:{caller_self_perspective}");

    let answer = call_with_headers(
        &router,
        Method::POST,
        &format!("/v1/tools/{CALLER_CONTEXT_TOOL}"),
        &ctx,
        Some(serde_json::json!({})),
        &[
            ("X-Proxima-Model-Id", "planner/model"),
            (
                header::USER_AGENT.as_str(),
                "planner-client/2.4.1 middleware/9",
            ),
            ("X-Proxima-Self-Perspective", &self_header),
        ],
    )
    .await;

    assert_eq!(answer.status, StatusCode::OK);
    assert_eq!(
        answer.json(),
        serde_json::json!({
            "model_id": "planner/model",
            "client_name": "planner-client",
            "client_version": "2.4.1",
            "caller_self_perspective": caller_self_perspective.to_string(),
        })
    );
}

async fn get(router: &Router, uri: &str, ctx: &McpAuthContext) -> Answer {
    call(router, Method::GET, uri, ctx, None).await
}

// --------------------------------------------------------- surface parity

/// The REST tool list equals the MCP tool catalog id for id, for whatever
/// `ToolScope` the caller holds.
///
/// `proxima://tools` is the catalog authority for a running server, and it
/// is reached here through the same [`McpToolHost::read_resource`] seam an
/// MCP client uses — so this compares two surfaces, not one surface against
/// a restatement of its own filter.
#[tokio::test]
async fn rest_tool_list_equals_the_mcp_catalog_for_every_scope() {
    let host = host();
    let router = app(host.clone());

    for scope in [
        ToolScope::All,
        // `resource:tools` is in the palette because the catalog resource is
        // itself scope-gated; without it the MCP side would be refused and
        // the comparison would be against nothing.
        ToolScope::Palette(vec![
            protocol_resource::TOOLS.to_string(),
            protocol_tool::CORE_SEARCH_MEMORIES.to_string(),
            protocol_action::CORE_GOAL_SET.to_string(),
        ]),
    ] {
        let ctx = auth(scope.clone());
        let expected: BTreeSet<String> = host
            .read_resource("proxima://tools", author(), Some(ctx.clone()))
            .await
            .expect("the MCP catalog is readable under this scope")["tools"]
            .as_array()
            .expect("tools array")
            .iter()
            .map(|tool| tool["tool_id"].as_str().expect("tool_id").to_string())
            .collect();
        assert!(!expected.is_empty(), "nothing to compare for {scope:?}");

        let answer = get(&router, "/v1/tools", &ctx).await;
        assert_eq!(answer.status, StatusCode::OK);
        let actual: BTreeSet<String> = answer.json()["tools"]
            .as_array()
            .expect("tools array")
            .iter()
            .map(|tool| tool["id"].as_str().expect("id").to_string())
            .collect();
        assert_eq!(actual, expected, "surface drift for {scope:?}");
    }

    // The fail-closed end of the range: an empty palette advertises nothing
    // on either surface, rather than REST falling open to the full registry.
    let empty = auth(ToolScope::Palette(Vec::new()));
    assert!(
        host.read_resource("proxima://tools", author(), Some(empty.clone()))
            .await
            .is_err(),
        "an empty palette denies the MCP catalog"
    );
    let answer = get(&router, "/v1/tools", &empty).await;
    assert_eq!(answer.json()["tools"].as_array().map(Vec::len), Some(0));
}

/// The narrowing reaches per-action too: a palette holding one leaf of a
/// dispatcher advertises one leaf, not the whole flattened schema.
#[tokio::test]
async fn a_palette_narrows_the_advertised_dispatcher_actions() {
    let router = app(host());
    let ctx = auth(ToolScope::Palette(vec![
        protocol_action::CORE_GOAL_SET.to_string(),
    ]));

    let answer = get(&router, "/v1/tools", &ctx).await;
    let goal = answer.json()["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .find(|tool| tool["id"] == protocol_tool::CORE_GOAL)
        .cloned()
        .expect("core_goal is advertised");
    let actions: Vec<String> = goal["args_schema"]["x-proxima-actions"]
        .as_object()
        .expect("x-proxima-actions")
        .keys()
        .cloned()
        .collect();
    assert_eq!(actions, vec!["set".to_string()]);
}

/// A `tool:action` palette entry admits a dispatcher leaf only. A flat tool
/// has no leaves, so a made-up suffix must agree with the invocation gate and
/// disappear from every derived catalog surface.
#[tokio::test]
async fn a_bogus_leaf_never_advertises_a_flat_tool() {
    let router = app(host());
    let flat = protocol_tool::CORE_SEARCH_MEMORIES;
    let ctx = auth(ToolScope::Palette(vec![format!("{flat}:bogus")]));

    let list = get(&router, "/v1/tools", &ctx).await;
    assert_eq!(list.status, StatusCode::OK);
    assert!(
        list.json()["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .all(|tool| tool["id"] != flat),
        "the REST catalog must not advertise the denied flat tool",
    );

    let catalog = get(&router, &format!("/v1/tools/{flat}"), &ctx).await;
    assert_eq!(catalog.status, StatusCode::NOT_FOUND);

    let openapi = get(&router, "/v1/openapi.json", &ctx).await.json();
    assert!(
        openapi["paths"].get(format!("/v1/tools/{flat}")).is_none(),
        "OpenAPI must not advertise the denied flat tool",
    );

    let invoked = call(
        &router,
        Method::POST,
        &format!("/v1/tools/{flat}"),
        &ctx,
        Some(serde_json::json!({})),
    )
    .await;
    assert_eq!(invoked.status, StatusCode::FORBIDDEN);
}

// ------------------------------------------------------------ gate parity

/// A tool denied over MCP is denied over REST — `403`, from
/// `ScopeGateBehavior` below the seam, not a `404` that would make the
/// refusal indistinguishable from absence.
///
/// The catalog read is the deliberate exception: `GET /v1/tools/{tool}`
/// answers "what is in your catalog", and a tool that is not in it is
/// absent.
#[tokio::test]
async fn a_denied_tool_is_403_on_invocation_and_404_in_the_catalog() {
    let router = app(host());
    let ctx = auth(ToolScope::Palette(vec![
        protocol_tool::CORE_SEARCH_MEMORIES.to_string(),
    ]));

    let invoked = call(
        &router,
        Method::POST,
        "/v1/tools/core_remember",
        &ctx,
        Some(serde_json::json!({ "text": "x" })),
    )
    .await;
    assert_eq!(invoked.status, StatusCode::FORBIDDEN);
    assert_eq!(
        invoked.header(header::CONTENT_TYPE),
        Some("application/problem+json")
    );
    let problem = invoked.json();
    assert_eq!(problem["type"], "https://proxima.dev/errors/not-authorized");
    assert_eq!(problem["instance"], "/v1/tools/core_remember");
    assert!(
        problem["detail"]
            .as_str()
            .expect("detail")
            .contains("not authorized"),
        "{problem}"
    );

    let catalog = get(&router, "/v1/tools/core_remember", &ctx).await;
    assert_eq!(catalog.status, StatusCode::NOT_FOUND);

    let visible = get(&router, "/v1/tools/core_search_memories", &ctx).await;
    assert_eq!(visible.status, StatusCode::OK);
    assert_eq!(
        visible.json()["id"],
        protocol_tool::CORE_SEARCH_MEMORIES,
        "an in-palette tool is readable"
    );
}

/// No bound auth context means the request never passed the shared auth
/// layer. Fail closed rather than dispatch.
#[tokio::test]
async fn a_request_without_a_bound_auth_context_is_401() {
    let router = app(host());
    let response = router
        .oneshot(
            Request::builder()
                .uri("/v1/tools")
                .body(Body::empty())
                .expect("request builds"),
        )
        .await
        .expect("infallible");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

// --------------------------------------------------------- reserved fields

/// Each reserved name is refused, and the refusal carries the exact fix.
/// The cost is one failed request during integration; the alternative is a
/// corpus of Facts silently attributed to `unknown` in an append-only store.
#[tokio::test]
async fn every_reserved_argument_is_400_naming_its_header() {
    let router = app(host());
    let ctx = auth(ToolScope::All);

    for (field, expected_header) in [
        ("model_id", "X-Proxima-Model-Id"),
        ("caller_self_perspective", "X-Proxima-Self-Perspective"),
        (
            "_proxima_caller_self_perspective",
            "X-Proxima-Self-Perspective",
        ),
        (
            "current_root_perspective_memory_id",
            "X-Proxima-Self-Perspective",
        ),
    ] {
        let answer = call(
            &router,
            Method::POST,
            "/v1/tools/core_remember",
            &ctx,
            Some(serde_json::json!({ "text": "hello", field: "value" })),
        )
        .await;
        assert_eq!(answer.status, StatusCode::BAD_REQUEST, "{field}");
        let problem = answer.json();
        assert_eq!(
            problem["type"],
            "https://proxima.dev/errors/reserved-argument"
        );
        let detail = problem["detail"].as_str().expect("detail");
        assert!(detail.contains(field), "{detail}");
        assert!(detail.contains(expected_header), "{detail}");
    }
}

// -------------------------------------------------------- action injection

#[tokio::test]
async fn a_conflicting_body_action_is_400_and_an_unknown_action_is_404() {
    let router = app(host());
    let ctx = auth(ToolScope::All);

    // Rejected even when the values agree: silent agreement invites a client
    // that sets only the body field and breaks when the route changes.
    for body_action in ["set", "transition"] {
        let answer = call(
            &router,
            Method::POST,
            "/v1/tools/core_goal/set",
            &ctx,
            Some(serde_json::json!({ "action": body_action, "title": "t" })),
        )
        .await;
        assert_eq!(answer.status, StatusCode::BAD_REQUEST, "{body_action}");
        assert_eq!(
            answer.json()["type"],
            "https://proxima.dev/errors/action-conflict"
        );
    }

    // Refused at the route layer, before dispatch, so it reads as "no such
    // route" rather than as an argument error.
    let unknown = call(
        &router,
        Method::POST,
        "/v1/tools/core_goal/no_such_action",
        &ctx,
        Some(serde_json::json!({})),
    )
    .await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);
    assert_eq!(
        unknown.json()["instance"],
        "/v1/tools/core_goal/no_such_action"
    );
}

// ------------------------------------------------- flavor dispatcher routes

/// A flavor dispatcher's action routes are served, not 404'd.
///
/// The router enumerates actions from `McpToolDescriptor::action_arg_specs`.
/// `try_add_tool` — the path `proxima_flavor!` uses — hardcoded an empty
/// slice, so this exact request was a `404` while the same tool's flattened
/// schema advertised the action to every client.
#[tokio::test]
async fn a_flavor_dispatcher_serves_its_action_routes() {
    let router = app(flavor_host());
    let ctx = auth(ToolScope::All);

    let answer = call(
        &router,
        Method::POST,
        &format!("/v1/tools/{FLAVOR_DISPATCH}/look"),
        &ctx,
        Some(serde_json::json!({ "id": "x" })),
    )
    .await;
    assert_eq!(
        answer.status,
        StatusCode::OK,
        "body: {}",
        String::from_utf8_lossy(&answer.body)
    );
}

#[tokio::test]
async fn an_unknown_flavor_dispatcher_action_is_404() {
    let router = app(flavor_host());
    let ctx = auth(ToolScope::All);

    let answer = call(
        &router,
        Method::POST,
        &format!("/v1/tools/{FLAVOR_DISPATCH}/vanish"),
        &ctx,
        Some(serde_json::json!({})),
    )
    .await;
    assert_eq!(answer.status, StatusCode::NOT_FOUND);
    assert_eq!(
        answer.json()["instance"],
        format!("/v1/tools/{FLAVOR_DISPATCH}/vanish")
    );
}

#[tokio::test]
async fn a_conflicting_body_action_on_a_flavor_route_is_400() {
    let router = app(flavor_host());
    let ctx = auth(ToolScope::All);

    for body_action in ["look", "touch"] {
        let answer = call(
            &router,
            Method::POST,
            &format!("/v1/tools/{FLAVOR_DISPATCH}/look"),
            &ctx,
            Some(serde_json::json!({ "action": body_action, "id": "x" })),
        )
        .await;
        assert_eq!(answer.status, StatusCode::BAD_REQUEST, "{body_action}");
        assert_eq!(
            answer.json()["type"],
            "https://proxima.dev/errors/action-conflict"
        );
    }
}

#[tokio::test]
async fn the_openapi_document_advertises_a_flavor_dispatchers_actions() {
    let host = flavor_host();
    let router = app(host.clone());
    let answer = get(&router, "/v1/openapi.json", &auth(ToolScope::All)).await;
    assert_eq!(answer.status, StatusCode::OK);

    let document = answer.json();
    let path = format!("/v1/tools/{FLAVOR_DISPATCH}/look");
    let item = document["paths"]
        .as_object()
        .expect("paths object")
        .get(&path)
        .unwrap_or_else(|| panic!("{path} is advertised: {document:#}"));
    let schema = item
        .pointer("/post/requestBody/content/application~1json/schema")
        .expect("the narrowed operation carries a request schema");
    assert!(
        schema.pointer("/properties/id").is_some(),
        "the narrowed schema keeps the action's own fields: {schema:#}",
    );
    assert!(
        schema.pointer("/properties/action").is_none(),
        "`action` is carried by the route, not the body: {schema:#}",
    );
    let catalog = host
        .read_resource("proxima://tools", author(), Some(auth(ToolScope::All)))
        .await
        .expect("tool catalog");
    let catalog_schema = catalog["tools"]
        .as_array()
        .expect("catalog tools")
        .iter()
        .find(|tool| tool["tool_id"] == FLAVOR_DISPATCH)
        .and_then(|tool| tool["actions"].as_array())
        .and_then(|actions| actions.iter().find(|action| action["action"] == "look"))
        .map(|action| &action["argument_schema"])
        .expect("catalog action schema");
    assert_eq!(
        schema, catalog_schema,
        "REST action schema equals catalog metadata"
    );
    assert_eq!(
        item.pointer("/post/description")
            .and_then(serde_json::Value::as_str),
        Some("Inspect one thing without changing it."),
        "the action operation uses enum-variant prose: {item:#}",
    );
}

/// The same narrowing for a flavor: a palette holding one leaf advertises
/// one leaf.
///
/// The projection narrows from the descriptor's own actions. Were it keyed
/// on the substrate tables instead, a flavor dispatcher would give it
/// nothing to narrow with and the whole flattened schema would be
/// advertised — the assertion below is what proves it is not.
#[tokio::test]
async fn a_palette_narrows_a_flavor_dispatchers_advertised_actions() {
    let router = app(flavor_host());
    let ctx = auth(ToolScope::Palette(vec![format!("{FLAVOR_DISPATCH}:look")]));

    let answer = get(&router, "/v1/tools", &ctx).await;
    let dispatch = answer.json()["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .find(|tool| tool["id"] == FLAVOR_DISPATCH)
        .cloned()
        .unwrap_or_else(|| panic!("{FLAVOR_DISPATCH} is advertised: {}", answer.json()));
    let actions: Vec<String> = dispatch["args_schema"]["x-proxima-actions"]
        .as_object()
        .expect("x-proxima-actions")
        .keys()
        .cloned()
        .collect();
    assert_eq!(actions, vec!["look".to_string()]);
}

/// And the gate refuses the leaf the palette does not carry — `403` from
/// `ScopeGateBehavior` below the seam, naming the leaf rather than the tool.
#[tokio::test]
async fn a_denied_flavor_dispatcher_action_is_403() {
    let router = app(flavor_host());
    let ctx = auth(ToolScope::Palette(vec![format!("{FLAVOR_DISPATCH}:look")]));

    let answer = call(
        &router,
        Method::POST,
        &format!("/v1/tools/{FLAVOR_DISPATCH}/touch"),
        &ctx,
        Some(serde_json::json!({ "id": "x" })),
    )
    .await;
    assert_eq!(answer.status, StatusCode::FORBIDDEN);
    assert_eq!(
        answer.header(header::CONTENT_TYPE),
        Some("application/problem+json")
    );
    let detail = answer.json()["detail"]
        .as_str()
        .expect("detail")
        .to_string();
    assert!(
        detail.contains(&format!("{FLAVOR_DISPATCH}:touch")),
        "the denial names the leaf: {detail}",
    );

    let granted = call(
        &router,
        Method::POST,
        &format!("/v1/tools/{FLAVOR_DISPATCH}/look"),
        &ctx,
        Some(serde_json::json!({ "id": "x" })),
    )
    .await;
    assert_eq!(granted.status, StatusCode::OK);
}

// ---------------------------------------------------------- method gating

#[tokio::test]
async fn unsupported_whole_dispatcher_method_is_405_before_body_parsing() {
    let router = app(flavor_host());
    let ctx = auth(ToolScope::All);
    let mut request = Request::builder()
        .method(Method::DELETE)
        .uri(format!("/v1/tools/{FLAVOR_DISPATCH}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from("{not-json"))
        .expect("request builds");
    request.extensions_mut().insert(ctx);

    let response = router.oneshot(request).await.expect("router is infallible");

    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(
        response
            .headers()
            .get(header::ALLOW)
            .and_then(|v| v.to_str().ok()),
        Some("GET, POST, QUERY"),
    );
}

#[tokio::test]
async fn query_on_a_write_tool_is_405_with_allow() {
    let router = app(host());
    let ctx = auth(ToolScope::All);

    let answer = call(
        &router,
        Method::QUERY,
        "/v1/tools/core_remember",
        &ctx,
        Some(serde_json::json!({ "text": "x" })),
    )
    .await;
    assert_eq!(answer.status, StatusCode::METHOD_NOT_ALLOWED);
    let allow = answer
        .header(header::ALLOW)
        .expect("Allow is emitted by hand");
    assert!(allow.contains("POST"), "{allow}");
    assert!(
        !allow.contains("QUERY"),
        "a write tool must not advertise QUERY: {allow}"
    );
}

/// The dispatcher-action routes carry no `GET`, so `Allow` is exactly the
/// invocation methods.
#[tokio::test]
async fn query_on_a_write_action_is_405_with_allow_post() {
    let router = app(host());
    let ctx = auth(ToolScope::All);

    let answer = call(
        &router,
        Method::QUERY,
        "/v1/tools/core_upload/prepare",
        &ctx,
        Some(serde_json::json!({})),
    )
    .await;
    assert_eq!(answer.status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(answer.header(header::ALLOW), Some("POST"));
}

/// Read/write is resolved per action, never inherited from the parent tool.
/// `core_upload` and `core_membership` are write-annotated dispatchers with
/// one read-only leaf each; gating on the tool would strip `QUERY` from a
/// genuine read, and — the direction that actually bites — would hand
/// `QUERY` to a write the day one is added under a read-only parent.
#[tokio::test]
async fn a_read_only_action_under_a_write_parent_keeps_query() {
    let router = app(host());
    let ctx = auth(ToolScope::All);

    for (uri, sibling) in [
        (
            "/v1/tools/core_upload/read_url",
            "/v1/tools/core_upload/abort",
        ),
        (
            "/v1/tools/core_membership/list_members",
            "/v1/tools/core_membership/add_member",
        ),
    ] {
        let read = call(
            &router,
            Method::QUERY,
            uri,
            &ctx,
            Some(serde_json::json!({})),
        )
        .await;
        assert_ne!(
            read.status,
            StatusCode::METHOD_NOT_ALLOWED,
            "{uri} is read-only and must accept QUERY"
        );

        let write = call(
            &router,
            Method::QUERY,
            sibling,
            &ctx,
            Some(serde_json::json!({})),
        )
        .await;
        assert_eq!(
            write.status,
            StatusCode::METHOD_NOT_ALLOWED,
            "{sibling} is a write and must refuse QUERY"
        );
        assert_eq!(write.header(header::ALLOW), Some("POST"));
    }
}

/// A flavor dispatcher may mix reads and writes. Every projection and gate
/// reads the selected descriptor spec: a viewer sees and reaches `look`, never
/// `touch`; a writer still gets `touch` as POST-only.
#[tokio::test]
async fn mixed_flavor_dispatcher_actions_keep_role_and_method_boundaries() {
    let host = flavor_host();
    let router = app(host.clone());
    let viewer = viewer_auth(ToolScope::All);
    let writer = auth(ToolScope::All);

    let catalog = host
        .read_resource("proxima://tools", author(), Some(viewer.clone()))
        .await
        .expect("viewer reads the tool catalog");
    let dispatch = catalog["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .find(|tool| tool["tool_id"] == FLAVOR_DISPATCH)
        .expect("mixed dispatcher stays visible through its read action");
    let actions = dispatch["actions"].as_array().expect("actions");
    assert_eq!(actions.len(), 1, "viewer catalog: {dispatch:#}");
    assert_eq!(actions[0]["action"], "look");
    assert_eq!(
        actions[0]["description"],
        "Inspect one thing without changing it."
    );
    assert_eq!(actions[0]["annotations"]["read_only"], true);

    let rest_catalog = get(&router, "/v1/tools", &viewer).await.json();
    let rest_dispatch = rest_catalog["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .find(|tool| tool["id"] == FLAVOR_DISPATCH)
        .expect("REST advertises the mixed dispatcher");
    let advertised = rest_dispatch["args_schema"]["x-proxima-actions"]
        .as_object()
        .expect("actions");
    assert_eq!(advertised.keys().collect::<Vec<_>>(), ["look"]);
    assert_eq!(
        advertised["look"]["description"],
        "Inspect one thing without changing it."
    );
    assert_eq!(rest_dispatch["annotations"]["read_only"], true);

    for uri in [
        format!("/v1/tools/{FLAVOR_DISPATCH}/look"),
        format!("/v1/tools/{FLAVOR_DISPATCH}"),
    ] {
        let body = if uri.ends_with("/look") {
            serde_json::json!({ "id": "x" })
        } else {
            serde_json::json!({ "action": "look", "id": "x" })
        };
        let answer = call(&router, Method::QUERY, &uri, &viewer, Some(body)).await;
        assert_eq!(answer.status, StatusCode::OK, "viewer QUERY {uri}");
    }

    let touch = format!("/v1/tools/{FLAVOR_DISPATCH}/touch");
    let retryable_write = call(
        &router,
        Method::QUERY,
        &touch,
        &viewer,
        Some(serde_json::json!({ "id": "x" })),
    )
    .await;
    assert_eq!(retryable_write.status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(retryable_write.header(header::ALLOW), Some("POST"));

    let viewer_write = call(
        &router,
        Method::POST,
        &touch,
        &viewer,
        Some(serde_json::json!({ "id": "x" })),
    )
    .await;
    assert_eq!(viewer_write.status, StatusCode::FORBIDDEN);

    let writer_write = call(
        &router,
        Method::POST,
        &touch,
        &writer,
        Some(serde_json::json!({ "id": "x" })),
    )
    .await;
    assert_eq!(writer_write.status, StatusCode::OK);

    let viewer_openapi = get(&router, "/v1/openapi.json", &viewer).await.json();
    let look_path = format!("/v1/tools/{FLAVOR_DISPATCH}/look");
    assert!(viewer_openapi["paths"][&look_path]["query"].is_object());
    assert!(viewer_openapi["paths"].get(&touch).is_none());
    assert!(viewer_openapi["paths"][&format!("/v1/tools/{FLAVOR_DISPATCH}")]["query"].is_object());

    let writer_openapi = get(&router, "/v1/openapi.json", &writer).await.json();
    assert!(writer_openapi["paths"][&touch]["post"].is_object());
    assert!(writer_openapi["paths"][&touch].get("query").is_none());
    assert!(
        writer_openapi["paths"][&format!("/v1/tools/{FLAVOR_DISPATCH}")]
            .get("query")
            .is_none()
    );
}

/// An argv-keyed dispatcher mixes reads and writes exactly like a tagged
/// one, and every gate reads the derived command's own annotation.
///
/// Before `McpArgvActionSpec` could carry annotations, every command
/// classified from the tool — which, for a dispatcher that must declare
/// itself writable because a minority of its commands write, cost a
/// read-capable-only owner the whole read surface and took retry-safe
/// `QUERY` off the reads as well.
#[tokio::test]
async fn an_annotated_argv_read_command_keeps_role_and_query_under_a_write_tool() {
    let host = argv_host();
    let router = app(host.clone());
    let viewer = viewer_auth(ToolScope::All);
    let writer = auth(ToolScope::All);
    let uri = format!("/v1/tools/{FLAVOR_ARGV}");

    // The tool itself is a write; the annotated command is not.
    let rest_catalog = get(&router, "/v1/tools", &viewer).await.json();
    assert!(
        rest_catalog["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .any(|tool| tool["id"] == FLAVOR_ARGV),
        "the read command keeps a writable argv dispatcher visible to a viewer: {rest_catalog:#}"
    );

    let read = call(
        &router,
        Method::QUERY,
        &uri,
        &viewer,
        Some(serde_json::json!({ "argv": ["approval", "--list"] })),
    )
    .await;
    assert_eq!(read.status, StatusCode::OK, "viewer QUERY {uri}");
    assert_eq!(read.json(), serde_json::json!(["approval", "--list"]));

    // The unannotated sibling classifies from the tool, which writes: no
    // QUERY for anyone, and no call at all for a viewer.
    let retryable_write = call(
        &router,
        Method::QUERY,
        &uri,
        &writer,
        Some(serde_json::json!({ "argv": ["approval", "decide", "--id", "7"] })),
    )
    .await;
    assert_eq!(retryable_write.status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(retryable_write.header(header::ALLOW), Some("GET, POST"));

    let viewer_write = call(
        &router,
        Method::POST,
        &uri,
        &viewer,
        Some(serde_json::json!({ "argv": ["approval", "decide", "--id", "7"] })),
    )
    .await;
    assert_eq!(viewer_write.status, StatusCode::FORBIDDEN);

    let writer_write = call(
        &router,
        Method::POST,
        &uri,
        &writer,
        Some(serde_json::json!({ "argv": ["approval", "decide", "--id", "7"] })),
    )
    .await;
    assert_eq!(writer_write.status, StatusCode::OK);

    // The OpenAPI document says the same thing the router enforces: a
    // caller who can only reach the read command is offered `query`; one
    // who can also reach the write is not.
    let viewer_openapi = get(&router, "/v1/openapi.json", &viewer).await.json();
    assert!(viewer_openapi["paths"][&uri]["query"].is_object());
    let writer_openapi = get(&router, "/v1/openapi.json", &writer).await.json();
    assert!(writer_openapi["paths"][&uri].get("query").is_none());
}

/// `QUERY` and `POST` are the same read, so they must answer identically —
/// byte for byte. Any divergence would mean the method, not the arguments,
/// changed the answer.
#[tokio::test]
async fn query_and_post_on_a_read_only_tool_are_byte_identical() {
    let router = app(host());
    let ctx = auth(ToolScope::All);
    let body = serde_json::json!({ "query": "anything" });

    let via_post = call(
        &router,
        Method::POST,
        "/v1/tools/core_search_memories",
        &ctx,
        Some(body.clone()),
    )
    .await;
    let via_query = call(
        &router,
        Method::QUERY,
        "/v1/tools/core_search_memories",
        &ctx,
        Some(body),
    )
    .await;

    assert_ne!(via_post.status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(via_post.status, via_query.status);
    assert_eq!(via_post.body, via_query.body);
    assert_eq!(
        via_post.header(header::CONTENT_TYPE),
        via_query.header(header::CONTENT_TYPE)
    );
    // Owner- and token-scoped either way; QUERY's cacheability is unused.
    assert_eq!(
        via_post.header(header::CACHE_CONTROL),
        Some("private, no-store")
    );
    assert_eq!(
        via_query.header(header::CACHE_CONTROL),
        Some("private, no-store")
    );
}

// ---------------------------------------------------- resource passthrough

/// The path and raw query are pasted back onto `proxima://` and handed to
/// the same seam MCP uses, so a malformed parameter produces the same error
/// class — REST adds no parser that could classify it differently.
#[tokio::test]
async fn resource_reads_match_the_mcp_error_class() {
    let host = host();
    let router = app(host.clone());
    let ctx = auth(ToolScope::All);

    for (rest_uri, resource_uri, expected) in [
        (
            "/v1/resources/change-events?limit=not-a-number",
            "proxima://change-events?limit=not-a-number",
            StatusCode::BAD_REQUEST,
        ),
        (
            "/v1/resources/memory/F:018f0000-0000-7000-8000-000000000001/lineage?direction=sideways",
            "proxima://memory/F:018f0000-0000-7000-8000-000000000001/lineage?direction=sideways",
            StatusCode::BAD_REQUEST,
        ),
        (
            "/v1/resources/no-such-resource",
            "proxima://no-such-resource",
            StatusCode::NOT_FOUND,
        ),
        (
            "/v1/resources/wake-candidates",
            "proxima://wake-candidates",
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let mcp_error = host
            .read_resource(resource_uri, author(), Some(ctx.clone()))
            .await
            .expect_err("the MCP read fails too");

        let answer = get(&router, rest_uri, &ctx).await;
        assert_eq!(answer.status, expected, "{rest_uri}");
        assert_eq!(
            answer.header(header::CONTENT_TYPE),
            Some("application/problem+json")
        );
        // Same fault, same words: `detail` is the client message the MCP
        // surface would have sent, not a REST-side restatement.
        let detail = answer.json()["detail"]
            .as_str()
            .expect("detail")
            .to_string();
        assert!(
            detail.contains(
                resource_uri
                    .trim_start_matches("proxima://")
                    .split('?')
                    .next()
                    .unwrap_or_default()
            ) || detail == format!("tool {resource_uri} not found"),
            "detail {detail} does not describe {mcp_error:?}"
        );
    }
}

/// `proxima://how-to` is synthesized per request rather than served through
/// the seam, so it is unreachable through the resource passthrough and has
/// its own route.
#[tokio::test]
async fn how_to_has_its_own_route_and_is_not_a_passthrough_resource() {
    let router = app(host());
    let ctx = auth(ToolScope::All);

    let passthrough = get(&router, "/v1/resources/how-to", &ctx).await;
    assert_eq!(passthrough.status, StatusCode::NOT_FOUND);

    let answer = get(&router, "/v1/how-to", &ctx).await;
    assert_eq!(answer.status, StatusCode::OK);
    assert_eq!(
        answer.header(header::CONTENT_TYPE),
        Some("text/markdown; charset=utf-8")
    );
    assert!(!answer.body.is_empty());
}

#[tokio::test]
async fn the_resource_catalog_is_scope_filtered_and_maps_onto_rest_paths() {
    let router = app(host());

    let full = get(&router, "/v1/resources", &auth(ToolScope::All)).await;
    assert_eq!(full.status, StatusCode::OK);
    let resources = full.json()["resources"]
        .as_array()
        .expect("resources")
        .clone();
    assert!(!resources.is_empty());
    for resource in &resources {
        let path = resource["path"].as_str().expect("path");
        assert!(path.starts_with("/v1/resources/"), "{path}");
    }

    let empty = get(
        &router,
        "/v1/resources",
        &auth(ToolScope::Palette(Vec::new())),
    )
    .await;
    assert_eq!(
        empty.json()["resources"].as_array().map(Vec::len),
        Some(0),
        "an empty palette advertises no resources"
    );
}

#[tokio::test]
async fn the_openapi_document_is_caller_scoped_and_never_shared_cacheable() {
    let router = app(host());
    let answer = get(&router, "/v1/openapi.json", &auth(ToolScope::All)).await;
    assert_eq!(answer.status, StatusCode::OK);
    assert_eq!(
        answer.header(header::CONTENT_TYPE),
        Some("application/json")
    );
    assert_eq!(
        answer.header(header::CACHE_CONTROL),
        Some("private, no-store")
    );
    assert!(answer.json()["openapi"].as_str().is_some());
}
