//! rmcp 1.6 dynamic tool projection.
//!
//! The SDK exposes dynamic tools through direct
//! `ServerHandler::list_tools` / `call_tool` overrides. This adapter
//! projects the frozen build-time `FlavorRegistry` tool descriptors
//! into MCP tool metadata at request time.

use std::borrow::Cow;
use std::collections::BTreeSet;
use std::sync::Arc;

use proxima_core::mcp::{
    McpToolAnnotations, McpToolError, McpToolErrorKind, core_tool_annotations,
    provider_safe_tool_name, tool_name_matches,
};
use proxima_core::{McpAuthorContext, MemoryId};
use rmcp::ServerHandler;
use rmcp::model::{
    AnnotateAble, CallToolRequestParams, CallToolResult, Content, ErrorData, Implementation,
    InitializeRequestParams, InitializeResult, ListResourcesResult, ListToolsResult,
    PaginatedRequestParams, RawResource, ReadResourceRequestParams, ReadResourceResult,
    ResourceContents, ServerCapabilities, ServerInfo, Tool, ToolAnnotations,
};
use rmcp::service::{MaybeSendFuture, RequestContext, RoleServer};

use crate::selfdoc;

use crate::auth::McpAuthContext;
use crate::server::{McpToolHost, ToolInvocationError};
use proxima_core::ToolScope;

#[derive(Clone, Debug)]
pub struct DynamicHandler {
    pub server: McpToolHost,
}

impl DynamicHandler {
    /// Canonical ids of the tools advertised to a caller with `scope`. Same
    /// filter `list_tools` applies, so self-documentation never references a
    /// tool the caller cannot see.
    fn advertised_tool_ids(&self, scope: Option<&ToolScope>) -> BTreeSet<&'static str> {
        self.server
            .registry()
            .list_mcp_tools()
            .iter()
            .filter(|descriptor| scope_allows(scope, descriptor.name))
            .map(|descriptor| descriptor.name)
            .collect()
    }
}

