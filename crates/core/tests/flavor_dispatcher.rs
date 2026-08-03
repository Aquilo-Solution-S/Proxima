//! A flavor can declare a dispatcher, and every seam treats it as one.
//!
//! `McpToolDescriptor::action_arg_specs` is THE enumeration of a dispatcher's
//! actions. Before that was true, a flavor tool with an internally tagged
//! `Args` reached the registry through `try_add_tool`, which hardcoded an
//! empty spec slice: the schema pass still stamped `x-proxima-actions` on it,
//! so clients saw a dispatcher, while the scope gate keyed off the substrate
//! `CoreActionMeta` tables and saw a flat tool. The tool was gated whole,
//! validated against every variant's fields merged together, listed no
//! actions in `proxima://tools`, and served no REST action route.

use std::sync::Arc;

use futures::future::BoxFuture;
use proxima_core::mcp::{
    McpActionArgSpec, McpAuthorContext, McpTool, McpToolAnnotations, McpToolCtx, McpToolError,
    McpToolExtensions, McpToolOrigin, Next, RequestBehavior, ScopeGateBehavior, TerminalDispatch,
    ToolCall, core_action_meta,
};
use proxima_core::{
    AuthPath, AuthzContext, FlavorRegistry, FlavorRegistryFrozen, OwnerRef, Tool, ToolCtx,
    ToolError, ToolScope, UserId, proxima_flavor,
};

/// `CARGO_PKG_NAME` is `proxima-core` inside core's own `tests/`, so the
/// macro's prefix assertion demands that name — see `flavor_macro.rs`.
const FLAVOR: &str = "proxima-core";
const DISPATCH: &str = "proxima-core_dispatch";

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case")]
#[expect(
    dead_code,
    reason = "the derived schema is the subject, not the values"
)]
enum DispatchArgs {
    Look {
        #[schemars(description = "Which thing to look at.")]
        id: String,
    },
    Touch {
        #[schemars(description = "Which thing to touch.")]
        id: String,
        #[schemars(description = "Optional note recorded with the touch.")]
        note: Option<String>,
    },
}

#[derive(Debug)]
struct DispatchTool;

impl Tool for DispatchTool {
    const NAME: &'static str = DISPATCH;
    const DESCRIPTION: &'static str = "A flavor dispatcher declared through proxima_flavor!.";
    const ANNOTATIONS: Option<McpToolAnnotations> =
        Some(McpToolAnnotations::new().read_only(false).open_world(false));
    const ACTION_ARG_SPECS: &'static [McpActionArgSpec] = &[
        McpActionArgSpec {
            action: "look",
            allowed_fields: &["id"],
            required_fields: &["id"],
        },
        McpActionArgSpec {
            action: "touch",
            allowed_fields: &["id", "note"],
            required_fields: &["id"],
        },
    ];
    type Args = DispatchArgs;
    type Output = ();

    fn call(_ctx: ToolCtx, _args: Self::Args) -> BoxFuture<'static, Result<(), ToolError>> {
        Box::pin(async { Ok(()) })
    }
}

proxima_flavor! {
    name = "proxima-core",
    mcp_tools = [ DispatchTool ],
}

fn frozen() -> Arc<FlavorRegistryFrozen> {
    let mut registry = FlavorRegistry::new();
    register(&mut registry).expect("the flavor registers");
    Arc::new(registry.freeze_or_panic_for_tests())
}

fn ctx(registry: &Arc<FlavorRegistryFrozen>, scope: ToolScope) -> McpToolCtx {
    let owner = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));
    McpToolCtx {
        owner,
        authz: AuthzContext::single_owner(&owner, AuthPath::HostBearer).with_tool_scope(scope),
        registry: registry.clone(),
        author: McpAuthorContext {
            model_id: "test".into(),
            client_name: "test".into(),
            client_version: "0".into(),
            caller_self_perspective: None,
        },
        caller_self_perspective: None,
        extensions: McpToolExtensions::default(),
        engine: None,
    }
}

/// Run the argument through the real behavior chain, which is the only way
/// `ScopeGateBehavior` is reached in production.
async fn through_the_gate(
    registry: &Arc<FlavorRegistryFrozen>,
    scope: ToolScope,
    args: serde_json::Value,
) -> Result<serde_json::Value, McpToolError> {
    let behaviors: Vec<Arc<dyn RequestBehavior>> = vec![Arc::new(ScopeGateBehavior)];
    let terminal: TerminalDispatch<'_> =
        Box::new(|_call| Box::pin(async { Ok(serde_json::json!({ "reached": true })) }));
    Next::new(&behaviors, terminal)
        .run(ToolCall {
            name: DISPATCH.to_string(),
            args,
            ctx: ctx(registry, scope),
        })
        .await
}

