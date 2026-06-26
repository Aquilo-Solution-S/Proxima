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
    McpToolAnnotations, McpToolError, McpToolErrorKind, all_core_resources, core_tool_annotations,
    provider_safe_tool_name, tool_name_matches,
};
use proxima_core::{McpAuthorContext, MemoryId};
use rmcp::ServerHandler;
use rmcp::model::{
    AnnotateAble, CallToolRequestParams, CallToolResult, Content, ErrorData, Implementation,
    InitializeRequestParams, InitializeResult, ListResourceTemplatesResult, ListResourcesResult,
    ListToolsResult, PaginatedRequestParams, RawResource, RawResourceTemplate,
    ReadResourceRequestParams, ReadResourceResult, ResourceContents, ServerCapabilities,
    ServerInfo, Tool, ToolAnnotations,
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
        let advertised_tools = self.advertised_tool_ids(scope);
        let advertised_resources = advertised_resource_scope_keys(scope);
        let mut info = self.get_info();
        let instructions = selfdoc::build_instructions(&advertised_tools, &advertised_resources);
        if !instructions.is_empty() {
            info.instructions = Some(instructions);
        }
        std::future::ready(Ok(info))
    }

    fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListResourcesResult, ErrorData>> + MaybeSendFuture + '_ {
        let auth = auth_context(&context);
        let scope = auth.as_ref().map(|ctx| &ctx.authz.capabilities.tool_scope);
        let resource = RawResource {
            title: Some(selfdoc::HOW_TO_TITLE.to_string()),
            description: Some(selfdoc::HOW_TO_DESCRIPTION.to_string()),
            mime_type: Some(selfdoc::HOW_TO_MIME.to_string()),
            ..RawResource::new(selfdoc::HOW_TO_URI, selfdoc::HOW_TO_NAME)
        }
        .no_annotation();
        let mut resources = vec![resource];
        resources.extend(
            all_core_resources()
                .filter(|resource| {
                    !resource.is_template && resource_scope_allows(scope, resource.scope_key)
                })
                .map(raw_resource_from_meta),
        );
        std::future::ready(Ok(ListResourcesResult {
            resources,
            ..Default::default()
        }))
    }

    fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListResourceTemplatesResult, ErrorData>> + MaybeSendFuture + '_
    {
        let auth = auth_context(&context);
        let scope = auth.as_ref().map(|ctx| &ctx.authz.capabilities.tool_scope);
        let resource_templates = all_core_resources()
            .filter(|resource| {
                resource.is_template && resource_scope_allows(scope, resource.scope_key)
            })
            .map(raw_resource_template_from_meta)
            .collect();
        std::future::ready(Ok(ListResourceTemplatesResult {
            resource_templates,
            ..Default::default()
        }))
    }

    fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ReadResourceResult, ErrorData>> + MaybeSendFuture + '_ {
        let uri = request.uri;
        let auth = auth_context(&context);
        let server = self.server.clone();
        async move {
            if uri == selfdoc::HOW_TO_URI {
                let scope = auth.as_ref().map(|ctx| &ctx.authz.capabilities.tool_scope);
                let advertised = server
                    .registry()
                    .list_mcp_tools()
                    .iter()
                    .filter(|descriptor| scope_allows(scope, descriptor.name))
                    .map(|descriptor| descriptor.name)
                    .collect();
                let advertised_resources = advertised_resource_scope_keys(scope);
                let body = selfdoc::how_to_markdown(&advertised, &advertised_resources);
                return Ok(ReadResourceResult::new(vec![
                    ResourceContents::text(body, selfdoc::HOW_TO_URI)
                        .with_mime_type(selfdoc::HOW_TO_MIME),
                ]));
            }
            if !uri.starts_with("proxima://") {
                return Err(ErrorData::resource_not_found(
                    format!("unknown resource {uri}"),
                    None,
                ));
            }
            let author = author_from_ctx(auth.as_ref());
            let value = server
                .read_resource(&uri, author, auth)
                .await
                .map_err(resource_invocation_error_to_error_data)?;
            let text = serde_json::to_string(&value).map_err(generic_internal_error)?;
            Ok(ReadResourceResult::new(vec![
                ResourceContents::text(text, uri).with_mime_type("application/json"),
            ]))
        }
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
            let mut args = request
                .arguments
                .map_or_else(|| serde_json::json!({}), serde_json::Value::Object);
            let author = author_from_args(&args, auth.as_ref())?;
            strip_call_context_args(&mut args);
            let output = server
                .call_tool(&canonical_name, args, author, auth)
                .await
                .map_err(tool_invocation_error_to_error_data)?;
            let text = serde_json::to_string(&output).map_err(generic_internal_error)?;
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

fn resource_invocation_error_to_error_data(err: ToolInvocationError) -> ErrorData {
    match err {
        ToolInvocationError::NotAuthorized(name) => ErrorData::invalid_request(
            format!("resource {name} not authorized for this MCP token"),
            None,
        ),
        ToolInvocationError::ToolNotFound(name) => {
            ErrorData::resource_not_found(format!("unknown resource: {name}"), None)
        }
        ToolInvocationError::Tool(inner) => mcp_tool_error_to_error_data(&inner),
    }
}