impl ServerHandler for DynamicHandler {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.capabilities = ServerCapabilities::builder()
            .enable_tools()
            .enable_resources()
            .build();
        info.server_info = Implementation::from_build_env();
        info
    }

    /// Override `initialize` so the `instructions` returned at the handshake
    /// are generated from the caller's *resolved* tool scope (deployment
    /// profile ∩ token capabilities) — the same scope `list_tools` advertises.
    /// A `memory`-profile deployment thus omits guidance for tools it does not
    /// expose. Mirrors the SDK default's `set_peer_info` bookkeeping.
    fn initialize(
        &self,
        request: InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<InitializeResult, ErrorData>> + MaybeSendFuture + '_ {
        if context.peer.peer_info().is_none() {
            context.peer.set_peer_info(request);
        }
        let auth = auth_context(&context);
        let scope = auth.as_ref().map(|ctx| &ctx.authz.capabilities.tool_scope);
        let advertised = self.advertised_tool_ids(scope);
        let mut info = self.get_info();
        let instructions = selfdoc::build_instructions(&advertised);
        if !instructions.is_empty() {
            info.instructions = Some(instructions);
        }
        std::future::ready(Ok(info))
    }

    fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListResourcesResult, ErrorData>> + MaybeSendFuture + '_ {
        let resource = RawResource {
            title: Some(selfdoc::HOW_TO_TITLE.to_string()),
            description: Some(selfdoc::HOW_TO_DESCRIPTION.to_string()),
            mime_type: Some(selfdoc::HOW_TO_MIME.to_string()),
            ..RawResource::new(selfdoc::HOW_TO_URI, selfdoc::HOW_TO_NAME)
        }
        .no_annotation();
        std::future::ready(Ok(ListResourcesResult {
            resources: vec![resource],
            ..Default::default()
        }))
    }

    fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ReadResourceResult, ErrorData>> + MaybeSendFuture + '_ {
        let result = if request.uri == selfdoc::HOW_TO_URI {
            let auth = auth_context(&context);
            let scope = auth.as_ref().map(|ctx| &ctx.authz.capabilities.tool_scope);
            let advertised = self.advertised_tool_ids(scope);
            let body = selfdoc::how_to_markdown(&advertised);
            Ok(ReadResourceResult::new(vec![
                ResourceContents::text(body, selfdoc::HOW_TO_URI)
                    .with_mime_type(selfdoc::HOW_TO_MIME),
            ]))
        } else {
            Err(ErrorData::resource_not_found(
                format!("unknown resource {}", request.uri),
                None,
            ))
        };
        std::future::ready(result)
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, ErrorData>> + MaybeSendFuture + '_ {
        let auth = auth_context(&context);
        let scope = auth.as_ref().map(|ctx| &ctx.authz.capabilities.tool_scope);
        let tools: Vec<Tool> = self
            .server
            .registry()
            .list_mcp_tools()
            .iter()
            .filter(|descriptor| scope_allows(scope, descriptor.name))
            .map(|descriptor| {
                let tool = Tool::new(
                    Cow::Owned(provider_safe_tool_name(descriptor.name)),
                    Cow::Borrowed(descriptor.description),
                    Arc::new(rmcp::model::object(descriptor.args_schema.clone())),
                );
                match core_tool_annotations(descriptor.name) {
                    Some(annotations) => tool.annotate(to_rmcp_annotations(annotations)),
                    None => tool,
                }
            })
            .collect();
        std::future::ready(Ok(ListToolsResult {
            tools,
            ..Default::default()
        }))
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.server
            .registry()
            .list_mcp_tools()
            .iter()
            .find(|descriptor| tool_name_matches(descriptor.name, name))
            .map(|descriptor| {
                let tool = Tool::new(
                    Cow::Owned(provider_safe_tool_name(descriptor.name)),
                    Cow::Borrowed(descriptor.description),
                    Arc::new(rmcp::model::object(descriptor.args_schema.clone())),
                );
                match core_tool_annotations(descriptor.name) {
                    Some(annotations) => tool.annotate(to_rmcp_annotations(annotations)),
                    None => tool,
                }
            })
    }

    fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CallToolResult, ErrorData>> + MaybeSendFuture + '_ {
        let server = self.server.clone();
        let auth = auth_context(&context);
        async move {
            let request_name = request.name.to_string();
            let canonical_name =
                canonical_tool_name(&server, &request_name).unwrap_or_else(|| request_name.clone());
            if !scope_allows(
                auth.as_ref().map(|ctx| &ctx.authz.capabilities.tool_scope),
                &canonical_name,
            ) {
                return Err(ErrorData::invalid_request(
                    format!("tool {} not authorized for this MCP token", request.name),
                    None,
                ));
            }
            let mut args = request
                .arguments
                .map_or_else(|| serde_json::json!({}), serde_json::Value::Object);
            let author = author_from_args(&args, auth.as_ref())?;
            strip_call_context_args(&mut args);
            let output = server
                .call_tool(&canonical_name, args, author, auth)
                .await
                .map_err(tool_invocation_error_to_error_data)?;
            let text = serde_json::to_string(&output)
                .map_err(|err| ErrorData::internal_error(err.to_string(), None))?;
            let mut result = CallToolResult::success(vec![Content::text(text)]);
            result.structured_content = Some(output);
            Ok(result)
        }
    }
}

/// Map a tool-invocation failure to a typed JSON-RPC error so external
/// agents can tell bad input from a server fault, instead of every failure
/// collapsing to `internal_error` (-32603).
fn tool_invocation_error_to_error_data(err: ToolInvocationError) -> ErrorData {
    match err {
        ToolInvocationError::NotAuthorized(name) => ErrorData::invalid_request(
            format!("tool {name} not authorized for this MCP token"),
            None,
        ),
        ToolInvocationError::ToolNotFound(name) => {
            ErrorData::invalid_params(format!("unknown tool: {name}"), None)
        }
        ToolInvocationError::Tool(inner) => mcp_tool_error_to_error_data(&inner),
    }
}

/// Classify an [`McpToolError`] by JSON-RPC code: caller-input faults →
/// `invalid_params` (-32602); well-formed-but-illegal requests →
/// `invalid_request` (-32600); infrastructure faults → `internal_error`
/// (-32603).
fn mcp_tool_error_to_error_data(err: &McpToolError) -> ErrorData {
    let msg = err.to_string();
    match err.kind() {
        McpToolErrorKind::InvalidInput => ErrorData::invalid_params(msg, None),
        McpToolErrorKind::InvalidRequest => ErrorData::invalid_request(msg, None),
        McpToolErrorKind::Internal => ErrorData::internal_error(msg, None),
    }
}

