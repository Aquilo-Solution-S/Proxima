use std::sync::{Arc, Mutex};

use futures_util::future::BoxFuture;
use proxima_core::mcp::{
    JudgedKey, McpActionArgSpec, McpTool, McpToolAudience, McpToolCtx, McpToolError,
};
use proxima_core::{AuthPath, AuthzContext, FlavorRegistry, FlavorServices, ScopeKey, ToolScope};
use rmcp::service::{RunningService, serve_directly};
use serde_json::{Value, json};

use super::*;

const FLAT: &str = "observed_ping";
const DISPATCH: &str = "observed_dispatch";

#[derive(Clone, Debug, Default)]
struct Seen(Arc<Mutex<Vec<JudgedKey>>>);

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct FlatArgs {}

#[derive(Debug, serde::Serialize, schemars::JsonSchema)]
struct Output {}

#[derive(Debug)]
struct Flat;

impl McpTool for Flat {
    const TITLE: &'static str = "Observed flat tool";
    const NAME: &'static str = FLAT;
    const DESCRIPTION: &'static str = "Retains the context's judged cell.";
    const EFFECT: Option<ToolEffect> = Some(ToolEffect::ReadOnly);
    type Args = FlatArgs;
    type Output = Output;

    fn call(ctx: McpToolCtx, _: FlatArgs) -> BoxFuture<'static, Result<Output, McpToolError>> {
        Box::pin(async move {
            ctx.services
                .get::<Seen>()
                .expect("fixture service")
                .0
                .lock()
                .expect("seen")
                .push(ctx.judged);
            Ok(Output {})
        })
    }
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case")]
enum DispatchArgs {
    /// Read the fixture.
    Look {},
    /// Change the fixture.
    Touch {},
}

#[derive(Debug)]
struct Dispatch;

impl McpTool for Dispatch {
    const TITLE: &'static str = "Observed dispatcher";
    const NAME: &'static str = DISPATCH;
    const DESCRIPTION: &'static str = "Exercises action scope decisions.";
    const ACTION_ARG_SPECS: &'static [McpActionArgSpec] = &[
        McpActionArgSpec {
            action: "look",
            allowed_fields: &[],
            required_fields: &[],
            effect: ToolEffect::ReadOnly,
            audience: McpToolAudience::Shared,
            title: "Look",
        },
        McpActionArgSpec {
            action: "touch",
            allowed_fields: &[],
            required_fields: &[],
            effect: ToolEffect::ReadOnly,
            audience: McpToolAudience::Shared,
            title: "Touch",
        },
    ];
    type Args = DispatchArgs;
    type Output = Output;

    fn call(_: McpToolCtx, _: DispatchArgs) -> BoxFuture<'static, Result<Output, McpToolError>> {
        Box::pin(async { Ok(Output {}) })
    }
}

struct Fixture {
    handler: DynamicHandler,
    running: RunningService<RoleServer, DynamicHandler>,
    _remote: tokio::io::DuplexStream,
    seen: Seen,
}

impl Fixture {
    fn new() -> Self {
        let mut registry = FlavorRegistry::new();
        registry.add_mcp_tool_or_panic_for_tests::<Flat>("observed");
        registry.add_mcp_tool_or_panic_for_tests::<Dispatch>("observed");
        let seen = Seen::default();
        let mut services = FlavorServices::default();
        services.try_insert(seen.clone()).expect("unique service");
        let handler = DynamicHandler::new(McpToolHost::from_parts(
            Arc::new(registry.freeze_or_panic_for_tests()),
            services,
        ));
        let (transport, remote) = tokio::io::duplex(4096);
        let running = serve_directly::<RoleServer, _, _, _, _>(handler.clone(), transport, None);
        Self {
            handler,
            running,
            _remote: remote,
            seen,
        }
    }

    fn context(&self, scope: Option<ToolScope>) -> RequestContext<RoleServer> {
        let mut context = RequestContext::new(
            rmcp::model::NumberOrString::Number(1),
            self.running.peer().clone(),
        );
        if let Some(scope) = scope {
            let owner = proxima_core::test_fixtures::owner_fixture();
            let (mut parts, ()) = http::Request::new(()).into_parts();
            parts.extensions.insert(McpAuthContext {
                owner,
                authz: AuthzContext::single_owner(&owner, AuthPath::HostBearer)
                    .with_tool_scope(scope),
            });
            context.extensions.insert(parts);
        }
        context
    }
}