/// The macro path stores the specs, and the derived schema agrees with them
/// — which `try_freeze` now also refuses to seal without.
#[test]
fn a_macro_registered_flavor_dispatcher_carries_its_action_specs() {
    let registry = frozen();
    let descriptor = registry
        .mcp_tool(DISPATCH)
        .expect("the macro registered the dispatcher");

    assert_eq!(
        descriptor.origin,
        McpToolOrigin::Flavor(FLAVOR.to_string()),
        "a macro-registered tool is flavor-origin",
    );
    let declared: Vec<&str> = descriptor
        .action_arg_specs
        .iter()
        .map(|spec| spec.action)
        .collect();
    assert_eq!(declared, ["look", "touch"]);

    let derived: Vec<&String> = descriptor
        .args_schema
        .get("x-proxima-actions")
        .and_then(serde_json::Value::as_object)
        .expect("the schema pass stamps the extension")
        .keys()
        .collect();
    assert_eq!(derived, ["look", "touch"]);
}

/// A palette holding one leaf grants one leaf. Keyed on the substrate
/// tables, this tool fell to the whole-tool branch: `allows("…_dispatch")`
/// is false for a palette of leaves, so the token that was granted `look`
/// was refused the tool outright.
#[tokio::test]
async fn a_flavor_dispatcher_is_gated_per_action() {
    let registry = frozen();
    let scope = ToolScope::Palette(vec![format!("{DISPATCH}:look")]);

    through_the_gate(
        &registry,
        scope.clone(),
        serde_json::json!({ "action": "look", "id": "x" }),
    )
    .await
    .expect("the granted leaf passes the gate");

    let err = through_the_gate(
        &registry,
        scope,
        serde_json::json!({ "action": "touch", "id": "x" }),
    )
    .await
    .expect_err("an ungranted leaf is refused");
    assert!(
        matches!(err, McpToolError::NotAuthorized(ref key) if key == &format!("{DISPATCH}:touch")),
        "the denial names the leaf, not the tool: {err:?}",
    );
}

/// The other half of the whole-tool fallback: with nothing enumerating the
/// actions, an unknown one reached the tool and failed at decode instead of
/// at the gate.
#[tokio::test]
async fn an_unknown_action_on_a_flavor_dispatcher_is_refused_at_the_gate() {
    let registry = frozen();
    let err = through_the_gate(
        &registry,
        ToolScope::All,
        serde_json::json!({ "action": "vanish" }),
    )
    .await
    .expect_err("an action the tool does not declare is refused");
    assert!(
        matches!(err, McpToolError::InvalidInput(ref message)
            if message.contains(r#"unknown action "vanish""#)),
        "got {err:?}",
    );
}

/// Strict pre-decode validation is a dispatcher property, not a substrate
/// one. `try_add_tool` used to store empty specs, so `validate_action_args`
/// short-circuited and every variant's fields were accepted on every action.
#[tokio::test]
async fn a_flavor_dispatcher_validates_arguments_per_action_before_decode() {
    let registry = frozen();
    let descriptor = registry.mcp_tool(DISPATCH).expect("registered");

    let err = (descriptor.call)(
        ctx(&registry, ToolScope::All),
        serde_json::json!({ "action": "look", "id": "x", "note": "y" }),
    )
    .await
    .expect_err("`note` belongs to `touch`, not to `look`");
    assert!(
        matches!(err, McpToolError::InvalidInput(ref message) if message.contains("note")),
        "got {err:?}",
    );

    let err = (descriptor.call)(
        ctx(&registry, ToolScope::All),
        serde_json::json!({ "action": "touch" }),
    )
    .await
    .expect_err("`touch` requires `id`");
    assert!(
        matches!(err, McpToolError::InvalidInput(ref message) if message.contains("id")),
        "got {err:?}",
    );
}

/// The known gap, pinned rather than left to be discovered: per-action
/// annotations are substrate-only decoration, so a flavor dispatcher's
/// read/write answer comes from the tool. See docs/12 §Known gaps.
#[test]
fn a_flavor_dispatcher_resolves_read_only_at_tool_level() {
    let registry = frozen();
    let descriptor = registry.mcp_tool(DISPATCH).expect("registered");

    assert!(
        core_action_meta(DISPATCH, "look").is_none(),
        "CoreActionMeta is a substrate table; a flavor action has no entry",
    );
    assert!(
        !descriptor.is_read_only(),
        "the tool's own ANNOTATIONS decide, and they say write",
    );
    assert_eq!(<DispatchTool as McpTool>::ACTION_ARG_SPECS.len(), 2);
}