fn to_rmcp_annotations(annotations: McpToolAnnotations) -> ToolAnnotations {
    let mut hints = ToolAnnotations::new();
    if let Some(read_only) = annotations.read_only {
        hints = hints.read_only(read_only);
    }
    if let Some(destructive) = annotations.destructive {
        hints = hints.destructive(destructive);
    }
    if let Some(idempotent) = annotations.idempotent {
        hints = hints.idempotent(idempotent);
    }
    if let Some(open_world) = annotations.open_world {
        hints = hints.open_world(open_world);
    }
    hints
}

fn canonical_tool_name(server: &McpToolHost, request_name: &str) -> Option<String> {
    server
        .registry()
        .list_mcp_tools()
        .iter()
        .find(|descriptor| tool_name_matches(descriptor.name, request_name))
        .map(|descriptor| descriptor.name.to_string())
}

/// Resolve the token scope from the request auth context. Returns `None`
/// when no token-bearing layer ran ahead of rmcp (direct handler tests).
/// Local master tokens carry all-tools scope; host-bearer tokens carry
/// the host-provided scope.
///
/// rmcp's `StreamableHttpService` injects [`http::request::Parts`] into
/// the rmcp request extensions, and our `mcp_auth_layer` inserts
/// `McpAuthContext` into the axum request extensions before nesting the
/// rmcp service. The two extension stores are different — we follow the
/// documented bridge.
fn auth_context(context: &RequestContext<RoleServer>) -> Option<McpAuthContext> {
    let parts = context.extensions.get::<http::request::Parts>()?;
    let ctx = parts.extensions.get::<McpAuthContext>()?;
    Some(ctx.clone())
}

fn scope_allows(scope: Option<&ToolScope>, name: &str) -> bool {
    match scope {
        Some(scope) => scope.allows_group_advertisement(name),
        // No auth context bound to the request. In release builds this
        // means the request bypassed `mcp_auth_layer` (which 401s before
        // dispatch) — fail closed rather than expose the full tool
        // surface. Direct handler tests run without the layer, so the
        // test arm stays permissive; it is compiled out of release.
        None => UNAUTHENTICATED_SCOPE_ALLOWS,
    }
}

/// Whether a request that carries no bound auth context may see or call
/// a tool. Release: `false` (fail closed — a missing `mcp_auth_layer` is
/// a regression, not a no-auth grant). Test: `true` (direct-handler
/// ergonomics). The split makes the permissive arm un-shippable.
#[cfg(not(test))]
const UNAUTHENTICATED_SCOPE_ALLOWS: bool = false;
#[cfg(test)]
const UNAUTHENTICATED_SCOPE_ALLOWS: bool = true;

fn author_from_args(
    args: &serde_json::Value,
    auth: Option<&McpAuthContext>,
) -> Result<McpAuthorContext, ErrorData> {
    let model_id = args
        .get("model_id")
        .and_then(serde_json::Value::as_str)
        .or_else(|| auth.and_then(|ctx| ctx.model_id.as_deref()))
        .unwrap_or("unknown")
        .to_string();
    let caller_self_perspective = caller_self_perspective_from_args(args)?;
    Ok(McpAuthorContext {
        model_id,
        client_name: "unknown".into(),
        client_version: "0".into(),
        personality_instance_id: None,
        caller_self_perspective,
    })
}

fn caller_self_perspective_from_args(
    args: &serde_json::Value,
) -> Result<Option<MemoryId>, ErrorData> {
    let Some(raw) = args
        .get("_proxima_caller_self_perspective")
        .or_else(|| args.get("caller_self_perspective"))
        .or_else(|| args.get("current_root_perspective_memory_id"))
    else {
        return Ok(None);
    };
    let Some(raw) = raw.as_str() else {
        return Err(ErrorData::internal_error(
            "caller self perspective metadata must be a UUID string",
            None,
        ));
    };
    let id = uuid::Uuid::parse_str(raw).map_err(|err| {
        ErrorData::internal_error(format!("invalid caller self perspective UUID: {err}"), None)
    })?;
    Ok(Some(MemoryId::new(id)))
}