/// Classify an [`McpToolError`] by JSON-RPC code: caller-input faults →
/// `invalid_params` (-32602); well-formed-but-illegal requests →
/// `invalid_request` (-32600); infrastructure faults → `internal_error`
/// (-32603).
fn mcp_tool_error_to_error_data(err: &McpToolError) -> ErrorData {
    match err.kind() {
        McpToolErrorKind::InvalidInput => ErrorData::invalid_params(err.client_message(), None),
        McpToolErrorKind::InvalidRequest => ErrorData::invalid_request(err.client_message(), None),
        McpToolErrorKind::Internal => generic_internal_error(err),
    }
}

fn generic_internal_error(err: impl std::fmt::Display) -> ErrorData {
    tracing::error!(error = %err, "mcp internal error");
    ErrorData::internal_error("internal server error", None)
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

fn raw_resource_from_meta(meta: &proxima_core::CoreResourceMeta) -> rmcp::model::Resource {
    RawResource::new(static_resource_uri(meta.uri_template), meta.name)
        .with_title(meta.title)
        .with_description(meta.description)
        .with_mime_type("application/json")
        .no_annotation()
}

fn raw_resource_template_from_meta(
    meta: &proxima_core::CoreResourceMeta,
) -> rmcp::model::ResourceTemplate {
    RawResourceTemplate::new(meta.uri_template, meta.name)
        .with_title(meta.title)
        .with_description(meta.description)
        .with_mime_type("application/json")
        .no_annotation()
}

fn static_resource_uri(uri_template: &str) -> String {
    uri_template
        .split_once('{')
        .map_or(uri_template, |(uri, _)| uri)
        .to_string()
}

fn resource_scope_allows(scope: Option<&ToolScope>, scope_key: &str) -> bool {
    match scope {
        Some(scope) => scope.allows(scope_key),
        None => UNAUTHENTICATED_SCOPE_ALLOWS,
    }
}

fn advertised_resource_scope_keys(scope: Option<&ToolScope>) -> BTreeSet<&'static str> {
    all_core_resources()
        .filter(|resource| resource_scope_allows(scope, resource.scope_key))
        .map(|resource| resource.scope_key)
        .collect()
}

fn author_from_ctx(auth: Option<&McpAuthContext>) -> McpAuthorContext {
    McpAuthorContext {
        model_id: auth
            .and_then(|ctx| ctx.model_id.as_deref())
            .unwrap_or("unknown")
            .to_string(),
        client_name: "unknown".into(),
        client_version: "0".into(),
        personality_instance_id: None,
        caller_self_perspective: None,
    }
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
    let Some((field, raw)) = [
        "_proxima_caller_self_perspective",
        "caller_self_perspective",
        "current_root_perspective_memory_id",
    ]
    .into_iter()
    .find_map(|field| args.get(field).map(|raw| (field, raw))) else {
        return Ok(None);
    };
    let Some(raw) = raw.as_str() else {
        return Err(ErrorData::invalid_params(
            format!("{field} must be a UUID string"),
            None,
        ));
    };
    let id = uuid::Uuid::parse_str(raw).map_err(|err| {
        ErrorData::invalid_params(format!("{field} must be a valid UUID: {err}"), None)
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

    #[test]
    fn caller_self_perspective_metadata_errors_are_invalid_params() {
        let args = serde_json::json!({
            "caller_self_perspective": 42,
        });
        let err = author_from_args(&args, None).expect_err("non-string metadata fails");
        assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
        assert!(
            err.message.contains("caller_self_perspective"),
            "message: {}",
            err.message
        );

        let args = serde_json::json!({
            "current_root_perspective_memory_id": "not-a-uuid",
        });
        let err = author_from_args(&args, None).expect_err("invalid uuid metadata fails");
        assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
        assert!(
            err.message.contains("current_root_perspective_memory_id"),
            "message: {}",
            err.message
        );
    }

    #[test]
    fn internal_tool_errors_are_redacted_for_clients() {
        let err = mcp_tool_error_to_error_data(&McpToolError::Other("storage DSN leaked".into()));
        assert_eq!(err.code, rmcp::model::ErrorCode::INTERNAL_ERROR);
        assert_eq!(err.message, "internal server error");
    }

    #[test]
    fn tool_scope_denials_remain_invalid_request() {
        let err = tool_invocation_error_to_error_data(
            McpToolError::NotAuthorized("core_wake:set".into()).into(),
        );

        assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_REQUEST);
        assert_eq!(
            err.message,
            "tool core_wake:set not authorized for this MCP token"
        );
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

        // Grouped fact dispatcher is conservative: tombstone is destructive,
        // but its writes converge.
        let fact = core_tool_annotations("core_fact").expect("fact dispatcher");
        assert_eq!(fact.read_only, Some(false));
        assert_eq!(fact.destructive, Some(true));
        assert_eq!(fact.idempotent, Some(true));

        // Personality and wake dispatchers aggregate destructive and
        // non-replay-safe actions.
        let personality =
            core_tool_annotations("core_personality").expect("personality dispatcher");
        assert_eq!(personality.destructive, Some(true));
        assert_eq!(personality.idempotent, Some(false));
        let wake = core_tool_annotations("core_wake").expect("wake dispatcher");
        assert_eq!(wake.destructive, Some(true));
        assert_eq!(wake.idempotent, Some(false));

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
