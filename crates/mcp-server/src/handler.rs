//! rmcp dynamic tool projection.
//!
//! The SDK exposes dynamic tools through direct
//! `ServerHandler::list_tools` / `call_tool` overrides. This adapter
//! projects the frozen build-time `FlavorRegistry` tool descriptors
//! into MCP tool metadata at request time.

use std::borrow::Cow;
use std::collections::BTreeSet;
use std::sync::Arc;

use proxima_core::mcp::{
    McpToolAnnotations, McpToolDescriptor, McpToolError, McpToolErrorKind, all_core_actions,
    all_core_resources, core_tool_annotations, provider_safe_tool_name, scope_permits_action,
    tool_name_matches,
};
use proxima_core::{AccessKind, McpAuthorContext, MemoryId};
use rmcp::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, ContentBlock, ErrorData, Implementation,
    InitializeRequestParams, InitializeResult, ListResourceTemplatesResult, ListResourcesResult,
    ListToolsResult, PaginatedRequestParams, ReadResourceRequestParams, ReadResourceResult,
    Resource, ResourceContents, ResourceTemplate, ServerCapabilities, ServerInfo, Tool,
    ToolAnnotations,
};
use rmcp::service::{MaybeSendFuture, RequestContext, RoleServer};

use crate::selfdoc;

