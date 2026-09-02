//! A flavor can declare a dispatcher, and every seam treats it as one.
//!
//! `McpToolDescriptor::action_arg_specs` is THE enumeration of a dispatcher's
//! actions. Every seam reads it: the scope gate, argument validation,
//! `proxima://tools`, and the REST action routes. A tool whose `Args` is an
//! internally tagged enum has `x-proxima-actions` stamped on its schema, so
//! clients see a dispatcher — and one registered with an empty spec slice is
//! gated whole, validated against every variant's fields merged together,
//! lists no actions in `proxima://tools`, and serves no REST action route.

use std::sync::Arc;

use futures::future::BoxFuture;
use proxima_core::flavor::{FlavorContract, ProjectionDecl, ToolContract};
use proxima_core::mcp::{
    McpActionArgSpec, McpAuthorContext, McpTool, McpToolAnnotations, McpToolAudience, McpToolCtx,
    McpToolError, McpToolOrigin, Next, RequestBehavior, ScopeGateBehavior, TerminalDispatch,
    ToolCall, core_action_meta,
};
use proxima_core::{
    AuthPath, AuthzContext, FlavorRegistry, FlavorRegistryFrozen, FlavorServices, GroupId,
    OwnerRef, Tool, ToolCtx, ToolError, ToolScope, UserId, access::Role, proxima_flavor,
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
    /// Inspect one thing without changing it.
    Look {
        #[schemars(description = "Which thing to look at.")]
        id: String,
    },
    /// Change one thing and optionally record a note.
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
    // Deliberately read-only at parent level: action specs remain authoritative
    // and keep the mixed dispatcher conservative as a whole.
    const ANNOTATIONS: Option<McpToolAnnotations> =
        Some(McpToolAnnotations::new().read_only(true).open_world(false));
    const ACTION_ARG_SPECS: &'static [McpActionArgSpec] = &[
        McpActionArgSpec {
            action: "look",
            allowed_fields: &["id"],
            required_fields: &["id"],
            annotations: Some(McpToolAnnotations::new().read_only(true).open_world(false)),
            audience: McpToolAudience::Shared,
        },
        McpActionArgSpec {
            action: "touch",
            allowed_fields: &["id", "note"],
            required_fields: &["id"],
            annotations: Some(McpToolAnnotations::new().read_only(false).open_world(false)),
            audience: McpToolAudience::Shared,
        },
    ];
    type Args = DispatchArgs;
    type Output = ();

    fn call(_ctx: ToolCtx, _args: Self::Args) -> BoxFuture<'static, Result<(), ToolError>> {
        Box::pin(async { Ok(()) })
    }
}

/// The flavor's declaration. `contract =` is optional to the macro and
/// refused at freeze when omitted, so the fixture carries one.
///
/// `actions` and `idempotent` are held to the REGISTRATION: the action list
/// is the specs above, in order, and `idempotent` is false because a
/// dispatcher resolves its annotations per action and this one mixes a read
/// with a write.
static DISPATCH_FLAVOR_CONTRACT: FlavorContract = FlavorContract {
    flavor_id: FLAVOR,
    // Non-zero: ordinal 0 is core's, and two claims on it are refused.
    ordinal: 7,
    schemas: &[],
    state_surfaces: &[],
    scopes: &[],
    kernel_surfaces: &[],
    tools: &[ToolContract {
        wire_name: DISPATCH,
        actions: &["look", "touch"],
        idempotent: false,
    }],
    resources: &[],
    bespoke_erase_legs: &[],
    bespoke_transfer_legs: &[],
    projection: ProjectionDecl::None {
        why: "a dispatcher fixture registers no search surface",
    },
};

proxima_flavor! {
    name = "proxima-core",
    mcp_tools = [ DispatchTool ],
    contract = &DISPATCH_FLAVOR_CONTRACT,
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
        services: FlavorServices::default(),
        engine: None,
    }
}

fn viewer_ctx(registry: &Arc<FlavorRegistryFrozen>, scope: ToolScope) -> McpToolCtx {
    let owner = OwnerRef::Group(GroupId::new(uuid::Uuid::now_v7()));
    McpToolCtx {
        owner,
        authz: AuthzContext::for_subject_with_role(
            UserId::new(uuid::Uuid::now_v7()),
            [(owner, Role::viewer())],
            AuthPath::HostBearer,
        )
        .with_tool_scope(scope),
        registry: registry.clone(),
        author: McpAuthorContext {
            model_id: "test".into(),
            client_name: "test".into(),
            client_version: "0".into(),
            caller_self_perspective: None,
        },
        caller_self_perspective: None,
        services: FlavorServices::default(),
        engine: None,
    }
}

