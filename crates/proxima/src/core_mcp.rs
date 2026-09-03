use std::sync::Arc;

use proxima_core::{
    AuthzContext, Engine, FlavorRegistryFrozen, FlavorServices, McpAuthorContext,
    McpToolDescriptor, McpToolErrorKind, Owner, ToolScope, provider_safe_tool_name,
    resolve_operator_label, tool_name_matches,
};
use proxima_mcp_server::{DynamicHandler, McpAuthContext, McpToolHost, ToolInvocationError};

/// Facade handle for listing and dispatching the composed engine MCP tools
/// from an embedding host's own authenticated endpoint.
#[derive(Clone, Debug)]
pub struct CoreMcpTools {
    host: McpToolHost,
}

/// MCP tool descriptor projected without rmcp transport types.
#[derive(Debug, Clone, PartialEq)]
pub struct CoreToolInfo {
    pub name: String,
    pub description: String,
    pub args_schema: serde_json::Value,
    /// The schema of what a successful call returns, as the descriptor
    /// derives it. The MCP handler puts it on `Tool::output_schema` and the
    /// REST document puts it on the 200 response; a host driving the tools
    /// through this facade had no way to see it at all.
    pub output_schema: serde_json::Value,
    pub read_only: Option<bool>,
    pub destructive: Option<bool>,
    pub idempotent: Option<bool>,
    pub open_world: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CoreMcpErrorKind {
    #[error("not authorized")]
    NotAuthorized,
    #[error("not found")]
    NotFound,
    #[error("invalid input")]
    InvalidInput,
    #[error("invalid request")]
    InvalidRequest,
    #[error("internal")]
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CoreMcpError {
    #[error("tool not authorized: {0}")]
    NotAuthorized(String),
    #[error("tool not found: {0}")]
    NotFound(String),
    #[error("tool error ({kind:?}): {message}")]
    Tool {
        kind: McpToolErrorKind,
        message: String,
    },
}

impl CoreMcpError {
    #[must_use]
    pub const fn kind(&self) -> CoreMcpErrorKind {
        match self {
            Self::NotAuthorized(_) => CoreMcpErrorKind::NotAuthorized,
            Self::NotFound(_) => CoreMcpErrorKind::NotFound,
            Self::Tool { kind, .. } => match kind {
                McpToolErrorKind::InvalidInput => CoreMcpErrorKind::InvalidInput,
                McpToolErrorKind::NotFound => CoreMcpErrorKind::NotFound,
                McpToolErrorKind::InvalidRequest => CoreMcpErrorKind::InvalidRequest,
                McpToolErrorKind::Internal => CoreMcpErrorKind::Internal,
            },
        }
    }

    fn from_invocation_error(err: ToolInvocationError) -> Self {
        match err {
            ToolInvocationError::NotAuthorized(tool) => Self::NotAuthorized(tool),
            ToolInvocationError::ToolNotFound(tool) => Self::NotFound(tool),
            ToolInvocationError::Tool(err) => Self::Tool {
                kind: err.kind(),
                message: err.to_string(),
            },
        }
    }
}

impl CoreMcpTools {
    #[must_use]
    pub fn new(
        registry: Arc<FlavorRegistryFrozen>,
        engine: Arc<Engine>,
        services: FlavorServices,
    ) -> Self {
        Self {
            host: McpToolHost::from_parts(registry, services).with_engine(engine),
        }
    }

    /// Consume this boot-wired facade and expose the exact native MCP handler.
    ///
    /// The handler retains the complete service bag assembled during Proxima
    /// boot. Embedding hosts can wrap it while preserving Proxima's native
    /// request, authorization, and session semantics.
    #[must_use]
    pub fn into_dynamic_handler(self) -> DynamicHandler {
        DynamicHandler { server: self.host }
    }

    /// List all build-time registered MCP tools from the frozen registry.
    ///
    /// This intentionally does not filter by `ToolScope`: the embedding
    /// host owns presentation/gating on its unified endpoint, and
    /// [`Self::call_core_tool`] runs the host behavior chain before dispatch.
    #[must_use]
    pub fn list_core_tools(&self) -> Vec<CoreToolInfo> {
        self.host
            .registry()
            .list_mcp_tools()
            .iter()
            .map(tool_info_from_descriptor)
            .collect::<Vec<_>>()
    }

    /// List registered MCP tools visible under `scope`.
    #[must_use]
    pub fn list_core_tools_for_scope(&self, scope: &ToolScope) -> Vec<CoreToolInfo> {
        self.host
            .registry()
            .list_mcp_tools()
            .iter()
            .filter(|descriptor| scope.allows_group_advertisement(descriptor.name))
            .map(tool_info_from_descriptor)
            .collect()
    }

    /// Dispatch one registered core/flavor MCP tool under caller-supplied
    /// host authz and storage owner.
    ///
    /// Direct facade dispatch supplies host authz and optional caller
    /// Perspective metadata only.
    ///
    /// # Errors
    ///
    /// Returns `NotAuthorized` when the host behavior chain rejects `name`,
    /// `NotFound` for an unknown registry tool, or `Tool` for validation,
    /// storage, or role errors.
    pub async fn call_core_tool(
        &self,
        authz: AuthzContext,
        owner: Owner,
        model_id: Option<String>,
        name: &str,
        args: serde_json::Value,
    ) -> Result<serde_json::Value, CoreMcpError> {
        let Some(descriptor) = self.find_tool(name) else {
            return Err(CoreMcpError::NotFound(name.to_string()));
        };
        let canonical_name = descriptor.name;

        let author = host_author(&authz, model_id.as_deref())?;
        let auth = McpAuthContext { owner, authz };

        self.host
            .call_tool(canonical_name, args, author, Some(auth))
            .await
            .map_err(CoreMcpError::from_invocation_error)
    }

    /// Read one core MCP resource through the same host authz boundary as
    /// direct tool dispatch.
    ///
    /// # Errors
    ///
    /// Returns `NotAuthorized` when the caller's resource scope rejects the
    /// URI, or `Tool` for resource validation/storage errors.
    pub async fn read_core_resource(
        &self,
        authz: AuthzContext,
        owner: Owner,
        model_id: Option<String>,
        uri: &str,
    ) -> Result<serde_json::Value, CoreMcpError> {
        let author = host_author(&authz, model_id.as_deref())?;
        let auth = McpAuthContext { owner, authz };

        self.host
            .read_resource(uri, author, Some(auth))
            .await
            .map_err(CoreMcpError::from_invocation_error)
    }

    fn find_tool(&self, name: &str) -> Option<&McpToolDescriptor> {
        find_tool_descriptor(self.host.registry().list_mcp_tools(), name)
    }
}

/// Operator provenance for one in-process host call.
///
/// Same precedence as the MCP and REST edges: a model identity bound by the
/// caller's own `AuthzContext` wins, and a host that passes a different
/// `model_id` is told rather than silently overridden.
fn host_author(
    authz: &AuthzContext,
    model_id: Option<&str>,
) -> Result<McpAuthorContext, CoreMcpError> {
    let trusted = authz.trusted_model_id();
    let model_id =
        resolve_operator_label(trusted, model_id).map_err(|conflict| CoreMcpError::Tool {
            kind: McpToolErrorKind::InvalidInput,
            message: conflict.detail("model_id"),
        })?;
    Ok(McpAuthorContext {
        model_id,
        trusted_model_id: trusted.map(ToString::to_string),
        client_name: "host".into(),
        client_version: "0".into(),
        caller_self_perspective: None,
    })
}

fn find_tool_descriptor<'a>(
    descriptors: &'a [McpToolDescriptor],
    name: &str,
) -> Option<&'a McpToolDescriptor> {
    descriptors
        .iter()
        .find(|descriptor| tool_name_matches(descriptor.name, name))
}