fn strip_call_context_args(args: &mut serde_json::Value) {
    let Some(obj) = args.as_object_mut() else {
        return;
    };
    obj.remove("_proxima_caller_self_perspective");
    obj.remove("caller_self_perspective");
    obj.remove("current_root_perspective_memory_id");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn author_from_args_extracts_caller_self_perspective() {
        let self_id = uuid::Uuid::now_v7();
        let args = serde_json::json!({
            "model_id": "test-model",
            "_proxima_caller_self_perspective": self_id.to_string(),
        });

        let author = author_from_args(&args, None).expect("author context");

        assert_eq!(author.model_id, "test-model");
        assert_eq!(
            author.caller_self_perspective.map(MemoryId::into_inner),
            Some(self_id)
        );
    }

    #[test]
    fn strip_call_context_args_removes_reserved_metadata() {
        let mut args = serde_json::json!({
            "payload": {},
            "_proxima_caller_self_perspective": uuid::Uuid::now_v7().to_string(),
            "caller_self_perspective": uuid::Uuid::now_v7().to_string(),
            "current_root_perspective_memory_id": uuid::Uuid::now_v7().to_string(),
        });

        strip_call_context_args(&mut args);

        assert!(args.get("payload").is_some());
        assert!(args.get("_proxima_caller_self_perspective").is_none());
        assert!(args.get("caller_self_perspective").is_none());
        assert!(args.get("current_root_perspective_memory_id").is_none());
    }

    // Completeness gate: every `core_` tool the substrate registers must
    // carry MCP annotations, so a newly-added core tool cannot silently ship
    // with unset hints (client defaults: not-read-only, destructive,
    // open-world — wrong for this closed substrate).
    #[test]
    fn every_core_tool_is_annotated() {
        let registry = proxima_core::FlavorRegistry::new().freeze();
        for descriptor in registry.list_mcp_tools() {
            if descriptor.name.starts_with("core_") {
                assert!(
                    core_tool_annotations(descriptor.name).is_some(),
                    "core tool {} has no MCP annotations — add it to core_tool_annotations",
                    descriptor.name
                );
            }
        }
    }

    #[test]
    fn core_tool_annotations_encode_expected_semantics() {
        // Closed substrate: open_world is always false.
        let read = core_tool_annotations("core_search_memories").expect("read tool");
        assert_eq!(read.read_only, Some(true));
        assert_eq!(read.open_world, Some(false));

        // Convergent additive write (required idempotency key).
        let derive = core_tool_annotations("core_derive").expect("write tool");
        assert_eq!(derive.read_only, Some(false));
        assert_eq!(derive.destructive, Some(false));
        assert_eq!(derive.idempotent, Some(true));

        // Additive write with an OPTIONAL idempotency key: identical args
        // without a key create a new Fact, so it is not replay-safe.
        let remember = core_tool_annotations("core_remember").expect("non-idempotent write");
        assert_eq!(remember.read_only, Some(false));
        assert_eq!(remember.destructive, Some(false));
        assert_eq!(remember.idempotent, Some(false));

        // Destructive write that converges (a second call is a no-op).
        let cleanup = core_tool_annotations("core_cleanup_facts").expect("destructive tool");
        assert_eq!(cleanup.read_only, Some(false));
        assert_eq!(cleanup.destructive, Some(true));
        assert_eq!(cleanup.idempotent, Some(true));

        // Destructive write that is NOT replay-safe (audit-Fact divergence).
        let tombstone =
            core_tool_annotations("core_tombstone_personality").expect("destructive tool");
        assert_eq!(tombstone.destructive, Some(true));
        assert_eq!(tombstone.idempotent, Some(false));

        // Create-new-each-call write is not replay-safe.
        let link = core_tool_annotations("core_link").expect("create tool");
        assert_eq!(link.idempotent, Some(false));

        // Grouped goal dispatcher aggregates mixed write actions, so it is
        // advertised as non-idempotent at tool level.
        let goal = core_tool_annotations("core_goal").expect("goal dispatcher");
        assert_eq!(goal.read_only, Some(false));
        assert_eq!(goal.destructive, Some(false));
        assert_eq!(goal.idempotent, Some(false));

        // Flavor-shipped / unknown tools get no substrate hints here.
        assert!(core_tool_annotations("company/upsert").is_none());
    }
}