/// Product name reported to MCP clients on `initialize`.
const SERVER_NAME: &str = "proxima";

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
    fn advertised_tool_ids(&self, auth: Option<&McpAuthContext>) -> BTreeSet<&'static str> {
        self.server
            .registry()
            .list_mcp_tools()
            .iter()
            .filter(|descriptor| tool_allowed_for_auth(auth, descriptor))
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
        // NOT `Implementation::from_build_env()`: those `env!` macros expand
        // against rmcp's own manifest, so every Proxima deployment introduced
        // itself as `rmcp 2.2.0` and no client or operator could tell which
        // release they were talking to.
        info.server_info = Implementation::new(SERVER_NAME, proxima_core::RELEASE_VERSION);
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
        let advertised_tools = self.advertised_tool_ids(auth.as_ref());
        let scope = auth.as_ref().map(|ctx| ctx.authz.tool_scope());
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
        let scope = auth.as_ref().map(|ctx| ctx.authz.tool_scope());
        let resource = Resource::new(selfdoc::HOW_TO_URI, selfdoc::HOW_TO_NAME)
            .with_title(selfdoc::HOW_TO_TITLE)
            .with_description(selfdoc::HOW_TO_DESCRIPTION)
            .with_mime_type(selfdoc::HOW_TO_MIME);
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
        let scope = auth.as_ref().map(|ctx| ctx.authz.tool_scope());
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
        let (client_name, client_version) = peer_implementation(&context);
        let server = self.server.clone();
        async move {
            if uri == selfdoc::HOW_TO_URI {
                let scope = auth.as_ref().map(|ctx| ctx.authz.tool_scope());
                let advertised = server
                    .registry()
                    .list_mcp_tools()
                    .iter()
                    .filter(|descriptor| tool_allowed_for_auth(auth.as_ref(), descriptor))
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
            let author = author_from_ctx(auth.as_ref(), &client_name, &client_version);
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
        let scope = auth.as_ref().map(|ctx| ctx.authz.tool_scope());
        let tools: Vec<Tool> = self
            .server
            .registry()
            .list_mcp_tools()
            .iter()
            .filter(|descriptor| tool_allowed_for_auth(auth.as_ref(), descriptor))
            .map(|descriptor| {
                let schema =
                    project_dispatcher_actions(&descriptor.args_schema, scope, descriptor.name);
                let tool = Tool::new(
                    Cow::Owned(provider_safe_tool_name(descriptor.name)),
                    Cow::Borrowed(descriptor.description),
                    Arc::new(rmcp::model::object(schema)),
                );
                match resolved_annotations(descriptor) {
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
                match resolved_annotations(descriptor) {
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
        let (client_name, client_version) = peer_implementation(&context);
        async move {
            let request_name = request.name.to_string();
            let canonical_name =
                canonical_tool_name(&server, &request_name).unwrap_or_else(|| request_name.clone());
            let mut args = request
                .arguments
                .map_or_else(|| serde_json::json!({}), serde_json::Value::Object);
            let author = author_from_args(&args, auth.as_ref(), &client_name, &client_version)?;
            strip_call_context_args(&mut args);
            reject_nul_in_args(&args)?;
            let scope = auth.as_ref().map(|ctx| ctx.authz.tool_scope().clone());
            let output = server
                .call_tool(&canonical_name, args, author, auth)
                .await
                .map_err(|err| tool_invocation_error_to_error_data(err, scope.as_ref()))?;
            let text = serde_json::to_string(&output).map_err(generic_internal_error)?;
            let mut result = CallToolResult::success(vec![ContentBlock::text(text)]);
            result.structured_content = Some(output);
            Ok(result)
        }
    }
}

/// Reject `NUL` anywhere in a tool's arguments before dispatch.
///
/// A Postgres `text` value cannot contain `U+0000` — the server answers
/// `invalid byte sequence for encoding "UTF8": 0x00` and aborts the
/// statement. JSON strings can carry it (a `\u0000` escape is well-formed), so a
/// caller could send one through any string argument of any tool, and it
/// arrived as `-32603 internal server error` plus an `ERROR`-level log
/// line: a client-driven input mistake reported, and alerted on, as a
/// server fault. That is the exact failure
/// [`tool_invocation_error_to_error_data`] exists to prevent, one layer
/// further out.
///
/// Rejected rather than stripped. Silently removing the byte would answer
/// a different query than the one asked, and the caller would have no way
/// to know.
///
/// Checked over the whole argument tree rather than per tool, because the
/// constraint is Postgres', not any one tool's: every string that reaches
/// storage has it, including arguments of tools not yet written.
fn reject_nul_in_args(args: &serde_json::Value) -> Result<(), ErrorData> {
    // An explicit worklist rather than recursion. `serde_json` already caps
    // parse depth at 127, so nothing that arrives here could exhaust the
    // stack — but that bound belongs to the parser, and this walk should
    // not depend on it.
    let mut stack = vec![args];
    while let Some(value) = stack.pop() {
        match value {
            serde_json::Value::String(text) => {
                if text.contains('\0') {
                    return Err(ErrorData::invalid_params(
                        "arguments must not contain NUL (U+0000)".to_string(),
                        None,
                    ));
                }
            }
            serde_json::Value::Array(items) => stack.extend(items.iter()),
            serde_json::Value::Object(map) => {
                for (key, item) in map {
                    if key.contains('\0') {
                        return Err(ErrorData::invalid_params(
                            "argument names must not contain NUL (U+0000)".to_string(),
                            None,
                        ));
                    }
                    stack.push(item);
                }
            }
            serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {
            }
        }
    }
    Ok(())
}

/// Map a tool-invocation failure to a typed JSON-RPC error so external
/// agents can tell bad input from a server fault, instead of every failure
/// collapsing to `internal_error` (-32603).
fn tool_invocation_error_to_error_data(
    err: ToolInvocationError,
    scope: Option<&ToolScope>,
) -> ErrorData {
    match err {
        ToolInvocationError::NotAuthorized(name) => {
            ErrorData::invalid_request(not_authorized_message(&name, scope), None)
        }
        ToolInvocationError::ToolNotFound(name) => {
            ErrorData::invalid_params(format!("unknown tool: {name}"), None)
        }
        ToolInvocationError::Tool(inner) => mcp_tool_error_to_error_data(&inner),
    }
}

/// Build the not-authorized message, enriched with the caller's still-allowed
/// actions on the denied dispatcher tool so an agent can immediately retry with
/// a permitted action instead of guessing. `name` is either a tool id or a
/// `tool:action` leaf.
fn not_authorized_message(name: &str, scope: Option<&ToolScope>) -> String {
    let tool = name.split_once(':').map_or(name, |(tool, _)| tool);
    let allowed: Vec<&str> = scope.map_or_else(Vec::new, |scope| {
        all_core_actions()
            .filter(|meta| meta.tool == tool)
            .filter(|meta| scope_permits_action(scope, tool, meta.action))
            .map(|meta| meta.action)
            .collect()
    });
    if allowed.is_empty() {
        format!("tool {name} not authorized for this MCP token")
    } else {
        format!(
            "tool {name} not authorized for this MCP token; allowed {tool} actions: {}",
            allowed.join(", ")
        )
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
        // A missing entity behind a well-formed resource URI is a
        // `resource_not_found` (-32002), unlike the tool path where the
        // same fault is an argument problem.
        ToolInvocationError::Tool(inner) if inner.kind() == McpToolErrorKind::NotFound => {
            ErrorData::resource_not_found(inner.client_message(), None)
        }
        ToolInvocationError::Tool(inner) => mcp_tool_error_to_error_data(&inner),
    }
}

/// Classify an [`McpToolError`] by JSON-RPC code: caller-input faults —
/// including references to missing entities — → `invalid_params` (-32602);
/// well-formed-but-illegal requests → `invalid_request` (-32600);
/// infrastructure faults → `internal_error` (-32603). Resource reads remap
/// `NotFound` before reaching here (see
/// [`resource_invocation_error_to_error_data`]).
fn mcp_tool_error_to_error_data(err: &McpToolError) -> ErrorData {
    match err.kind() {
        McpToolErrorKind::InvalidInput | McpToolErrorKind::NotFound => {
            ErrorData::invalid_params(err.client_message(), None)
        }
        McpToolErrorKind::InvalidRequest => ErrorData::invalid_request(err.client_message(), None),
        McpToolErrorKind::Internal => generic_internal_error(err),
    }
}

fn generic_internal_error(err: impl std::fmt::Display) -> ErrorData {
    tracing::error!(error = %err, "mcp internal error");
    ErrorData::internal_error("internal server error", None)
}

/// Narrow a dispatcher tool's advertised `action` enum and `x-proxima-actions`
/// to the actions a `Palette` scope permits, so `tools/list` never advertises
/// an action the caller cannot invoke. `All` (or absent) scopes and flat tools
/// are returned unchanged.
fn project_dispatcher_actions(
    schema: &serde_json::Value,
    scope: Option<&ToolScope>,
    tool: &str,
) -> serde_json::Value {
    let Some(scope_ref) = scope else {
        return schema.clone();
    };
    if !matches!(scope_ref, ToolScope::Palette(_)) {
        return schema.clone();
    }
    let Some(actions) = schema
        .get("x-proxima-actions")
        .and_then(serde_json::Value::as_object)
    else {
        return schema.clone();
    };
    let permitted: Vec<String> = actions
        .keys()
        .filter(|action| scope_permits_action(scope_ref, tool, action))
        .cloned()
        .collect();
    // Whole-tool grant (or exactly the full action set) needs no narrowing.
    if permitted.len() == actions.len() {
        return schema.clone();
    }
    let mut projected = schema.clone();
    let Some(map) = projected.as_object_mut() else {
        return schema.clone();
    };
    if let Some(actions) = map
        .get_mut("x-proxima-actions")
        .and_then(serde_json::Value::as_object_mut)
    {
        actions.retain(|action, _| permitted.iter().any(|permit| permit == action));
    }
    // The flattener sets `required = [discriminator]`; use it to find the enum.
    let discriminator = map
        .get("required")
        .and_then(serde_json::Value::as_array)
        .and_then(|items| items.first())
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    if let Some(discriminator) = discriminator
        && let Some(enum_values) = map
            .get_mut("properties")
            .and_then(serde_json::Value::as_object_mut)
            .and_then(|properties| properties.get_mut(&discriminator))
            .and_then(serde_json::Value::as_object_mut)
            .and_then(|property| property.get_mut("enum"))
            .and_then(serde_json::Value::as_array_mut)
    {
        enum_values.retain(|value| {
            value
                .as_str()
                .is_some_and(|s| permitted.iter().any(|p| p == s))
        });
    }
    projected
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

fn raw_resource_from_meta(meta: &proxima_core::CoreResourceMeta) -> Resource {
    Resource::new(static_resource_uri(meta.uri_template), meta.name)
        .with_title(meta.title)
        .with_description(meta.description)
        .with_mime_type("application/json")
}

fn raw_resource_template_from_meta(meta: &proxima_core::CoreResourceMeta) -> ResourceTemplate {
    ResourceTemplate::new(meta.uri_template, meta.name)
        .with_title(meta.title)
        .with_description(meta.description)
        .with_mime_type("application/json")
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

fn author_from_ctx(
    auth: Option<&McpAuthContext>,
    client_name: &str,
    client_version: &str,
) -> McpAuthorContext {
    McpAuthorContext {
        model_id: auth
            .and_then(|ctx| ctx.model_id.as_deref())
            .unwrap_or("unknown")
            .to_string(),
        client_name: client_name.to_string(),
        client_version: client_version.to_string(),
        caller_self_perspective: None,
    }
}

/// Client `(name, version)` from the initialize handshake's `client_info`,
/// recorded as operator provenance. Falls back to `("unknown", "0")` when the
/// peer info is absent (e.g. a request that never completed `initialize`).
fn peer_implementation(context: &RequestContext<RoleServer>) -> (String, String) {
    context.peer.peer_info().map_or_else(
        || ("unknown".to_string(), "0".to_string()),
        |info| {
            (
                info.client_info.name.clone(),
                info.client_info.version.clone(),
            )
        },
    )
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
/// Host-bearer tokens carry the host-provided scope.
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

/// Whether this caller may see and call `descriptor`.
///
/// Takes the descriptor rather than the name because the name alone
/// cannot answer the read-vs-write question for a flavor tool: the only
/// place a flavor's behaviour is recorded is its own `ANNOTATIONS`, which
/// lives on the descriptor. Every call site already had one.
fn tool_allowed_for_auth(auth: Option<&McpAuthContext>, descriptor: &McpToolDescriptor) -> bool {
    let scope = auth.map(|ctx| ctx.authz.tool_scope());
    scope_allows(scope, descriptor.name) && owner_role_allows_tool(auth, descriptor)
}

fn owner_role_allows_tool(auth: Option<&McpAuthContext>, descriptor: &McpToolDescriptor) -> bool {
    let Some(ctx) = auth else {
        return UNAUTHENTICATED_SCOPE_ALLOWS;
    };
    if descriptor_is_read_only(descriptor) {
        ctx.authz.may_read(&ctx.owner, AccessKind::Fact)
    } else {
        ctx.authz.may_write(&ctx.owner, AccessKind::Fact)
    }
}

/// The tool's own declaration, then the core manifest.
///
/// One resolution order, used by everything in this adapter that needs to
/// know what a tool does: the visibility gate below, the `tools/list`
/// projection, and `get_tool`. It matches
/// `ScopeGateBehavior::enforce_owner_role`, and it is the order
/// `FlavorRegistry::try_freeze` guarantees resolves to `Some` for every
/// registered tool.
fn resolved_annotations(descriptor: &McpToolDescriptor) -> Option<McpToolAnnotations> {
    descriptor
        .annotations
        .or_else(|| core_tool_annotations(descriptor.name))
}

fn descriptor_is_read_only(descriptor: &McpToolDescriptor) -> bool {
    resolved_annotations(descriptor)
        .and_then(|annotations| annotations.read_only)
        .unwrap_or(false)
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
    client_name: &str,
    client_version: &str,
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
        client_name: client_name.to_string(),
        client_version: client_version.to_string(),
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
    // `model_id` is the reserved operator label: `author_from_args` has already
    // captured it into the author context, so strip it as a context field. This
    // lets any dispatcher tool (whose per-action specs do not list `model_id`)
    // accept it without a spurious unexpected-field rejection; flat tools that
    // want it (e.g. core_derive) read it from `ctx.author.model_id`.
    obj.remove("model_id");
}

#[cfg(test)]
mod tests {
    use super::*;
    use proxima_core::protocol::{action as protocol_action, tool as protocol_tool};

    #[test]
    fn author_from_args_extracts_caller_self_perspective() {
        let self_id = uuid::Uuid::now_v7();
        let args = serde_json::json!({
            "model_id": "test-model",
            "_proxima_caller_self_perspective": self_id.to_string(),
        });

        let author = author_from_args(&args, None, "unknown", "0").expect("author context");

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
    fn strip_call_context_args_removes_reserved_model_id() {
        // `model_id` is captured into the author context before stripping, then
        // removed so a dispatcher tool that does not list it as an action field
        // is not tripped by an unexpected-field rejection.
        let args = serde_json::json!({ "action": "set", "model_id": "claude" });
        let author = author_from_args(&args, None, "unknown", "0").expect("author reads model_id");
        assert_eq!(author.model_id, "claude");

        let mut args = args;
        strip_call_context_args(&mut args);
        assert!(args.get("model_id").is_none(), "model_id stripped: {args}");
        assert_eq!(args["action"], "set");
    }

    #[test]
    fn author_from_args_carries_peer_implementation() {
        let args = serde_json::json!({ "model_id": "m" });
        let author = author_from_args(&args, None, "example-client", "1.2.3")
            .expect("author with peer info");
        assert_eq!(author.client_name, "example-client");
        assert_eq!(author.client_version, "1.2.3");
    }

    #[test]
    fn palette_scope_narrows_advertised_dispatcher_actions() {
        use proxima_core::ToolScope;

        let registry = proxima_core::FlavorRegistry::new().freeze_or_panic_for_tests();
        let goal = registry
            .list_mcp_tools()
            .iter()
            .find(|descriptor| descriptor.name == protocol_tool::CORE_GOAL)
            .expect("core_goal descriptor")
            .clone();

        // A palette that permits only the `set` leaf of core_goal.
        let scope = ToolScope::Palette(vec![protocol_action::CORE_GOAL_SET.to_string()]);
        let projected =
            project_dispatcher_actions(&goal.args_schema, Some(&scope), protocol_tool::CORE_GOAL);

        let enum_values = projected
            .pointer("/properties/action/enum")
            .and_then(serde_json::Value::as_array)
            .expect("action enum");
        assert_eq!(enum_values, &vec![serde_json::json!("set")]);

        let advertised: Vec<&str> = projected
            .get("x-proxima-actions")
            .and_then(serde_json::Value::as_object)
            .expect("x-proxima-actions")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(advertised, vec!["set"]);
    }

    #[test]
    fn all_scope_leaves_dispatcher_actions_unchanged() {
        use proxima_core::ToolScope;

        let registry = proxima_core::FlavorRegistry::new().freeze_or_panic_for_tests();
        let goal = registry
            .list_mcp_tools()
            .iter()
            .find(|descriptor| descriptor.name == protocol_tool::CORE_GOAL)
            .expect("core_goal descriptor")
            .clone();
        let projected = project_dispatcher_actions(
            &goal.args_schema,
            Some(&ToolScope::All),
            protocol_tool::CORE_GOAL,
        );
        assert_eq!(projected, goal.args_schema);
    }

    #[test]
    fn not_authorized_message_lists_allowed_actions() {
        use proxima_core::ToolScope;

        let scope = ToolScope::Palette(vec![protocol_action::CORE_GOAL_SET.to_string()]);
        let err = tool_invocation_error_to_error_data(
            McpToolError::NotAuthorized(protocol_action::CORE_GOAL_TRANSITION.into()).into(),
            Some(&scope),
        );
        assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_REQUEST);
        assert!(
            err.message.contains(&format!(
                "allowed {} actions: set",
                protocol_tool::CORE_GOAL
            )),
            "message: {}",
            err.message
        );
    }

    #[test]
    fn unavailable_error_reaches_caller_as_invalid_request() {
        let err = mcp_tool_error_to_error_data(&McpToolError::Unavailable(
            "semantic search unavailable: no embedding client is configured for this host".into(),
        ));
        assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_REQUEST);
        assert_eq!(
            err.message,
            "semantic search unavailable: no embedding client is configured for this host"
        );
    }

    #[test]
    fn caller_self_perspective_metadata_errors_are_invalid_params() {
        let args = serde_json::json!({
            "caller_self_perspective": 42,
        });
        let err =
            author_from_args(&args, None, "unknown", "0").expect_err("non-string metadata fails");
        assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
        assert!(
            err.message.contains("caller_self_perspective"),
            "message: {}",
            err.message
        );

        let args = serde_json::json!({
            "current_root_perspective_memory_id": "not-a-uuid",
        });
        let err =
            author_from_args(&args, None, "unknown", "0").expect_err("invalid uuid metadata fails");
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

    /// A missing entity is a `resource_not_found` on the resource path but
    /// an argument fault on the tool path — the same `McpToolError` maps
    /// to different JSON-RPC codes by surface.
    #[test]
    fn not_found_maps_by_surface() {
        let not_found = || McpToolError::NotFound("memory F:018f not found".into());

        let resource = resource_invocation_error_to_error_data(
            crate::server::ToolInvocationError::Tool(not_found()),
        );
        assert_eq!(resource.code, rmcp::model::ErrorCode::RESOURCE_NOT_FOUND);
        assert_eq!(resource.message, "memory F:018f not found");

        let tool = mcp_tool_error_to_error_data(&not_found());
        assert_eq!(tool.code, rmcp::model::ErrorCode::INVALID_PARAMS);
        assert_eq!(tool.message, "memory F:018f not found");
    }

    /// An unmatched resource URI is a `resource_not_found`, not the
    /// `invalid_params` it used to collapse into.
    #[test]
    fn unknown_resource_uri_maps_to_resource_not_found() {
        let err = resource_invocation_error_to_error_data(
            crate::server::ToolInvocationError::ToolNotFound("proxima://nope".into()),
        );
        assert_eq!(err.code, rmcp::model::ErrorCode::RESOURCE_NOT_FOUND);
        assert!(err.message.contains("proxima://nope"), "{}", err.message);
    }

    #[test]
    fn tool_scope_denials_remain_invalid_request() {
        // No scope threaded (e.g. direct-handler path) → bare message, no
        // allowed-action enrichment.
        let err = tool_invocation_error_to_error_data(
            McpToolError::NotAuthorized(protocol_action::CORE_GOAL_SET.into()).into(),
            None,
        );

        assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_REQUEST);
        assert_eq!(
            err.message,
            format!(
                "tool {} not authorized for this MCP token",
                protocol_action::CORE_GOAL_SET
            )
        );
    }

    /// A viewer sees a flavor's read tool in `tools/list`.
    ///
    /// This gate resolved read-vs-write from `core_tool_annotations(name)`
    /// alone — a table over *core* names — and fell through to
    /// `may_write` for everything else. So every flavor tool, read or
    /// not, was hidden from a read-only principal: not refused with a
    /// reason, just absent, which is the harder symptom to trace.
    ///
    /// PR 130 fixed the sibling gate on the *call* path
    /// (`ScopeGateBehavior::enforce_owner_role`). This is the
    /// *visibility* path, and it kept the bug. The call sites all had the
    /// descriptor in hand and passed only its name.
    #[test]
    fn a_viewer_sees_a_flavor_read_tool_and_not_its_write_tool() {
        use proxima_core::mcp::{McpToolAnnotations, McpToolOrigin};
        use proxima_core::{AuthPath, AuthzContext, GroupId, Owner, UserId, access::Role};

        fn descriptor(
            name: &'static str,
            annotations: Option<McpToolAnnotations>,
        ) -> McpToolDescriptor {
            McpToolDescriptor {
                name,
                description: "stub",
                origin: McpToolOrigin::Flavor("proxima-stub".to_string()),
                produces_schema_ids: &[],
                args_schema: serde_json::json!({"type": "object"}),
                action_arg_specs: &[],
                annotations,
                call: &|_, _| Box::pin(async { Ok(serde_json::Value::Null) }),
            }
        }

        let owner = Owner::Group(GroupId::new(uuid::Uuid::now_v7()));
        let viewer = McpAuthContext {
            owner,
            authz: AuthzContext::for_subject_with_role(
                UserId::new(uuid::Uuid::now_v7()),
                [(owner, Role::viewer())],
                AuthPath::HostBearer,
            ),
            model_id: None,
        };

        let read = descriptor(
            "proxima-stub_search",
            Some(McpToolAnnotations::new().read_only(true).open_world(false)),
        );
        let write = descriptor(
            "proxima-stub_write",
            Some(McpToolAnnotations::new().read_only(false).open_world(false)),
        );
        // Core still answers through the manifest, with no descriptor
        // annotations of its own.
        let core_read = McpToolDescriptor {
            origin: McpToolOrigin::Substrate,
            ..descriptor(protocol_tool::CORE_SEARCH_MEMORIES, None)
        };

        assert!(
            owner_role_allows_tool(Some(&viewer), &read),
            "a flavor tool that declares read_only must be visible to a viewer"
        );
        assert!(
            !owner_role_allows_tool(Some(&viewer), &write),
            "a flavor write tool must stay hidden from a viewer"
        );
        assert!(
            owner_role_allows_tool(Some(&viewer), &core_read),
            "core still resolves through the manifest"
        );
    }

    // Completeness gate: every `core_` tool the substrate registers must
    // carry MCP annotations, so a newly-added core tool cannot silently ship
    // with unset hints (client defaults: not-read-only, destructive,
    // open-world — wrong for this closed substrate).
    #[test]
    fn every_core_tool_is_annotated() {
        let registry = proxima_core::FlavorRegistry::new().freeze_or_panic_for_tests();
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
        let read = core_tool_annotations(protocol_tool::CORE_SEARCH_MEMORIES).expect("read tool");
        assert_eq!(read.read_only, Some(true));
        assert_eq!(read.open_world, Some(false));

        // Convergent additive write (required idempotency key).
        let derive = core_tool_annotations(protocol_tool::CORE_DERIVE).expect("write tool");
        assert_eq!(derive.read_only, Some(false));
        assert_eq!(derive.destructive, Some(false));
        assert_eq!(derive.idempotent, Some(true));

        // Additive write with an OPTIONAL idempotency key: identical args
        // without a key create a new Fact, so it is not replay-safe.
        let remember =
            core_tool_annotations(protocol_tool::CORE_REMEMBER).expect("non-idempotent write");
        assert_eq!(remember.read_only, Some(false));
        assert_eq!(remember.destructive, Some(false));
        assert_eq!(remember.idempotent, Some(false));

        // Grouped fact dispatcher now contains only citation reads.
        let fact = core_tool_annotations(protocol_tool::CORE_FACT).expect("fact dispatcher");
        assert_eq!(fact.read_only, Some(true));
        assert_eq!(fact.destructive, Some(false));
        assert_eq!(fact.idempotent, Some(true));

        // Create-new-each-call write is not replay-safe.
        let link = core_tool_annotations(protocol_tool::CORE_LINK).expect("create tool");
        assert_eq!(link.idempotent, Some(false));

        // Grouped goal dispatcher aggregates mixed write actions, so it is
        // advertised as non-idempotent at tool level.
        let goal = core_tool_annotations(protocol_tool::CORE_GOAL).expect("goal dispatcher");
        assert_eq!(goal.read_only, Some(false));
        assert_eq!(goal.destructive, Some(false));
        assert_eq!(goal.idempotent, Some(false));

        // Flavor-shipped / unknown tools get no substrate hints here.
        assert!(core_tool_annotations("company/upsert").is_none());
    }

    /// A NUL is well-formed JSON and fatal to Postgres, so it has to be
    /// caught here rather than surfacing as a server fault.
    #[test]
    fn nul_in_a_string_argument_is_invalid_params() {
        let args = serde_json::json!({ "query": "chunk\u{0}er" });
        let err = reject_nul_in_args(&args).expect_err("NUL must be rejected");
        assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
        assert!(err.message.contains("NUL"), "{}", err.message);
    }

    #[test]
    fn nul_nested_in_an_array_or_object_is_found() {
        for args in [
            serde_json::json!({ "tags": ["fine", "b\u{0}ad"] }),
            serde_json::json!({ "outer": { "inner": { "deep": "b\u{0}ad" } } }),
            serde_json::json!({ "list": [{ "k": ["x", { "y": "b\u{0}ad" }] }] }),
        ] {
            assert!(
                reject_nul_in_args(&args).is_err(),
                "NUL must be found anywhere in the tree: {args}"
            );
        }
    }

    #[test]
    fn nul_in_an_argument_name_is_rejected() {
        let mut map = serde_json::Map::new();
        map.insert("na\u{0}me".to_string(), serde_json::json!("fine"));
        let err = reject_nul_in_args(&serde_json::Value::Object(map))
            .expect_err("NUL in a key must be rejected");
        assert!(err.message.contains("argument names"), "{}", err.message);
    }

    /// The check must not reject ordinary arguments, including other
    /// control characters and non-ASCII text, which Postgres stores fine.
    #[test]
    fn ordinary_arguments_pass() {
        let args = serde_json::json!({
            "query": "how does the chunker decide\tsize?\nline two",
            "limit": 12,
            "include_calls": true,
            "repo_handle": serde_json::Value::Null,
            "tags": ["münchen", "\u{1F525} emoji", ""],
        });
        assert!(reject_nul_in_args(&args).is_ok());
    }

    /// The deepest argument tree that can actually arrive.
    ///
    /// `serde_json` rejects nesting at depth 128 with "recursion limit
    /// exceeded", so 127 is the maximum an argument value can reach — this
    /// walks it without touching the stack. (A hand-built `Value` far deeper
    /// than that overflows on `Drop`, in `serde_json` itself, before this
    /// function is ever called; such a value cannot come from a request.)
    #[test]
    fn the_deepest_reachable_argument_tree_is_walked() {
        let deep = format!("{}{}{}", "[".repeat(127), "\"leaf\"", "]".repeat(127));
        let value: serde_json::Value =
            serde_json::from_str(&deep).expect("127 levels is under the parser limit");
        assert!(reject_nul_in_args(&value).is_ok());

        let too_deep = format!("{}{}{}", "[".repeat(128), "\"leaf\"", "]".repeat(128));
        assert!(
            serde_json::from_str::<serde_json::Value>(&too_deep).is_err(),
            "the parser, not this function, is what bounds depth"
        );
    }
}