fn request(name: &str, args: &Value) -> CallToolRequestParams {
    serde_json::from_value(json!({"name": name, "arguments": args})).expect("tool request")
}

fn key(value: &str) -> ScopeKey {
    ScopeKey::parse(value).expect("fixture key")
}

#[tokio::test]
async fn observed_flat_and_action_calls_report_the_gate_key() {
    let fixture = Fixture::new();
    for (name, args, expected) in [
        (FLAT, json!({}), FLAT),
        (
            DISPATCH,
            json!({"action": "look"}),
            "observed_dispatch:look",
        ),
    ] {
        let (judged, call) = fixture
            .handler
            .call_tool_observed(request(name, &args), &fixture.context(Some(ToolScope::All)));
        assert_eq!(judged.get(), None);
        assert!(call.await.is_ok());
        assert_eq!(judged.get(), Some(key(expected)));
    }
    assert_eq!(
        fixture.seen.0.lock().expect("seen")[0].get(),
        Some(key(FLAT))
    );
}

#[tokio::test]
async fn observed_scope_denial_retains_the_denied_action() {
    let fixture = Fixture::new();
    let (judged, call) = fixture.handler.call_tool_observed(
        request(DISPATCH, &json!({"action": "touch"})),
        &fixture.context(Some(ToolScope::Palette(vec![key(
            "observed_dispatch:look",
        )]))),
    );
    let error = call.await.expect_err("touch is outside the palette");
    assert!(error.message.contains("not authorized"), "{error:?}");
    assert_eq!(judged.get(), Some(key("observed_dispatch:touch")));
}

#[tokio::test]
async fn observed_invalid_actions_have_no_judged_key() {
    let fixture = Fixture::new();
    for args in [
        json!({}),
        json!({"action": 7}),
        json!({"action": "unknown"}),
    ] {
        let (judged, call) = fixture.handler.call_tool_observed(
            request(DISPATCH, &args),
            &fixture.context(Some(ToolScope::All)),
        );
        assert!(call.await.is_err());
        assert_eq!(judged.get(), None);
    }
}

#[tokio::test]
async fn observed_early_refusals_have_no_judged_key() {
    let fixture = Fixture::new();
    for (name, args, scope) in [
        ("missing_tool", json!({}), Some(ToolScope::All)),
        (FLAT, json!({}), None),
        (FLAT, json!({"bad": "\u{0}"}), Some(ToolScope::All)),
    ] {
        let (judged, call) = fixture
            .handler
            .call_tool_observed(request(name, &args), &fixture.context(scope));
        assert!(call.await.is_err());
        assert_eq!(judged.get(), None);
    }
}

#[tokio::test]
async fn observations_are_isolated_even_when_another_call_is_pending() {
    let fixture = Fixture::new();
    let (flat_key, flat_call) = fixture.handler.call_tool_observed(
        request(FLAT, &json!({})),
        &fixture.context(Some(ToolScope::All)),
    );
    let (action_key, action_call) = fixture.handler.call_tool_observed(
        request(DISPATCH, &json!({"action": "look"})),
        &fixture.context(Some(ToolScope::All)),
    );
    assert!(flat_call.await.is_ok());
    assert_eq!(flat_key.get(), Some(key(FLAT)));
    assert_eq!(action_key.get(), None);
    let retained = fixture.seen.0.lock().expect("seen")[0].clone();
    assert!(action_call.await.is_ok());
    assert_eq!(action_key.get(), Some(key("observed_dispatch:look")));
    assert_eq!(flat_key.get(), Some(key(FLAT)));
    assert_eq!(retained.get(), flat_key.get());
    let (invalid_key, invalid_call) = fixture.handler.call_tool_observed(
        request(DISPATCH, &json!({})),
        &fixture.context(Some(ToolScope::All)),
    );
    assert!(invalid_call.await.is_err());
    assert_eq!(invalid_key.get(), None);
    assert_eq!(retained.get(), Some(key(FLAT)));
}