async fn run_gate(
    ctx: McpToolCtx,
    args: serde_json::Value,
) -> Result<serde_json::Value, McpToolError> {
    let behaviors: Vec<Arc<dyn RequestBehavior>> = vec![Arc::new(ScopeGateBehavior)];
    let terminal: TerminalDispatch<'_> =
        Box::new(|_call| Box::pin(async { Ok(serde_json::json!({ "reached": true })) }));
    Next::new(&behaviors, terminal)
        .run(ToolCall {
            name: DISPATCH.to_string(),
            args,
            ctx,
        })
        .await
}

/// Run the argument through the real behavior chain, which is the only way
/// `ScopeGateBehavior` is reached in production.
async fn through_the_gate(
    registry: &Arc<FlavorRegistryFrozen>,
    scope: ToolScope,
    args: serde_json::Value,
) -> Result<serde_json::Value, McpToolError> {
    run_gate(ctx(registry, scope), args).await
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
    assert_eq!(
        descriptor.resolved_action_description("look"),
        Some("Inspect one thing without changing it."),
    );
    assert_eq!(
        descriptor.resolved_action_description("touch"),
        Some("Change one thing and optionally record a note."),
    );
}

/// The schema-derived variant prose is also the flavor catalog prose; there
/// is no second per-action description table for flavors.
#[tokio::test]
async fn a_flavor_dispatcher_catalog_uses_variant_descriptions() {
    use proxima_core::mcp::core_tools::list_substrate_tools::{
        ListSubstrateToolsArgs, list_substrate_tools,
    };

    let registry = frozen();
    let output = list_substrate_tools(ctx(&registry, ToolScope::All), ListSubstrateToolsArgs {})
        .await
        .expect("catalog lists the flavor dispatcher");
    let dispatch = output
        .tools
        .iter()
        .find(|tool| tool.tool_id == DISPATCH)
        .expect("flavor dispatcher catalog row");

    assert_eq!(dispatch.actions[0].action, "look");
    assert_eq!(
        dispatch.actions[0].description,
        "Inspect one thing without changing it."
    );
    assert_eq!(dispatch.actions[1].action, "touch");
    assert_eq!(
        dispatch.actions[1].description,
        "Change one thing and optionally record a note."
    );
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

/// Owner-role classification follows the selected descriptor spec. The
/// parent deliberately says read-only, but the write leaf stays a write.
#[tokio::test]
async fn a_viewer_reaches_only_the_read_action_of_a_mixed_flavor_dispatcher() {
    let registry = frozen();

    run_gate(
        viewer_ctx(&registry, ToolScope::All),
        serde_json::json!({ "action": "look", "id": "x" }),
    )
    .await
    .expect("the read action admits a viewer");

    let err = run_gate(
        viewer_ctx(&registry, ToolScope::All),
        serde_json::json!({ "action": "touch", "id": "x" }),
    )
    .await
    .expect_err("the write action still requires a writer");
    assert!(
        matches!(err, McpToolError::NotAuthorized(ref key) if key == DISPATCH),
        "got {err:?}",
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

/// Strict pre-decode validation is a dispatcher property: arguments are
/// checked per action before decode.
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

/// Per-action annotations live beside the field contract, so flavor actions
/// need no substrate metadata and never inherit their parent's behaviour.
#[test]
fn a_flavor_dispatcher_resolves_behavior_from_its_action_specs() {
    let registry = frozen();
    let descriptor = registry.mcp_tool(DISPATCH).expect("registered");

    assert!(
        core_action_meta(DISPATCH, "look").is_none(),
        "CoreActionMeta is a substrate table; a flavor action has no entry",
    );
    assert!(
        !descriptor.is_read_only(),
        "a mixed dispatcher is conservatively a write as a whole",
    );
    assert!(descriptor.action_is_read_only("look"));
    assert!(!descriptor.action_is_read_only("touch"));
    assert!(!descriptor.action_is_read_only("unknown"));
    assert_eq!(<DispatchTool as McpTool>::ACTION_ARG_SPECS.len(), 2);
}