fn tool_info_from_descriptor(descriptor: &McpToolDescriptor) -> CoreToolInfo {
    let annotations = descriptor.resolved_annotations().unwrap_or_default();
    CoreToolInfo {
        name: provider_safe_tool_name(descriptor.name),
        description: descriptor.description.to_string(),
        args_schema: descriptor.args_schema.clone(),
        output_schema: descriptor.output_schema.clone(),
        read_only: annotations.read_only,
        destructive: annotations.destructive,
        idempotent: annotations.idempotent,
        open_world: annotations.open_world,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::FutureExt as _;
    use proxima_core::{
        AuthPath, FlavorRegistry, McpTool, McpToolCtx, McpToolError, OwnerRef, StorageError, UserId,
    };

    /// A tool that resolves its operator label exactly the way the core
    /// authoring tools do, and answers with it.
    struct LabelEchoTool;

    impl McpTool for LabelEchoTool {
        const NAME: &'static str = "test_label_echo";
        const DESCRIPTION: &'static str = "echo the resolved operator label";
        const ANNOTATIONS: Option<proxima_core::McpToolAnnotations> = Some(
            proxima_core::McpToolAnnotations::new()
                .read_only(true)
                .open_world(false),
        );

        type Args = LabelEchoArgs;
        type Output = String;

        fn call(
            ctx: McpToolCtx,
            args: Self::Args,
        ) -> futures::future::BoxFuture<'static, Result<Self::Output, McpToolError>> {
            async move { proxima_core::operator_label(&ctx, args.model_id.as_deref()) }.boxed()
        }
    }

    /// Declares `model_id`, exactly as `core_derive` and `core_interpret`
    /// do — which is why the argument reaches the tool at all.
    #[derive(serde::Deserialize, schemars::JsonSchema)]
    struct LabelEchoArgs {
        #[serde(default)]
        model_id: Option<String>,
    }

    #[derive(Clone)]
    struct MarkerService(&'static str);

    struct MarkerTool;

    impl McpTool for MarkerTool {
        const NAME: &'static str = "test_marker";
        const DESCRIPTION: &'static str = "test marker";
        const ANNOTATIONS: Option<proxima_core::McpToolAnnotations> = Some(
            proxima_core::McpToolAnnotations::new()
                .read_only(true)
                .open_world(false),
        );

        type Args = serde_json::Value;
        type Output = String;

        fn call(
            ctx: McpToolCtx,
            _args: Self::Args,
        ) -> futures::future::BoxFuture<'static, Result<Self::Output, McpToolError>> {
            async move {
                Ok(ctx
                    .service::<MarkerService>()
                    .expect("boot service must survive handler conversion")
                    .0
                    .to_string())
            }
            .boxed()
        }
    }

    fn marker_auth() -> McpAuthContext {
        let owner = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));
        let authz = proxima_core::AuthzContext::single_owner(&owner, AuthPath::HostBearer);
        McpAuthContext { owner, authz }
    }

    fn trusted_authz(owner: OwnerRef) -> AuthzContext {
        proxima_core::AuthzContext::single_owner(&owner, AuthPath::HostBearer)
            .with_trusted_model_id("acme/runner-v3")
            .expect("a well-formed runner id binds")
    }

    fn untrusted_authz(owner: OwnerRef) -> AuthzContext {
        proxima_core::AuthzContext::single_owner(&owner, AuthPath::HostBearer)
    }

    /// The embedded host API takes its own `model_id`; the bound identity
    /// still wins, and the host is told rather than silently overridden.
    #[test]
    fn host_author_applies_the_same_precedence_as_the_transports() {
        let owner = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));

        let bound = host_author(&trusted_authz(owner), None).expect("no claim, no conflict");
        assert_eq!(bound.model_id, "acme/runner-v3");
        assert_eq!(bound.trusted_model_id.as_deref(), Some("acme/runner-v3"));

        let agreeing = host_author(&trusted_authz(owner), Some("  acme/runner-v3  "))
            .expect("an agreeing claim is not a conflict");
        assert_eq!(agreeing.model_id, "acme/runner-v3");

        let unbound = host_author(&untrusted_authz(owner), Some("host/label"))
            .expect("no binding, the host label stands");
        assert_eq!(unbound.model_id, "host/label");
        assert_eq!(unbound.trusted_model_id, None);

        let unattributed =
            host_author(&untrusted_authz(owner), None).expect("no binding, no claim");
        assert_eq!(unattributed.model_id, "unknown");
        assert_eq!(unattributed.trusted_model_id, None);
    }

    #[test]
    fn host_author_refuses_a_model_id_that_differs_from_the_bound_identity() {
        let owner = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));

        let err = host_author(&trusted_authz(owner), Some("openai/gpt-9"))
            .expect_err("a host may not relabel an authenticated runner");

        assert_eq!(err.kind(), CoreMcpErrorKind::InvalidInput);
        let CoreMcpError::Tool { message, .. } = err else {
            panic!("a bad reserved argument is a tool input error");
        };
        assert!(message.contains("model_id"), "{message}");
        assert!(message.contains("openai/gpt-9"), "{message}");
        assert!(message.contains("acme/runner-v3"), "{message}");
    }

    /// Regression: `call_core_tool` hands `args` to the tool host verbatim,
    /// and nothing on this path strips a `model_id` out of them. The guard
    /// that catches it lives in core's `operator_label`, and this proves it
    /// is reached through the embedded host API — not only through a
    /// transport edge.
    #[tokio::test]
    async fn a_model_id_argument_that_differs_from_the_bound_identity_is_refused() {
        let mut registry = FlavorRegistry::new();
        registry.add_mcp_tool_or_panic_for_tests::<LabelEchoTool>("test");
        let registry = Arc::new(registry.freeze_or_panic_for_tests());
        let tools = CoreMcpTools::new(
            registry.clone(),
            Arc::new(proxima_core::Engine::new((*registry).clone())),
            FlavorServices::default(),
        );
        let owner = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));

        let err = tools
            .call_core_tool(
                trusted_authz(owner),
                owner,
                None,
                "test_label_echo",
                serde_json::json!({ "model_id": "openai/gpt-9" }),
            )
            .await
            .expect_err("an argument may not relabel an authenticated runner");
        assert_eq!(err.kind(), CoreMcpErrorKind::InvalidInput);
        let CoreMcpError::Tool { message, .. } = err else {
            panic!("a bad reserved argument is a tool input error");
        };
        assert!(
            message.contains("authenticated token already binds"),
            "refused for the binding, not for being an unexpected field: {message}"
        );

        let agreeing = tools
            .call_core_tool(
                trusted_authz(owner),
                owner,
                None,
                "test_label_echo",
                serde_json::json!({ "model_id": "acme/runner-v3" }),
            )
            .await
            .expect("an agreeing argument is accepted");
        assert_eq!(agreeing, serde_json::json!("acme/runner-v3"));

        let absent = tools
            .call_core_tool(
                trusted_authz(owner),
                owner,
                None,
                "test_label_echo",
                serde_json::json!({}),
            )
            .await
            .expect("an omitted argument records the bound identity");
        assert_eq!(absent, serde_json::json!("acme/runner-v3"));
    }

    #[tokio::test]
    async fn into_dynamic_handler_preserves_boot_services_in_per_call_context() {
        let mut registry = FlavorRegistry::new();
        registry.add_mcp_tool_or_panic_for_tests::<MarkerTool>("test");
        let registry = Arc::new(registry.freeze_or_panic_for_tests());
        let tools = CoreMcpTools::new(
            registry.clone(),
            Arc::new(proxima_core::Engine::new((*registry).clone())),
            FlavorServices::with(MarkerService("boot-wired")),
        );
        let handler = tools.into_dynamic_handler();
        let auth = marker_auth();
        let author = McpAuthorContext {
            model_id: "marker-test".into(),
            trusted_model_id: None,
            client_name: "test".into(),
            client_version: "1".into(),
            caller_self_perspective: None,
        };

        let ctx = handler
            .server
            .ctx_for(author.clone(), &auth)
            .expect("no binding, no conflict");
        assert_eq!(
            ctx.service::<MarkerService>().as_deref().map(|v| v.0),
            Some("boot-wired")
        );

        let answer = handler
            .server
            .call_tool("test_marker", serde_json::json!({}), author, Some(auth))
            .await
            .expect("authorized marker call");
        assert_eq!(answer, serde_json::json!("boot-wired"));

        let denied = handler
            .server
            .call_tool(
                "test_marker",
                serde_json::json!({}),
                McpAuthorContext {
                    model_id: "marker-test".into(),
                    trusted_model_id: None,
                    client_name: "test".into(),
                    client_version: "1".into(),
                    caller_self_perspective: None,
                },
                None,
            )
            .await;
        assert!(matches!(
            denied,
            Err(ToolInvocationError::NotAuthorized(name)) if name == "test_marker"
        ));
    }

    #[test]
    fn facade_error_mapping_preserves_invalid_input_kind() {
        let err = CoreMcpError::from_invocation_error(ToolInvocationError::Tool(
            McpToolError::InvalidInput("bad params".into()),
        ));

        assert_eq!(err.kind(), CoreMcpErrorKind::InvalidInput);
        assert!(matches!(
            err,
            CoreMcpError::Tool {
                kind: McpToolErrorKind::InvalidInput,
                ..
            }
        ));
    }

    #[test]
    fn facade_error_mapping_preserves_internal_storage_kind() {
        let err = CoreMcpError::from_invocation_error(ToolInvocationError::Tool(
            McpToolError::Storage(StorageError::Internal("boom".into())),
        ));

        assert_eq!(err.kind(), CoreMcpErrorKind::Internal);
        assert!(matches!(
            err,
            CoreMcpError::Tool {
                kind: McpToolErrorKind::Internal,
                ..
            }
        ));
    }

    #[test]
    fn facade_tool_lookup_accepts_canonical_and_provider_safe_names() {
        fn call(
            _ctx: McpToolCtx,
            _args: serde_json::Value,
        ) -> futures::future::BoxFuture<'static, Result<serde_json::Value, McpToolError>> {
            async { Ok(serde_json::json!({})) }.boxed()
        }

        let descriptors = vec![McpToolDescriptor {
            name: "provider/slashed_name",
            description: "test",
            origin: proxima_core::McpToolOrigin::Flavor("provider".into()),
            produces_schema_ids: &[],
            args_schema: serde_json::json!({ "type": "object" }),
            output_schema: serde_json::json!({ "type": "object" }),
            action_arg_specs: &[],
            argv_action_specs: &[],
            annotations: None,
            audience: proxima_core::McpToolAudience::Shared,
            call: &call,
        }];

        assert_eq!(
            find_tool_descriptor(&descriptors, "provider/slashed_name").map(|tool| tool.name),
            Some("provider/slashed_name")
        );
        assert_eq!(
            find_tool_descriptor(&descriptors, "provider_slashed_name").map(|tool| tool.name),
            Some("provider/slashed_name")
        );
    }
}