impl Fixture {
    fn host_call<'a>(
        &'a self,
        name: &'a str,
        args: Value,
        scope: Option<ToolScope>,
    ) -> (
        JudgedKey,
        impl Future<Output = Result<ToolReply, crate::ToolInvocationError>> + use<'a>,
    ) {
        let owner = proxima_core::test_fixtures::owner_fixture();
        let auth = scope.map(|scope| McpAuthContext {
            owner,
            authz: AuthzContext::single_owner(&owner, AuthPath::HostBearer).with_tool_scope(scope),
        });
        self.handler.server.call_tool_reply_observed_in_request(
            name,
            args,
            proxima_core::mcp::McpAuthorContext {
                model_id: "fixture-model".into(),
                trusted_model_id: None,
                client_name: "fixture".into(),
                client_version: "1".into(),
                caller_self_perspective: None,
            },
            auth,
            FlavorServices::default(),
        )
    }
}

#[tokio::test]
async fn observed_host_calls_retain_the_flat_and_action_gate_keys() {
    let fixture = Fixture::new();
    for (name, args, expected) in [
        (FLAT, json!({}), FLAT),
        (
            DISPATCH,
            json!({"action": "look"}),
            "observed_dispatch:look",
        ),
    ] {
        let (judged, call) = fixture.host_call(name, args, Some(ToolScope::All));
        assert_eq!(judged.get(), None);
        assert_eq!(
            call.await.expect("admitted reply"),
            ToolReply::Structured(json!({}))
        );
        assert_eq!(judged.get(), Some(key(expected)));
    }
    assert_eq!(
        fixture.seen.0.lock().expect("seen")[0].get(),
        Some(key(FLAT))
    );
}

#[tokio::test]
async fn observed_host_scope_refusal_retains_the_denied_action() {
    let fixture = Fixture::new();
    let (judged, call) = fixture.host_call(
        DISPATCH,
        json!({"action": "touch"}),
        Some(ToolScope::Palette(vec![key("observed_dispatch:look")])),
    );
    assert!(matches!(
        call.await,
        Err(crate::ToolInvocationError::NotAuthorized(_))
    ));
    assert_eq!(judged.get(), Some(key("observed_dispatch:touch")));
}

#[tokio::test]
async fn observed_host_pre_gate_refusals_leave_the_cell_empty() {
    let fixture = Fixture::new();
    for (name, args, scope) in [
        (DISPATCH, json!({}), Some(ToolScope::All)),
        (DISPATCH, json!({"action": 7}), Some(ToolScope::All)),
        (DISPATCH, json!({"action": "unknown"}), Some(ToolScope::All)),
        ("missing_tool", json!({}), Some(ToolScope::All)),
        (FLAT, json!({}), None),
        (FLAT, json!({"bad": "\u{0}"}), Some(ToolScope::All)),
    ] {
        let (judged, call) = fixture.host_call(name, args, scope);
        assert!(call.await.is_err());
        assert_eq!(judged.get(), None);
    }
}

#[tokio::test]
async fn observed_host_cells_are_isolated_while_another_call_is_pending() {
    let fixture = Fixture::new();
    let (first, first_call) = fixture.host_call(FLAT, json!({}), Some(ToolScope::All));
    let (second, second_call) =
        fixture.host_call(DISPATCH, json!({"action": "look"}), Some(ToolScope::All));
    assert!(first_call.await.is_ok());
    assert_eq!(first.get(), Some(key(FLAT)));
    assert_eq!(second.get(), None);
    let retained = fixture.seen.0.lock().expect("seen")[0].clone();
    assert!(second_call.await.is_ok());
    assert_eq!(second.get(), Some(key("observed_dispatch:look")));
    let (invalid, invalid_call) = fixture.host_call(DISPATCH, json!({}), Some(ToolScope::All));
    assert!(invalid_call.await.is_err());
    assert_eq!(invalid.get(), None);
    assert_eq!(first.get(), Some(key(FLAT)));
    assert_eq!(retained.get(), Some(key(FLAT)));
}
