//! rmcp dynamic tool projection.
//!
//! The SDK exposes dynamic tools through direct
//! `ServerHandler::list_tools` / `call_tool` overrides. This adapter
//! projects the frozen build-time `FlavorRegistry` tool descriptors
//! into MCP tool metadata at request time.

use std::borrow::Cow;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use proxima_core::mcp::{
    McpToolAnnotations, McpToolCtx, McpToolDescriptor, McpToolError, McpToolErrorKind, ToolContent,
    ToolDescriptorView, ToolEffect, ToolReply, all_core_resources, mcp_wire_output_schema,
    normalize_mcp_output_schema, provider_safe_tool_name, tool_name_matches,
};
use proxima_core::{
    FlavorRegistryFrozen, McpAuthorContext, MemoryId, UNKNOWN_OPERATOR_LABEL,
    resolve_operator_label,
};
use rmcp::ServerHandler;
use rmcp::model::{
    CacheScope, CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock,
    DiscoverResult, ErrorData, Implementation, InitializeRequestParams, InitializeResult,
    JsonObject, ListResourceTemplatesResult, ListResourcesResult, ListToolsResult, MetaObject,
    PaginatedRequestParams, ProgressNotificationParam, ProgressToken, ProtocolVersion,
    ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult, Resource,
    ResourceContents, ResourceTemplate, ServerCapabilities, ServerConfig, SubscriptionFilter, Tool,
    ToolAnnotations,
};
use rmcp::service::{MaybeSendFuture, Peer, RequestContext, RoleServer, SubscriptionContext};

use crate::selfdoc;

/// Product name reported to MCP clients on `initialize`.
const SERVER_NAME: &str = "proxima";

/// Newest MCP revision Proxima implements. rmcp's default admits every
/// revision rmcp knows, so an rmcp upgrade that learns a newer one would
/// start serving it before Proxima meets it: `2026-07-28` was admitted that
/// way while the SEP-2549 list cache hints it requires were missing, and a
/// client on it rejected `tools/list`. Raise this only with the revision's
/// server obligations met; a test fails when rmcp knows a newer one.
const MAX_PROTOCOL_VERSION: ProtocolVersion = ProtocolVersion::V_2026_07_28;

/// SEP-2549 freshness of every list result, in milliseconds. Each list is
/// projected from the caller's token scope, so a grant or revocation must
/// show on the next list rather than after a cache expires.
const LIST_TTL_MS: u64 = 0;

/// SEP-2549 cache scope of every list result: per-caller, for the same
/// reason as [`LIST_TTL_MS`], so no shared cache may serve it to another
/// token.
const LIST_CACHE_SCOPE: CacheScope = CacheScope::Private;

use crate::auth::McpAuthContext;
use crate::host_tools::McpHostTool;
use crate::server::{McpToolHost, ToolInvocationError};
use proxima_core::{CapabilitySet, KeyDeclaration, ResourceKey, ScopeKey, ToolName};

#[derive(Clone, Debug)]
pub struct DynamicHandler {
    pub server: McpToolHost,
    /// Interval of the `notifications/progress` heartbeat a running tool
    /// call sends when its request carries a progress token.
    pub progress_heartbeat: Duration,
}

impl DynamicHandler {
    /// A handler over `server` with [`crate::DEFAULT_PROGRESS_HEARTBEAT`].
    #[must_use]
    pub const fn new(server: McpToolHost) -> Self {
        Self {
            server,
            progress_heartbeat: crate::DEFAULT_PROGRESS_HEARTBEAT,
        }
    }

    /// Set the progress heartbeat interval; keep it under the session idle
    /// timeout ([`crate::McpTransportConfig::progress_heartbeat`]).
    #[must_use]
    pub const fn with_progress_heartbeat(mut self, interval: Duration) -> Self {
        self.progress_heartbeat = interval;
        self
    }

    /// [`ServerHandler::get_info`] plus `instructions` generated from the
    /// caller's admitted surface (exact token keys, deployment surface, and
    /// request behaviors' `visible`), the surface `list_tools`
    /// advertises, followed by the host's own contribution
    /// ([`McpHostTools::instructions`](crate::McpHostTools::instructions)). A
    /// `memory`-profile deployment thus omits guidance for tools it does not
    /// expose.
    fn info_for<'a>(
        &'a self,
        context: &RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ServerConfig, ErrorData>> + MaybeSendFuture + use<'a> {
        let auth = auth_context(context);
        let listing = listing_ctx(&self.server, auth.as_ref(), context);
        async move {
            let generated = {
                let ctx = listing?;
                let caller = Caller::listing(auth.as_ref(), ctx.as_ref());
                selfdoc::build_instructions(
                    &advertised_tool_ids(self.server.registry(), caller),
                    &advertised_resource_scope_keys(auth.as_ref()),
                )
            };
            let host = match &auth {
                Some(auth) => self.server.host_instructions(auth).await,
                None => None,
            };
            let mut info = self.get_info();
            info.instructions = selfdoc::compose_instructions(&generated, host.as_deref());
            Ok(info)
        }
    }
}

/// Canonical ids of the tools a listing shows `caller`. The filter
/// `list_tools` applies, so self-documentation never references a tool the
/// caller cannot see.
pub(crate) fn advertised_tool_ids(
    registry: &FlavorRegistryFrozen,
    caller: Caller<'_>,
) -> BTreeSet<&'static str> {
    registry
        .list_mcp_tools()
        .iter()
        .filter(|descriptor| tool_allowed_for_auth(caller, descriptor))
        .map(|descriptor| descriptor.name)
        .collect()
}

/// The tool context request behaviors judge one listing with: the caller's
/// own, built as [`McpToolHost::read_resource_in_request`] builds its. `None`
/// without an authenticated caller, which a release build lists nothing to.
fn listing_ctx(
    server: &McpToolHost,
    auth: Option<&McpAuthContext>,
    context: &RequestContext<RoleServer>,
) -> Result<Option<McpToolCtx>, ErrorData> {
    let Some(auth) = auth else {
        return Ok(None);
    };
    let (client_name, client_version) = peer_implementation(context);
    let author = author_from_ctx(Some(auth), &client_name, &client_version);
    let request = request_services(server, context)?;
    server
        .ctx_for_request(author, auth, request)
        .map(Some)
        .map_err(|err| mcp_tool_error_to_error_data(&err))
}

impl ServerHandler for DynamicHandler {
    fn get_info(&self) -> ServerConfig {
        let mut info = ServerConfig::default();
        info.capabilities = ServerCapabilities::builder()
            .enable_tools()
            .enable_resources()
            .build();
        if self.server.tool_list_notifier().is_some()
            && let Some(tools) = info.capabilities.tools.as_mut()
        {
            tools.list_changed = Some(true);
        }
        // NOT `Implementation::from_build_env()`: those `env!` macros expand
        // against rmcp's own manifest, so every Proxima deployment introduced
        // itself as `rmcp 2.2.0` and no client or operator could tell which
        // release they were talking to.
        info.server_info = Implementation::new(SERVER_NAME, proxima_core::RELEASE_VERSION);
        info
    }

    /// Every revision rmcp knows up to `2026-07-28` (`MAX_PROTOCOL_VERSION`).
    /// This bounds what `initialize` may agree to, what a per-request
    /// version may name, and what `server/discover` advertises. A host
    /// wrapping this handler in its own `ServerHandler` delegates here (and
    /// to `discover`, `accepted_subscription_filter` and `listen`), or rmcp's
    /// defaults apply instead.
    fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
        Cow::Borrowed(ProtocolVersion::known_up_to(&MAX_PROTOCOL_VERSION))
    }

    /// The `initialize` handshake (revisions up to `2025-11-25`), answered
    /// with per-caller instructions (`info_for`). Mirrors the SDK
    /// default's `set_peer_info` bookkeeping, and registers the session
    /// with the tool-list notifier when one is attached.
    fn initialize(
        &self,
        request: InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<InitializeResult, ErrorData>> + MaybeSendFuture + '_ {
        if context.peer.peer_info().is_none() {
            context.peer.set_peer_info(request);
        }
        if let (Some(notifier), Some(auth)) =
            (self.server.tool_list_notifier(), auth_context(&context))
        {
            notifier.register_session(auth.owner, context.peer.clone());
        }
        self.info_for(&context)
    }

    /// `server/discover`, which replaces the handshake from `2026-07-28`:
    /// the same per-caller instructions `initialize` returns, never cached
    /// across callers (rmcp sets `ttlMs: 0`, `cacheScope: private`).
    fn discover(
        &self,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<DiscoverResult, ErrorData>> + MaybeSendFuture + '_ {
        let info = self.info_for(&context);
        async move {
            Ok(DiscoverResult::from_server_info(
                self.supported_protocol_versions().into_owned(),
                info.await?,
            ))
        }
    }

    /// `subscriptions/listen` (`2026-07-28`) carries `toolsListChanged` and
    /// nothing else, and only with a tool-list notifier attached.
    fn accepted_subscription_filter(
        &self,
        _requested: &SubscriptionFilter,
    ) -> Option<SubscriptionFilter> {
        self.server
            .tool_list_notifier()
            .map(|_| SubscriptionFilter::builder().tools_list_changed().build())
    }

    /// Holds one accepted subscription under the caller's owner until the
    /// client ends it.
    fn listen(
        &self,
        context: SubscriptionContext,
    ) -> impl Future<Output = Result<(), ErrorData>> + MaybeSendFuture + '_ {
        let registration = match (
            self.server.tool_list_notifier(),
            auth_context(context.request_context()),
        ) {
            (Some(notifier), Some(auth)) if context.accepted().tools_list_changed == Some(true) => {
                Some(notifier.register_subscription(auth.owner, context.sink().clone()))
            }
            _ => None,
        };
        async move {
            context.cancelled().await;
            drop(registration);
            Ok(())
        }
    }

    fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListResourcesResult, ErrorData>> + MaybeSendFuture + '_ {
        let auth = auth_context(&context);
        let resource = Resource::new(selfdoc::HOW_TO_URI, selfdoc::HOW_TO_NAME)
            .with_title(selfdoc::HOW_TO_TITLE)
            .with_description(selfdoc::HOW_TO_DESCRIPTION)
            .with_mime_type(selfdoc::HOW_TO_MIME);
        let mut resources = Vec::new();
        if how_to_allowed_for_auth(auth.as_ref()) {
            resources.push(resource);
        }
        resources.extend(
            all_core_resources()
                .filter(|resource| {
                    !resource.is_template && resource_allowed_for_auth(auth.as_ref(), resource)
                })
                .map(raw_resource_from_meta),
        );
        std::future::ready(Ok(ListResourcesResult::with_all_items(resources)
            .with_ttl_ms(LIST_TTL_MS)
            .with_cache_scope(LIST_CACHE_SCOPE)))
    }

    fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListResourceTemplatesResult, ErrorData>> + MaybeSendFuture + '_
    {
        let auth = auth_context(&context);
        let resource_templates = all_core_resources()
            .filter(|resource| {
                resource.is_template && resource_allowed_for_auth(auth.as_ref(), resource)
            })
            .map(raw_resource_template_from_meta)
            .collect();
        std::future::ready(Ok(ListResourceTemplatesResult::with_all_items(
            resource_templates,
        )
        .with_ttl_ms(LIST_TTL_MS)
        .with_cache_scope(LIST_CACHE_SCOPE)))
    }

    /// Answers are always [`ReadResourceResponse::Complete`]. rmcp 3 widened
    /// this return type for MRTR (SEP-2322), whose other variant lets a server
    /// pause mid-request to ask the *client* for input. Proxima resolves every
    /// resource from its own store under the caller's already-established
    /// scope, so there is nothing to ask back for — see [`call_tool`] for the
    /// same reasoning on the tool path.
    ///
    /// [`call_tool`]: DynamicHandler::call_tool
    fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ReadResourceResponse, ErrorData>> + MaybeSendFuture + '_ {
        let uri = request.uri;
        let auth = auth_context(&context);
        let (client_name, client_version) = peer_implementation(&context);
        let request_services = request_services(&self.server, &context);
        let how_to_ctx = if uri == selfdoc::HOW_TO_URI {
            listing_ctx(&self.server, auth.as_ref(), &context)
        } else {
            Ok(None)
        };
        let server = self.server.clone();
        async move {
            if uri == selfdoc::HOW_TO_URI {
                if !how_to_allowed_for_auth(auth.as_ref()) {
                    return Err(mcp_tool_error_to_error_data(&McpToolError::NotAuthorized(
                        proxima_core::protocol::resource::HOW_TO.into(),
                    )));
                }
                let ctx = how_to_ctx?;
                let caller = Caller::listing(auth.as_ref(), ctx.as_ref());
                let advertised = advertised_tool_ids(server.registry(), caller);
                let advertised_resources = advertised_resource_scope_keys(auth.as_ref());
                let body = selfdoc::how_to_markdown(&advertised, &advertised_resources);
                return Ok(ReadResourceResult::new(vec![
                    ResourceContents::text(body, selfdoc::HOW_TO_URI)
                        .with_mime_type(selfdoc::HOW_TO_MIME),
                ])
                .into());
            }
            if !uri.starts_with("proxima://") {
                return Err(ErrorData::resource_not_found(
                    format!("unknown resource {uri}"),
                    None,
                ));
            }
            let author = author_from_ctx(auth.as_ref(), &client_name, &client_version);
            let request = request_services?;
            let value = server
                .read_resource_in_request(&uri, author, auth, request)
                .await
                .map_err(resource_invocation_error_to_error_data)?;
            let text = serde_json::to_string(&value).map_err(generic_internal_error)?;
            Ok(ReadResourceResult::new(vec![
                ResourceContents::text(text, uri).with_mime_type("application/json"),
            ])
            .into())
        }
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, ErrorData>> + MaybeSendFuture + '_ {
        let auth = auth_context(&context);
        let listing = listing_ctx(&self.server, auth.as_ref(), &context);
        async move {
            let ctx = listing?;
            let caller = Caller::listing(auth.as_ref(), ctx.as_ref());
            // `core_search_memories` names the caller's readable stores when
            // an other-stores port is wired; the port is asked once here
            // because the projection below is synchronous.
            let search_schema = match ctx.as_ref() {
                Some(ctx) => Some(
                    caller_search_input_schema(
                        ctx,
                        self.server
                            .registry()
                            .list_mcp_tools()
                            .iter()
                            .find(|d| d.name == proxima_core::protocol::tool::CORE_SEARCH_MEMORIES)
                            .map(|d| project_dispatcher_actions_for_auth(caller, d))
                            .unwrap_or_default(),
                    )
                    .await?,
                ),
                None => None,
            };
            let mut tools: Vec<Tool> = self
                .server
                .registry()
                .list_mcp_tools()
                .iter()
                .filter(|descriptor| tool_allowed_for_auth(caller, descriptor))
                .map(|descriptor| {
                    let schema = match &search_schema {
                        Some(schema)
                            if descriptor.name
                                == proxima_core::protocol::tool::CORE_SEARCH_MEMORIES =>
                        {
                            schema.clone()
                        }
                        _ => project_dispatcher_actions_for_auth(caller, descriptor),
                    };
                    let tool = Tool::new(
                        Cow::Owned(provider_safe_tool_name(descriptor.name)),
                        Cow::Borrowed(descriptor.description),
                        Arc::new(rmcp::model::object(schema)),
                    )
                    .with_raw_output_schema(Arc::new(rmcp::model::object(mcp_wire_output_schema(
                        &descriptor.output_schema,
                    ))));
                    match annotations_for_auth(caller, descriptor) {
                        Some(annotations) => tool.annotate(to_rmcp_annotations(annotations)),
                        None => tool,
                    }
                })
                .collect();
            if let Some(auth) = &auth {
                let host_tools = self
                    .server
                    .host_tools_for(auth)
                    .await
                    .map_err(|err| mcp_tool_error_to_error_data(&err))?;
                tools.extend(
                    host_tools
                        .into_iter()
                        .filter(|tool| host_tool_allowed_for_auth(caller, tool))
                        .filter_map(host_tool_metadata),
                );
            }
            Ok(ListToolsResult::with_all_items(tools)
                .with_ttl_ms(LIST_TTL_MS)
                .with_cache_scope(LIST_CACHE_SCOPE))
        }
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
                )
                .with_raw_output_schema(Arc::new(rmcp::model::object(
                    mcp_wire_output_schema(&descriptor.output_schema),
                )));
                match annotations_for_auth(Caller::new(None), descriptor) {
                    Some(annotations) => tool.annotate(to_rmcp_annotations(annotations)),
                    None => tool,
                }
            })
    }

    /// Answers are always [`CallToolResponse::Complete`]. rmcp 3 widened this
    /// return type for two draft extensions Proxima deliberately does not
    /// implement:
    ///
    /// - `InputRequired` (MRTR, SEP-2322) pauses a call to elicit input from
    ///   the client. Every Proxima tool is a single authorized store
    ///   operation whose arguments arrive complete; a missing or malformed one
    ///   is a caller error, reported as such by
    ///   `tool_invocation_error_to_error_data`, not a prompt to fill in.
    /// - `Task` (SEP-2663) hands back a task id for the client to poll. That
    ///   is for long-running work; Proxima answers inline.
    ///
    /// Neither is advertised in [`get_info`]'s capabilities, so a spec-abiding
    /// client never expects them.
    ///
    /// [`get_info`]: DynamicHandler::get_info
    fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CallToolResponse, ErrorData>> + MaybeSendFuture + '_ {
        let server = self.server.clone();
        let auth = auth_context(&context);
        let (client_name, client_version) = peer_implementation(&context);
        let request_services = request_services(&self.server, &context);
        let heartbeat = context
            .meta
            .get_progress_token()
            .map(|token| (token, context.peer.clone(), self.progress_heartbeat));
        async move {
            let request_services = request_services?;
            let request_name = request.name.to_string();
            let mut args = request
                .arguments
                .map_or_else(|| serde_json::json!({}), serde_json::Value::Object);
            let author = author_from_args(&args, auth.as_ref(), &client_name, &client_version)?;
            strip_call_context_args(&mut args);
            let error_auth = auth.clone();
            let call = async {
                let Some(auth) = auth else {
                    let name = canonical_tool_name(&server, &request_name)
                        .unwrap_or_else(|| request_name.clone());
                    return Err(ToolInvocationError::NotAuthorized(name));
                };
                let target = match server.resolve_tool(&auth, &request_name).await {
                    Ok(target) => target,
                    Err(err) => return Err(err),
                };
                server
                    .call_resolved(target, args, author, auth, request_services)
                    .await
            };
            let reply = with_progress_heartbeat(call, heartbeat).await;
            let outcome = reply
                .map_err(|err| {
                    tool_invocation_error_to_error_data(server.registry(), err, error_auth.as_ref())
                })
                .and_then(|reply| wire_reply(&request_name, reply));
            Ok(outcome?.into())
        }
    }
}

/// Await `call`, sending `notifications/progress` for the request's token
/// every interval until it finishes.
///
/// rmcp closes a session that carries no traffic for its idle timeout, and
/// neither SSE pings nor a call still running count: a long tool call lost
/// its session and its result. Each notification is session traffic, and
/// tells the client the call is alive. `progress` counts beats — the spec
/// asks only that it increase — and no total is claimed.
async fn with_progress_heartbeat<F: Future>(
    call: F,
    heartbeat: Option<(ProgressToken, Peer<RoleServer>, Duration)>,
) -> F::Output {
    let Some((token, peer, interval)) = heartbeat else {
        return call.await;
    };
    let mut call = std::pin::pin!(call);
    let mut ticks = tokio::time::interval_at(tokio::time::Instant::now() + interval, interval);
    let mut beats = 0_u32;
    loop {
        tokio::select! {
            output = &mut call => return output,
            _ = ticks.tick() => {
                beats = beats.saturating_add(1);
                let beat = ProgressNotificationParam::new(token.clone(), f64::from(beats))
                    .with_message("tool call running");
                if let Err(err) = peer.notify_progress(beat).await {
                    tracing::debug!(error = %err, "progress heartbeat not delivered");
                }
            }
        }
    }
}

/// Map a tool-invocation failure to a typed JSON-RPC error so external
/// agents can tell bad input from a server fault, instead of every failure
/// collapsing to `internal_error` (-32603). The mapping `tools/call` uses;
/// a not-authorized message lists the caller's still-allowed actions.
#[must_use]
pub fn tool_invocation_error_to_error_data(
    registry: &FlavorRegistryFrozen,
    err: ToolInvocationError,
    auth: Option<&McpAuthContext>,
) -> ErrorData {
    match err {
        ToolInvocationError::NotAuthorized(name) => {
            ErrorData::invalid_request(not_authorized_message(registry, &name, auth), None)
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
///
/// The candidate actions come off the descriptor's `action_arg_specs`, which
/// is the enumeration every other seam reads. Sourced from the substrate
/// `all_core_actions()` table, a denied *flavor* dispatcher listed nothing,
/// so the one caller who most needs the hint — someone holding a partial
/// palette on a tool the substrate has never heard of — got the bare message.
fn not_authorized_message(
    registry: &FlavorRegistryFrozen,
    name: &str,
    auth: Option<&McpAuthContext>,
) -> String {
    let tool = name.split_once(':').map_or(name, |(tool, _)| tool);
    let allowed: Vec<&str> = auth.map_or_else(Vec::new, |auth| {
        registry.mcp_tool(tool).map_or_else(Vec::new, |descriptor| {
            descriptor
                .action_arg_specs
                .iter()
                .map(|spec| spec.action)
                .chain(descriptor.argv_action_specs.iter().map(|spec| spec.action))
                .filter(|action| {
                    action_allowed_for_auth(Caller::new(Some(auth)), descriptor, action)
                })
                .collect()
        })
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

/// JSON-RPC "Server error", the implementation-defined range the spec
/// reserves at -32000..-32099. Declared backpressure has no code of its own
/// in either JSON-RPC or MCP, and the three standard ones all misdescribe
/// it: -32602/-32600 blame a request that was legal, and -32603 is the
/// generic internal fault this exists to stop being confused with.
const SERVER_ERROR: rmcp::model::ErrorCode = rmcp::model::ErrorCode(-32000);

/// The machine-readable discriminator carried in `error.data.code`, so a
/// client switches on a token rather than on the human message. Same slug
/// as the REST surface's problem `type` (`capacity-exhausted`), spelled in
/// the snake case the JSON-RPC payloads use.
const CAPACITY_EXHAUSTED_CODE: &str = "capacity_exhausted";

/// `error.data.code` for another store that did not answer: retryable, and
/// never -32602 (a client would read that as "no such memory").
const STORE_UNAVAILABLE_CODE: &str = "store_unavailable";
const IDEMPOTENCY_CONFLICT_CODE: &str = "idempotency_conflict";

/// Classify an [`McpToolError`] by JSON-RPC code: caller-input faults —
/// including references to missing entities — → `invalid_params` (-32602);
/// well-formed-but-illegal requests → `invalid_request` (-32600); declared
/// backpressure → server error (-32000) with `data.code`
/// `capacity_exhausted` and the message verbatim; another store that did
/// not answer → server error (-32000) with `data.code` `store_unavailable`
/// and `data.store`; infrastructure faults →
/// `internal_error` (-32603). Resource reads remap `NotFound` before
/// reaching here (resources map a missing entity to `resource_not_found`).
#[must_use]
pub fn mcp_tool_error_to_error_data(err: &McpToolError) -> ErrorData {
    match err.kind() {
        McpToolErrorKind::InvalidInput | McpToolErrorKind::NotFound => {
            ErrorData::invalid_params(err.client_message(), None)
        }
        McpToolErrorKind::InvalidRequest => {
            if let Some(data) = idempotency_conflict_data(err) {
                ErrorData::invalid_request(err.client_message(), Some(data))
            } else {
                ErrorData::invalid_request(err.client_message(), None)
            }
        }
        McpToolErrorKind::CapacityExhausted => ErrorData::new(
            SERVER_ERROR,
            err.client_message(),
            Some(serde_json::json!({ "code": CAPACITY_EXHAUSTED_CODE })),
        ),
        McpToolErrorKind::StoreUnavailable => {
            let store = match err {
                McpToolError::StoreUnavailable { store } => Some(*store),
                _ => None,
            };
            ErrorData::new(
                SERVER_ERROR,
                err.client_message(),
                Some(serde_json::json!({ "code": STORE_UNAVAILABLE_CODE, "store": store })),
            )
        }
        McpToolErrorKind::Internal => generic_internal_error(err),
    }
}

fn idempotency_conflict_data(err: &McpToolError) -> Option<serde_json::Value> {
    let existing = match err {
        McpToolError::Protocol(inner)
            if inner.code == proxima_core::error::ErrorCode::IdempotencyConflict =>
        {
            inner.existing
        }
        McpToolError::Storage(proxima_core::StorageError::IdempotencyConflict {
            existing, ..
        }) => *existing,
        _ => return None,
    };
    let mut data = serde_json::Map::new();
    data.insert(
        "code".into(),
        serde_json::Value::String(IDEMPOTENCY_CONFLICT_CODE.to_string()),
    );
    if let Some(existing) = existing {
        data.insert(
            "existing".into(),
            serde_json::Value::String(existing.to_string()),
        );
    }
    Some(serde_json::Value::Object(data))
}

fn generic_internal_error(err: impl std::fmt::Display) -> ErrorData {
    tracing::error!(error = %err, "mcp internal error");
    ErrorData::internal_error("internal server error", None)
}

fn structured_tool_output(
    tool_name: &str,
    output: serde_json::Value,
) -> Result<(serde_json::Value, String), ErrorData> {
    if !output.is_object() {
        tracing::error!(tool = %tool_name, "mcp tool output must be a JSON object");
        return Err(ErrorData::internal_error("internal server error", None));
    }
    let text = serde_json::to_string(&output).map_err(generic_internal_error)?;
    Ok((output, text))
}

/// A tool's reply as the wire carries it.
fn wire_reply(tool: &str, reply: ToolReply) -> Result<CallToolResult, ErrorData> {
    match reply {
        ToolReply::Structured(output) => {
            let (output, text) = structured_tool_output(tool, output)?;
            let mut result = CallToolResult::success(vec![ContentBlock::text(text)]);
            result.structured_content = Some(output);
            Ok(result)
        }
        ToolReply::Content(content) => {
            let blocks: Vec<ContentBlock> = content.into_iter().map(content_block).collect();
            Ok(CallToolResult::success(blocks))
        }
        ToolReply::Failure(content) => Ok(CallToolResult::error(
            content.into_iter().map(content_block).collect(),
        )),
        ToolReply::StructuredFailure {
            structured,
            content,
        } => {
            let mut result =
                CallToolResult::error(content.into_iter().map(content_block).collect());
            result.structured_content = Some(serde_json::Value::Object(structured));
            Ok(result)
        }
    }
}

/// The MCP content block of the same name. Matched variant by variant, so a
/// new [`ToolContent`] shape cannot ship without its wire form.
fn content_block(content: ToolContent) -> ContentBlock {
    match content {
        ToolContent::Text(text) => ContentBlock::text(text),
        ToolContent::Image { data, mime_type } => ContentBlock::image(data, mime_type),
        ToolContent::Audio { data, mime_type } => ContentBlock::audio(data, mime_type),
        ToolContent::ResourceLink {
            uri,
            name,
            description,
            mime_type,
        } => {
            let mut link = Resource::new(uri, name);
            if let Some(description) = description {
                link = link.with_description(description);
            }
            if let Some(mime_type) = mime_type {
                link = link.with_mime_type(mime_type);
            }
            ContentBlock::resource_link(link)
        }
        ToolContent::TextResource {
            uri,
            mime_type,
            text,
        } => ContentBlock::resource(with_mime_type(ResourceContents::text(text, uri), mime_type)),
        ToolContent::BlobResource {
            uri,
            mime_type,
            blob,
        } => ContentBlock::resource(with_mime_type(ResourceContents::blob(blob, uri), mime_type)),
    }
}

fn with_mime_type(contents: ResourceContents, mime_type: Option<String>) -> ResourceContents {
    match mime_type {
        Some(mime_type) => contents.with_mime_type(mime_type),
        None => contents,
    }
}

/// A dispatcher's `inputSchema` over the actions both policies admit,
/// so `tools/list` never advertises an action the caller cannot invoke.
/// Unrestricted (or absent) capabilities and flat tools get the full schema.
#[cfg(test)]
pub(crate) fn project_dispatcher_actions(
    descriptor: &McpToolDescriptor,
    capabilities: Option<&CapabilitySet>,
) -> serde_json::Value {
    descriptor.input_schema(|action| {
        capabilities
            .is_none_or(|capabilities| descriptor.action_advertised_by(capabilities, action))
    })
}

/// A descriptor's `inputSchema` over the actions this caller can both name
/// and authorize. A viewer of a mixed dispatcher therefore sees its read
/// leaves — their fields, their prose — without being shown write leaves it
/// cannot invoke.
pub(crate) fn project_dispatcher_actions_for_auth(
    caller: Caller<'_>,
    descriptor: &McpToolDescriptor,
) -> serde_json::Value {
    descriptor.input_schema(|action| {
        caller.auth.is_none() || action_allowed_for_auth(caller, descriptor, action)
    })
}

async fn caller_search_input_schema(
    ctx: &McpToolCtx,
    schema: serde_json::Value,
) -> Result<serde_json::Value, ErrorData> {
    proxima_core::mcp::core_tools::search_memories::caller_input_schema(ctx, schema)
        .await
        .map_err(generic_internal_error)
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

fn raw_resource_from_meta(meta: &proxima_core::flavor::ResourceContract) -> Resource {
    Resource::new(static_resource_uri(meta.uri_template), meta.name)
        .with_title(meta.title)
        .with_description(meta.description)
        .with_mime_type("application/json")
}

fn raw_resource_template_from_meta(
    meta: &proxima_core::flavor::ResourceContract,
) -> ResourceTemplate {
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

/// Whether `auth` may see one core resource: its exact caller keys and
/// deployment surface admit it, and its owner role allows the declaration.
pub(crate) fn resource_allowed_for_auth(
    auth: Option<&McpAuthContext>,
    resource: &proxima_core::flavor::ResourceContract,
) -> bool {
    resource_declaration_allowed_for_auth(auth, resource.key(), resource.declaration())
}

fn resource_declaration_allowed_for_auth(
    auth: Option<&McpAuthContext>,
    key: ResourceKey,
    declared: KeyDeclaration,
) -> bool {
    let in_scope = auth.map_or(UNAUTHENTICATED_SCOPE_ALLOWS, |ctx| {
        ctx.authz
            .capabilities()
            .admits(&ScopeKey::Resource(key), declared)
    });
    in_scope && owner_role_allows(auth, declared.is_read())
}

pub(crate) fn how_to_allowed_for_auth(auth: Option<&McpAuthContext>) -> bool {
    resource_declaration_allowed_for_auth(
        auth,
        proxima_core::mcp::how_to_resource_key(),
        proxima_core::mcp::how_to_resource_declaration(),
    )
}

pub(crate) fn advertised_resource_scope_keys(
    auth: Option<&McpAuthContext>,
) -> BTreeSet<&'static str> {
    let mut keys: BTreeSet<_> = all_core_resources()
        .filter(|resource| resource_allowed_for_auth(auth, resource))
        .map(|resource| resource.scope_key)
        .collect();
    if how_to_allowed_for_auth(auth) {
        keys.insert(proxima_core::protocol::resource::HOW_TO);
    }
    keys
}

fn author_from_ctx(
    auth: Option<&McpAuthContext>,
    client_name: &str,
    client_version: &str,
) -> McpAuthorContext {
    let trusted = trusted_model_id(auth);
    McpAuthorContext {
        model_id: trusted
            .clone()
            .unwrap_or_else(|| UNKNOWN_OPERATOR_LABEL.to_string()),
        trusted_model_id: trusted,
        client_name: client_name.to_string(),
        client_version: client_version.to_string(),
        caller_self_perspective: None,
    }
}

/// The only source of trusted model provenance on this transport: the
/// authenticated context. Never `clientInfo`, never an argument.
fn trusted_model_id(auth: Option<&McpAuthContext>) -> Option<String> {
    auth.and_then(|ctx| ctx.authz.trusted_model_id())
        .map(ToString::to_string)
}

/// Client `(name, version)`, recorded as operator provenance. A per-request
/// call (its `_meta` names a protocol version, as every `2026-07-28` call
/// does) names its client in that `_meta` or not at all; a session call
/// takes the `initialize` handshake's. Falls back to `("unknown", "0")`.
///
/// Not `RequestContext::client_info`: for a per-request call without
/// `clientInfo` it falls back to the peer rmcp synthesizes for stateless
/// requests, which carries rmcp's own name.
#[must_use]
pub fn peer_implementation(context: &RequestContext<RoleServer>) -> (String, String) {
    let client = if context.meta.protocol_version().is_some() {
        context.meta.client_info()
    } else {
        context
            .peer
            .peer_info()
            .map(|info| info.client_info.clone())
    };
    client.map_or_else(
        || ("unknown".to_string(), "0".to_string()),
        |info| (info.name, info.version),
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

/// The per-request service set (request-extension bag plus allowlisted
/// headers) of one served call. Empty when rmcp injected no HTTP parts
/// (direct handler tests).
fn request_services(
    server: &McpToolHost,
    context: &RequestContext<RoleServer>,
) -> Result<proxima_core::FlavorServices, ErrorData> {
    context
        .extensions
        .get::<http::request::Parts>()
        .map_or_else(
            || Ok(proxima_core::FlavorServices::default()),
            |parts| server.request_services(&parts.headers, &parts.extensions),
        )
        .map_err(|err| mcp_tool_error_to_error_data(&err))
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
#[must_use]
pub fn auth_context(context: &RequestContext<RoleServer>) -> Option<McpAuthContext> {
    let parts = context.extensions.get::<http::request::Parts>()?;
    let ctx = parts.extensions.get::<McpAuthContext>()?;
    Some(ctx.clone())
}

fn scope_allows(capabilities: Option<&CapabilitySet>, descriptor: &McpToolDescriptor) -> bool {
    match capabilities {
        Some(capabilities) => {
            // Argv-keyed dispatchers advertise through `tool:action` leaves
            // exactly like `action`-tagged ones — the derived key is the
            // same vocabulary the invocation gate judges.
            descriptor.advertised_by(capabilities)
        }
        // No auth context bound to the request. In release builds this
        // means the request bypassed `mcp_auth_layer` (which 401s before
        // dispatch) — fail closed rather than expose the full tool
        // surface. Direct handler tests run without the layer, so the
        // test arm stays permissive; it is compiled out of release.
        None => UNAUTHENTICATED_SCOPE_ALLOWS,
    }
}

/// Who a projection is for, and whether request behaviors may narrow it.
///
/// A listing (`tools/list`, the REST catalog, the `OpenAPI` document, the
/// instructions) is built with [`Self::listing`] and also asks every request
/// behavior's `visible`. The call path is built with [`Self::new`] and never
/// does: there a behavior's `handle` decides, with the call in hand.
#[derive(Clone, Copy)]
pub(crate) struct Caller<'a> {
    auth: Option<&'a McpAuthContext>,
    behaviors: Option<&'a McpToolCtx>,
}

impl<'a> Caller<'a> {
    /// Caller keys, deployment surface and owner role only.
    pub(crate) const fn new(auth: Option<&'a McpAuthContext>) -> Self {
        Self {
            auth,
            behaviors: None,
        }
    }

    /// Both policies, owner role and every request behavior's `visible`, judged
    /// with `ctx` (see [`McpToolCtx::behaviors_show`]). `ctx` is `None` only
    /// without an authenticated caller, which nothing is listed to outside
    /// tests.
    pub(crate) const fn listing(
        auth: Option<&'a McpAuthContext>,
        ctx: Option<&'a McpToolCtx>,
    ) -> Self {
        Self {
            auth,
            behaviors: ctx,
        }
    }

    #[cfg(feature = "rest")]
    pub(crate) const fn auth(&self) -> Option<&'a McpAuthContext> {
        self.auth
    }

    fn behaviors_show(&self, tool: &ToolDescriptorView<'_>) -> bool {
        self.behaviors.is_none_or(|ctx| ctx.behaviors_show(tool))
    }
}

/// Whether this caller may see `descriptor` at all.
///
/// A dispatcher is visible when at least one of its actions is in scope,
/// permitted by the caller's owner role, and (in a listing) shown by every
/// request behavior. This keeps a mixed flavor dispatcher useful to a viewer
/// without advertising its writes.
pub(crate) fn tool_allowed_for_auth(caller: Caller<'_>, descriptor: &McpToolDescriptor) -> bool {
    let scope = caller.auth.map(|ctx| ctx.authz.capabilities());
    if !scope_allows(scope, descriptor) {
        return false;
    }
    if !descriptor.action_arg_specs.is_empty() {
        descriptor
            .action_arg_specs
            .iter()
            .any(|spec| action_allowed_for_auth(caller, descriptor, spec.action))
    } else if !descriptor.argv_action_specs.is_empty() {
        descriptor
            .argv_action_specs
            .iter()
            .any(|spec| action_allowed_for_auth(caller, descriptor, spec.action))
    } else {
        owner_role_allows(caller.auth, descriptor.is_read_only())
            && caller.behaviors_show(&ToolDescriptorView::registry(descriptor, None))
    }
}

/// Whether one dispatcher action is in scope, owner-role authorized and (in
/// a listing) shown by every request behavior.
pub(crate) fn action_allowed_for_auth(
    caller: Caller<'_>,
    descriptor: &McpToolDescriptor,
    action: &str,
) -> bool {
    let scope_allowed = caller
        .auth
        .is_none_or(|ctx| descriptor.action_advertised_by(ctx.authz.capabilities(), action));
    // One classification rule for both vocabularies, owned by the
    // descriptor, so this advertisement decision and the owner-role gate
    // `ScopeGateBehavior` runs at call time cannot answer differently.
    scope_allowed
        && owner_role_allows(caller.auth, descriptor.action_is_read_only(action))
        && caller.behaviors_show(&ToolDescriptorView::registry(descriptor, Some(action)))
}

fn owner_role_allows(auth: Option<&McpAuthContext>, read_only: bool) -> bool {
    let Some(ctx) = auth else {
        return UNAUTHENTICATED_SCOPE_ALLOWS;
    };
    ctx.authz.may_run_tool(&ctx.owner, read_only)
}

/// MCP/REST tool-level projection for the actions visible to one caller:
/// the [`ToolEffect::join`](proxima_core::ToolEffect::join) of those
/// actions, so a caller who sees only a dispatcher's reads is told it reads.
/// `None` when the caller sees no action at all.
pub(crate) fn annotations_for_auth(
    caller: Caller<'_>,
    descriptor: &McpToolDescriptor,
) -> Option<McpToolAnnotations> {
    // Each action's classification is the descriptor's single rule, never
    // re-derived here.
    descriptor
        .effect_over(|action| {
            caller.auth.is_none() || action_allowed_for_auth(caller, descriptor, action)
        })
        .map(McpToolAnnotations::registered)
}

/// Whether a request that carries no bound auth context may see or call
/// a tool. Release: `false` (fail closed — a missing `mcp_auth_layer` is
/// a regression, not a no-auth grant). Test: `true` (direct-handler
/// ergonomics). The split makes the permissive arm un-shippable.
#[cfg(not(test))]
const UNAUTHENTICATED_SCOPE_ALLOWS: bool = false;
#[cfg(test)]
const UNAUTHENTICATED_SCOPE_ALLOWS: bool = true;

/// The reserved call-context argument that names the operator label. The REST surface
/// reserves the same names, and [`strip_call_context_args`] removes them.
pub const MODEL_ID_ARG: &str = "model_id";
/// The reserved call-context arguments that name the caller's own Self Perspective, in the
/// order they are read.
pub const SELF_PERSPECTIVE_ARGS: [&str; 3] = [
    "_proxima_caller_self_perspective",
    "caller_self_perspective",
    "current_root_perspective_memory_id",
];

/// Author context for one `tools/call`.
///
/// The reserved `model_id` argument is a caller *claim*. When the token
/// binds a model identity, that identity wins and a differing claim is
/// refused as invalid params — the same error class this function already
/// returns for malformed reserved metadata. Precedence itself lives in
/// [`resolve_operator_label`], shared with the REST surface so the two
/// transports cannot drift.
///
/// # Errors
///
/// `invalid_params` when `model_id` contradicts the token's bound model, or
/// a caller-self-perspective argument is not a UUID string.
pub fn author_from_args(
    args: &serde_json::Value,
    auth: Option<&McpAuthContext>,
    client_name: &str,
    client_version: &str,
) -> Result<McpAuthorContext, ErrorData> {
    let trusted = trusted_model_id(auth);
    let claimed = match args.get(MODEL_ID_ARG) {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(label)) => Some(label.as_str()),
        Some(_) => {
            return Err(ErrorData::invalid_params(
                format!("{MODEL_ID_ARG} must be a string"),
                None,
            ));
        }
    };
    let model_id = resolve_operator_label(trusted.as_deref(), claimed)
        .map_err(|conflict| ErrorData::invalid_params(conflict.detail("model_id"), None))?;
    let caller_self_perspective = caller_self_perspective_from_args(args)?;
    Ok(McpAuthorContext {
        model_id,
        trusted_model_id: trusted,
        client_name: client_name.to_string(),
        client_version: client_version.to_string(),
        caller_self_perspective,
    })
}

fn caller_self_perspective_from_args(
    args: &serde_json::Value,
) -> Result<Option<MemoryId>, ErrorData> {
    let Some((field, raw)) = SELF_PERSPECTIVE_ARGS
        .into_iter()
        .find_map(|field| args.get(field).map(|raw| (field, raw)))
    else {
        return Ok(None);
    };
    let Some(raw) = raw.as_str() else {
        return Err(ErrorData::invalid_params(
            format!("{field} must be a UUID string"),
            None,
        ));
    };
    let id = proxima_core::mcp::parse_wire_uuid(raw).map_err(|err| {
        ErrorData::invalid_params(format!("{field} must be a valid UUID: {err}"), None)
    })?;
    Ok(Some(MemoryId::new(id)))
}

/// Remove the reserved call-context arguments [`author_from_args`] read
/// (`model_id` and the caller-self-perspective aliases), so a tool's own
/// argument validation never sees them.
pub fn strip_call_context_args(args: &mut serde_json::Value) {
    let Some(obj) = args.as_object_mut() else {
        return;
    };
    for field in SELF_PERSPECTIVE_ARGS {
        obj.remove(field);
    }
    // `model_id` is the reserved operator label: `author_from_args` has already
    // captured it into the author context, so strip it as a context field. This
    // lets any dispatcher tool (whose per-action specs do not list `model_id`)
    // accept it without a spurious unexpected-field rejection; flat tools that
    // want it (e.g. core_derive) read it from `ctx.author.model_id`.
    obj.remove(MODEL_ID_ARG);
}

/// Whether `scope` admits a host tool, which is flat: its key is its name as
/// a [`ToolName`], and its declaration is the host's, for every audience (the
/// host lists its tools per caller). A served name is a key (the host tool
/// list checked it), so a name that is not is held by no palette.
fn host_tool_in_scope(capabilities: &CapabilitySet, name: &str, effect: ToolEffect) -> bool {
    match ToolName::parse(name) {
        Ok(name) => capabilities.admits(
            &ScopeKey::Tool(name),
            proxima_core::KeyDeclaration::shared(Some(effect)),
        ),
        Err(_) => false,
    }
}

/// Whether this caller may see one host tool: both policies admit it, its
/// declaration permits the owner role, and every listing behavior shows it.
fn host_tool_allowed_for_auth(caller: Caller<'_>, tool: &McpHostTool) -> bool {
    let in_scope = caller.auth.map_or(UNAUTHENTICATED_SCOPE_ALLOWS, |ctx| {
        host_tool_in_scope(ctx.authz.capabilities(), &tool.name, tool.effect)
    });
    in_scope
        && owner_role_allows(caller.auth, tool.effect.is_read_only())
        && caller.behaviors_show(&ToolDescriptorView::host(
            &tool.name,
            &tool.description,
            tool.effect,
        ))
}

/// A host tool's `tools/list` entry; `None` (and a warning) when its
/// input schema is not a JSON object or its output schema admits non-objects.
/// Its output schema goes out as registered tools' do: validation only.
/// Its hints are [`McpToolAnnotations::host`] of its effect, plus the
/// `openWorldHint` it set; its `_meta` goes out as given.
fn host_tool_metadata(mut tool: McpHostTool) -> Option<Tool> {
    let output = match host_wire_output_schema(tool.output_schema.as_mut()) {
        Ok(output) => output,
        Err(error) => {
            tracing::warn!(tool = %tool.name, error = %error, "host tool output schema must describe JSON objects; not listed");
            return None;
        }
    };
    let serde_json::Value::Object(args) = tool.args_schema else {
        tracing::warn!(tool = %tool.name, "host tool input schema must be a JSON object; not listed");
        return None;
    };
    let mut listed = Tool::new(
        Cow::Owned(provider_safe_tool_name(&tool.name)),
        Cow::Owned(tool.description),
        Arc::new(args),
    )
    .annotate(to_rmcp_annotations(McpToolAnnotations {
        open_world: tool.open_world,
        ..McpToolAnnotations::host(tool.effect)
    }));
    if let Some(output) = output {
        listed = listed.with_raw_output_schema(Arc::new(output));
    }
    if let Some(meta) = tool.meta {
        listed = listed.with_meta(MetaObject(meta));
    }
    Some(listed)
}

/// The wire form of a host tool's output schema: `None` for a tool that
/// declares none, the reason for one that does not describe JSON objects.
fn host_wire_output_schema(
    schema: Option<&mut serde_json::Value>,
) -> Result<Option<JsonObject>, String> {
    let Some(schema) = schema else {
        return Ok(None);
    };
    normalize_mcp_output_schema(schema)?;
    match mcp_wire_output_schema(schema) {
        serde_json::Value::Object(wire) => Ok(Some(wire)),
        _ => Err("MCP output schema must be a JSON object".to_owned()),
    }
}

#[cfg(test)]
#[path = "handler/output_contract_tests.rs"]
mod output_contract_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use proxima_core::mcp::Replay;
    use proxima_core::protocol::{
        action as protocol_action, resource as protocol_resource, tool as protocol_tool,
    };
    use proxima_core::{ServedSurface, ToolScope};

    fn palette(ids: &[&str]) -> ToolScope {
        ToolScope::Palette(
            ids.iter()
                .map(|id| ScopeKey::parse(id).expect("a scope key"))
                .collect(),
        )
    }

    // Retain the existing guard and JSON-RPC error assertions through the
    // shared host validator and the production transport error mapper.
    fn reject_nul_in_args(args: &serde_json::Value) -> Result<(), ErrorData> {
        crate::server::reject_nul_in_args(args)
            .map_err(|error| mcp_tool_error_to_error_data(&error))
    }

    fn flavor_descriptor(name: &'static str, effect: Option<ToolEffect>) -> McpToolDescriptor {
        McpToolDescriptor {
            name,
            description: "stub",
            origin: proxima_core::mcp::McpToolOrigin::Flavor("proxima-stub".to_string()),
            produces_schema_ids: &[],
            args_schema: serde_json::json!({"type": "object"}),
            dispatcher_schema: None,
            output_schema: serde_json::json!({"type": "object"}),
            action_arg_specs: &[],
            argv_action_specs: &[],
            effect,
            audience: proxima_core::mcp::McpToolAudience::Shared,
            call: &|_, _| Box::pin(async { Ok(serde_json::json!({})) }),
            title: "Test tool",
        }
    }

    const MIXED_ACTIONS: &[proxima_core::mcp::McpActionArgSpec] = &[
        proxima_core::mcp::McpActionArgSpec {
            action: "look",
            allowed_fields: &["id"],
            required_fields: &["id"],
            effect: ToolEffect::ReadOnly,
            audience: proxima_core::mcp::McpToolAudience::Shared,
            title: "Test action",
        },
        proxima_core::mcp::McpActionArgSpec {
            action: "touch",
            allowed_fields: &["id"],
            required_fields: &["id"],
            effect: ToolEffect::Additive(Replay::NonIdempotent),
            audience: proxima_core::mcp::McpToolAudience::Shared,
            title: "Test action",
        },
    ];

    /// The argv twin of [`MIXED_ACTIONS`]: one command that reads, one that
    /// writes.
    const MIXED_ARGV_ACTIONS: &[proxima_core::mcp::McpArgvActionSpec] = &[
        proxima_core::mcp::McpArgvActionSpec {
            action: "approval",
            argv_prefix: &["approval"],
            effect: ToolEffect::ReadOnly,
            audience: proxima_core::mcp::McpToolAudience::Shared,
            title: "Test action",
        },
        proxima_core::mcp::McpArgvActionSpec {
            action: "approval-decide",
            argv_prefix: &["approval", "decide"],
            effect: ToolEffect::Additive(Replay::NonIdempotent),
            audience: proxima_core::mcp::McpToolAudience::Shared,
            title: "Test action",
        },
    ];

    /// Listing uses the call gate's predicate. Under an empty served palette
    /// an undeclared flat tool is not listed, a read-only one is, and a
    /// mixed dispatcher shows its read action only.
    #[test]
    fn listing_under_an_empty_served_palette_shows_reads_only() {
        let mut auth = full_auth(ToolScope::All);
        auth.authz = auth
            .authz
            .with_served_surface(ServedSurface::Palette(Vec::new()));
        let caller = Caller::new(Some(&auth));

        assert!(!tool_allowed_for_auth(
            caller,
            &flavor_descriptor("stub_undeclared", None)
        ));
        assert!(!tool_allowed_for_auth(
            caller,
            &flavor_descriptor(
                "stub_write",
                Some(ToolEffect::Additive(Replay::NonIdempotent))
            )
        ));
        assert!(tool_allowed_for_auth(
            caller,
            &flavor_descriptor("stub_read", Some(ToolEffect::ReadOnly))
        ));

        let mut mixed = flavor_descriptor("stub_mixed", None);
        mixed.action_arg_specs = MIXED_ACTIONS;
        assert!(tool_allowed_for_auth(caller, &mixed));
        assert!(action_allowed_for_auth(caller, &mixed, "look"));
        assert!(!action_allowed_for_auth(caller, &mixed, "touch"));

        // Owner audience is still a registered read; owner authorization
        // remains independent of deployment command selection.
        let mut owner_read = flavor_descriptor("stub_owner_read", Some(ToolEffect::ReadOnly));
        owner_read.audience = proxima_core::mcp::McpToolAudience::Owner;
        assert!(tool_allowed_for_auth(caller, &owner_read));
        let held = full_auth(ToolScope::Palette(vec![
            ScopeKey::parse("stub_owner_read").expect("a scope key"),
        ]));
        assert!(tool_allowed_for_auth(Caller::new(Some(&held)), &owner_read));

        for resource in all_core_resources() {
            assert!(
                resource_allowed_for_auth(Some(&auth), resource),
                "{} is a read and is listed",
                resource.scope_key
            );
        }
    }

    #[test]
    fn an_empty_caller_palette_hides_every_read_resource_and_guide() {
        let auth = full_auth(ToolScope::Palette(Vec::new()));
        let caller = Caller::new(Some(&auth));
        assert!(!tool_allowed_for_auth(
            caller,
            &flavor_descriptor("stub_read", Some(ToolEffect::ReadOnly))
        ));
        let mut owner_read = flavor_descriptor("stub_owner_read", Some(ToolEffect::ReadOnly));
        owner_read.audience = proxima_core::mcp::McpToolAudience::Owner;
        assert!(!tool_allowed_for_auth(caller, &owner_read));
        let mut mixed = flavor_descriptor("stub_mixed", None);
        mixed.action_arg_specs = MIXED_ACTIONS;
        assert!(!tool_allowed_for_auth(caller, &mixed));
        assert!(!action_allowed_for_auth(caller, &mixed, "look"));
        assert!(!action_allowed_for_auth(caller, &mixed, "touch"));
        assert!(
            all_core_resources().all(|resource| !resource_allowed_for_auth(Some(&auth), resource))
        );
        assert!(!how_to_allowed_for_auth(Some(&auth)));
        assert!(advertised_resource_scope_keys(Some(&auth)).is_empty());
        let host_read = McpHostTool::new(
            "host_read",
            "read",
            serde_json::json!({}),
            ToolEffect::ReadOnly,
        );
        assert!(!host_tool_allowed_for_auth(caller, &host_read));
    }

    /// A resource is listed as it is read: the palette admits it and the
    /// owner role allows what it declares. A caller with no role on the owner
    /// is shown none, under any palette; a resource that is no read needs
    /// the palette.
    #[test]
    fn a_resource_is_listed_only_where_it_is_admitted() {
        let owner = proxima_core::OwnerId::from(proxima_core::UserId::new(uuid::Uuid::now_v7()));
        let stranger = McpAuthContext {
            owner,
            authz: proxima_core::AuthzContext::for_subject(
                proxima_core::UserId::new(uuid::Uuid::now_v7()),
                proxima_core::AuthPath::HostBearer,
            )
            .with_tool_scope(ToolScope::All),
        };
        for resource in all_core_resources() {
            assert!(
                !resource_allowed_for_auth(Some(&stranger), resource),
                "{} is not listed to a caller with no role",
                resource.scope_key
            );
        }

        let memory = all_core_resources()
            .find(|resource| resource.scope_key == protocol_resource::MEMORY)
            .expect("the memory resource");
        let write = proxima_core::flavor::ResourceContract {
            read_only: false,
            ..*memory
        };
        let mut auth = full_auth(ToolScope::All);
        auth.authz = auth
            .authz
            .with_served_surface(ServedSurface::Palette(Vec::new()));
        assert!(resource_allowed_for_auth(Some(&auth), memory));
        assert!(!resource_allowed_for_auth(Some(&auth), &write));
        let empty_caller = full_auth(ToolScope::Palette(Vec::new()));
        assert!(!resource_allowed_for_auth(Some(&empty_caller), memory));
        assert!(!resource_allowed_for_auth(Some(&empty_caller), &write));
        let held = full_auth(ToolScope::Palette(vec![ScopeKey::Resource(write.key())]));
        assert!(resource_allowed_for_auth(Some(&held), &write));
    }

    fn full_auth(scope: ToolScope) -> McpAuthContext {
        let owner = proxima_core::OwnerId::from(proxima_core::UserId::new(uuid::Uuid::now_v7()));
        McpAuthContext {
            owner,
            authz: proxima_core::AuthzContext::single_owner(
                &owner,
                proxima_core::AuthPath::HostBearer,
            )
            .with_tool_scope(scope),
        }
    }

    /// An auth context whose token binds a model identity, as the OIDC
    /// subject map produces for a configured runner principal.
    fn trusted_auth(trusted_model_id: &str) -> McpAuthContext {
        let owner = proxima_core::OwnerId::from(proxima_core::UserId::new(uuid::Uuid::now_v7()));
        McpAuthContext {
            owner,
            authz: proxima_core::AuthzContext::single_owner(
                &owner,
                proxima_core::AuthPath::HostBearer,
            )
            .with_trusted_model_id(trusted_model_id)
            .expect("a well-formed runner id binds"),
        }
    }

    /// Fails when an rmcp upgrade knows a revision newer than the one
    /// Proxima serves: implement the new revision's server obligations
    /// (list cache hints, lifecycle, headers, errors), then raise
    /// `MAX_PROTOCOL_VERSION`. Admitting it by default is how AQS/aquilo#9484
    /// happened.
    #[test]
    fn the_protocol_ceiling_is_rmcps_newest_revision() {
        assert_eq!(
            ProtocolVersion::KNOWN_VERSIONS.last(),
            Some(&MAX_PROTOCOL_VERSION),
            "rmcp knows a revision newer than MAX_PROTOCOL_VERSION"
        );
    }

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
        let args = serde_json::json!({ "action": "set", "model_id": "example-model" });
        let author = author_from_args(&args, None, "unknown", "0").expect("author reads model_id");
        assert_eq!(author.model_id, "example-model");

        let mut args = args;
        strip_call_context_args(&mut args);
        assert!(args.get("model_id").is_none(), "model_id stripped: {args}");
        assert_eq!(args["action"], "set");
    }

    #[test]
    fn a_model_id_that_is_not_a_string_is_invalid_params() {
        for claimed in [
            serde_json::json!(123),
            serde_json::json!({}),
            serde_json::json!(["a"]),
        ] {
            let err = author_from_args(
                &serde_json::json!({ "model_id": claimed }),
                None,
                "unknown",
                "0",
            )
            .expect_err("a label that is not a string is refused, not read as absent");
            assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
            assert!(err.message.contains("model_id"), "{}", err.message);
        }
        let author = author_from_args(
            &serde_json::json!({ "model_id": null }),
            None,
            "unknown",
            "0",
        )
        .expect("null is an absent label");
        assert_eq!(author.model_id, "unknown");
    }

    #[test]
    fn a_bound_model_identity_becomes_the_persisted_label() {
        let auth = trusted_auth("runner/pinned");
        let author = author_from_args(&serde_json::json!({}), Some(&auth), "unknown", "0")
            .expect("no caller claim, no conflict");

        assert_eq!(author.trusted_model_id.as_deref(), Some("runner/pinned"));
        assert_eq!(
            author.model_id, "runner/pinned",
            "the persisted label is the bound identity, not `unknown`"
        );
    }

    #[test]
    fn a_matching_model_id_argument_is_accepted() {
        let auth = trusted_auth("runner/pinned");
        let author = author_from_args(
            &serde_json::json!({ "model_id": "  runner/pinned  " }),
            Some(&auth),
            "unknown",
            "0",
        )
        .expect("an agreeing claim is not a conflict");

        assert_eq!(author.model_id, "runner/pinned");
        assert_eq!(author.trusted_model_id.as_deref(), Some("runner/pinned"));
    }

    #[test]
    fn a_differing_model_id_argument_is_invalid_params() {
        let auth = trusted_auth("runner/pinned");
        let err = author_from_args(
            &serde_json::json!({ "model_id": "claimed/model" }),
            Some(&auth),
            "unknown",
            "0",
        )
        .expect_err("a caller may not relabel an authenticated runner");

        assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
        assert!(err.message.contains("model_id"), "{}", err.message);
        assert!(
            err.message.contains("authenticated token already binds"),
            "{}",
            err.message
        );
    }

    /// Without a bound identity nothing changes: the caller's label stands,
    /// and an unmapped caller that merely *says* a runner name gains no
    /// trusted status from having said it.
    #[test]
    fn an_unmapped_caller_keeps_its_own_label_and_gains_no_trusted_status() {
        let auth = full_auth(ToolScope::All);
        let author = author_from_args(
            &serde_json::json!({ "model_id": "runner/pinned" }),
            Some(&auth),
            "unknown",
            "0",
        )
        .expect("no bound identity, no conflict");

        assert_eq!(author.model_id, "runner/pinned");
        assert_eq!(
            author.trusted_model_id, None,
            "claiming the string is not being bound to it"
        );

        let unattributed = author_from_args(&serde_json::json!({}), Some(&auth), "unknown", "0")
            .expect("author context");
        assert_eq!(unattributed.model_id, "unknown");
        assert_eq!(unattributed.trusted_model_id, None);
    }

    /// A blank argument is no claim, matching REST — where an empty header
    /// is dropped before it can mean anything. Without the shared rule the
    /// same request would be an error here and `unknown` there.
    #[test]
    fn a_blank_model_id_argument_is_absent_on_both_paths() {
        for blank in ["", "   "] {
            let unbound = author_from_args(
                &serde_json::json!({ "model_id": blank }),
                Some(&full_auth(ToolScope::All)),
                "unknown",
                "0",
            )
            .expect("a blank argument is absent, not an empty label");
            assert_eq!(unbound.model_id, "unknown", "blank {blank:?}");

            let bound = author_from_args(
                &serde_json::json!({ "model_id": blank }),
                Some(&trusted_auth("runner/pinned")),
                "unknown",
                "0",
            )
            .expect("a blank argument cannot conflict");
            assert_eq!(bound.model_id, "runner/pinned", "blank {blank:?}");
        }
    }

    /// `clientInfo` is peer-declared and unauthenticated; it names the
    /// client, never the trusted model.
    #[test]
    fn client_info_never_becomes_the_trusted_model_id() {
        let auth = full_auth(ToolScope::All);
        let author = author_from_args(
            &serde_json::json!({}),
            Some(&auth),
            "runner/pinned",
            "1.0.0",
        )
        .expect("author context");

        assert_eq!(author.client_name, "runner/pinned");
        assert_eq!(author.trusted_model_id, None);
        assert_eq!(author.model_id, "unknown");
    }

    /// Resource reads take the same precedence, with no argument object to
    /// claim from.
    #[test]
    fn resource_author_context_carries_the_bound_identity() {
        let bound = author_from_ctx(Some(&trusted_auth("runner/pinned")), "unknown", "0");
        assert_eq!(bound.model_id, "runner/pinned");
        assert_eq!(bound.trusted_model_id.as_deref(), Some("runner/pinned"));

        let unbound = author_from_ctx(Some(&full_auth(ToolScope::All)), "unknown", "0");
        assert_eq!(unbound.model_id, "unknown");
        assert_eq!(unbound.trusted_model_id, None);
    }

    /// A bound identity is captured into the author context and the argument
    /// is still stripped, so a dispatcher tool never sees it as an
    /// unexpected field.
    #[test]
    fn strip_call_context_args_still_removes_an_agreeing_model_id() {
        let auth = trusted_auth("runner/pinned");
        let mut args = serde_json::json!({ "action": "set", "model_id": "runner/pinned" });
        let author =
            author_from_args(&args, Some(&auth), "unknown", "0").expect("agreeing claim accepted");
        assert_eq!(author.model_id, "runner/pinned");

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
        let registry = proxima_core::FlavorRegistry::new().freeze_or_panic_for_tests();
        let goal = registry
            .list_mcp_tools()
            .iter()
            .find(|descriptor| descriptor.name == protocol_tool::CORE_GOAL)
            .expect("core_goal descriptor")
            .clone();

        // A palette that permits only the `set` leaf of core_goal.
        let scope = palette(&[protocol_action::CORE_GOAL_SET]);
        let auth = full_auth(scope);
        let projected = project_dispatcher_actions(&goal, Some(auth.authz.capabilities()));

        let enum_values = projected
            .pointer("/properties/action/enum")
            .and_then(serde_json::Value::as_array)
            .expect("action enum");
        assert_eq!(enum_values, &vec![serde_json::json!("set")]);

        // Only `set`'s fields and prose: no field another action alone
        // takes, no guide line for an action the palette withholds.
        let advertised: std::collections::BTreeSet<&str> = projected["properties"]
            .as_object()
            .expect("properties")
            .keys()
            .map(String::as_str)
            .collect();
        let mut expected: std::collections::BTreeSet<&str> = goal
            .action_arg_specs
            .iter()
            .find(|spec| spec.action == "set")
            .expect("set spec")
            .allowed_fields
            .iter()
            .copied()
            .collect();
        expected.insert("action");
        assert_eq!(advertised, expected);
        let guide = projected["properties"]["action"]["description"]
            .as_str()
            .expect("action guide");
        assert!(guide.contains("\n- set: "), "{guide}");
        assert!(!guide.contains("\n- transition:"), "{guide}");
        assert!(
            !projected["properties"]["evidence"]["description"]
                .as_str()
                .expect("evidence prose")
                .contains("mark_achieved"),
            "{projected:#}"
        );
    }

    #[test]
    fn all_scope_leaves_dispatcher_actions_unchanged() {
        let registry = proxima_core::FlavorRegistry::new().freeze_or_panic_for_tests();
        let goal = registry
            .list_mcp_tools()
            .iter()
            .find(|descriptor| descriptor.name == protocol_tool::CORE_GOAL)
            .expect("core_goal descriptor")
            .clone();
        let projected = project_dispatcher_actions(&goal, Some(&CapabilitySet::all()));
        assert_eq!(projected, goal.args_schema);
    }

    #[test]
    fn not_authorized_message_lists_allowed_actions() {
        let registry = proxima_core::FlavorRegistry::new().freeze_or_panic_for_tests();
        let scope = palette(&[protocol_action::CORE_GOAL_SET]);
        let auth = full_auth(scope);
        let err = tool_invocation_error_to_error_data(
            &registry,
            McpToolError::NotAuthorized(protocol_action::CORE_GOAL_TRANSITION.into()).into(),
            Some(&auth),
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

    /// The retry hint reaches a FLAVOR dispatcher too. Built from
    /// `all_core_actions()` — a table over substrate names — this message
    /// listed nothing for a flavor tool, so the caller holding a partial
    /// palette on a tool the substrate never heard of got no hint at all.
    #[test]
    fn not_authorized_lists_a_flavor_dispatchers_allowed_actions() {
        use futures_util::future::BoxFuture;
        use proxima_core::mcp::{McpActionArgSpec, McpTool, McpToolCtx, McpToolError};

        #[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
        #[serde(tag = "action", rename_all = "snake_case")]
        #[expect(dead_code, reason = "the derived schema is the subject")]
        enum StubArgs {
            Look {
                #[schemars(description = "Which thing to look at.")]
                id: String,
            },
            Touch {
                #[schemars(description = "Which thing to touch.")]
                id: String,
            },
        }

        #[derive(Debug)]
        struct StubDispatchTool;

        #[derive(Debug, serde::Serialize, schemars::JsonSchema)]
        struct StubOutput {}

        impl McpTool for StubDispatchTool {
            const TITLE: &'static str = "Test tool";
            const NAME: &'static str = "proxima-stub_dispatch";
            const DESCRIPTION: &'static str = "A flavor dispatcher.";
            const ACTION_ARG_SPECS: &'static [McpActionArgSpec] = &[
                McpActionArgSpec {
                    action: "look",
                    allowed_fields: &["id"],
                    required_fields: &["id"],
                    effect: ToolEffect::Additive(Replay::NonIdempotent),
                    audience: proxima_core::mcp::McpToolAudience::Shared,
                    title: "Test action",
                },
                McpActionArgSpec {
                    action: "touch",
                    allowed_fields: &["id"],
                    required_fields: &["id"],
                    effect: ToolEffect::Additive(Replay::NonIdempotent),
                    audience: proxima_core::mcp::McpToolAudience::Shared,
                    title: "Test action",
                },
            ];
            type Args = StubArgs;
            type Output = StubOutput;
            fn call(
                _: McpToolCtx,
                _: Self::Args,
            ) -> BoxFuture<'static, Result<Self::Output, McpToolError>> {
                Box::pin(async { Ok(StubOutput {}) })
            }
        }

        let mut registry = proxima_core::FlavorRegistry::new();
        registry.add_mcp_tool_or_panic_for_tests::<StubDispatchTool>("proxima-stub");
        let registry = registry.freeze_or_panic_for_tests();

        let scope = palette(&["proxima-stub_dispatch:look"]);
        let auth = full_auth(scope);
        let err = tool_invocation_error_to_error_data(
            &registry,
            McpToolError::NotAuthorized("proxima-stub_dispatch:touch".into()).into(),
            Some(&auth),
        );
        assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_REQUEST);
        assert!(
            err.message
                .contains("allowed proxima-stub_dispatch actions: look"),
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

    /// #480: an other store that did not answer is a retryable -32000 with
    /// `data.code` `store_unavailable` naming the store, never -32602 (which
    /// a client reads as "no such memory") or -32600.
    #[test]
    fn an_unanswered_store_is_a_server_error_naming_the_store() {
        let err = mcp_tool_error_to_error_data(&McpToolError::StoreUnavailable { store: 1 });
        assert_eq!(err.code, SERVER_ERROR);
        assert_ne!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
        assert_ne!(err.code, rmcp::model::ErrorCode::INVALID_REQUEST);
        assert_eq!(err.message, "store 1 unavailable");
        assert_eq!(
            err.data,
            Some(serde_json::json!({ "code": "store_unavailable", "store": 1 }))
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
    fn tool_output_serialization_errors_are_internal_and_redacted() {
        let err = mcp_tool_error_to_error_data(&McpToolError::Other(
            "serialize tool output: fixture secret from output serializer".into(),
        ));
        assert_eq!(err.code, rmcp::model::ErrorCode::INTERNAL_ERROR);
        assert_eq!(err.message, "internal server error");
        assert!(!err.message.contains("fixture secret"));
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

    #[test]
    fn tool_scope_denials_remain_invalid_request() {
        // No auth threaded (e.g. direct-handler path) → bare message, no
        // allowed-action enrichment.
        let registry = proxima_core::FlavorRegistry::new().freeze_or_panic_for_tests();
        let err = tool_invocation_error_to_error_data(
            &registry,
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
    /// Read-vs-write comes from the descriptor's own effect, never from its
    /// name.
    #[test]
    fn a_viewer_sees_a_flavor_read_tool_and_not_its_write_tool() {
        use proxima_core::{AuthPath, AuthzContext, GroupId, OwnerId, UserId, access::Role};

        let owner = OwnerId::from(GroupId::new(uuid::Uuid::now_v7()));
        let viewer = McpAuthContext {
            owner,
            authz: AuthzContext::for_subject_with_role(
                UserId::new(uuid::Uuid::now_v7()),
                [(owner, Role::viewer())],
                AuthPath::HostBearer,
            ),
        };

        let read = flavor_descriptor("proxima-stub_search", Some(ToolEffect::ReadOnly));
        let write = flavor_descriptor(
            "proxima-stub_write",
            Some(ToolEffect::Additive(Replay::NonIdempotent)),
        );
        // No name-keyed fallback: a substrate descriptor that declares
        // nothing is a write, whatever its name.
        let silent_core = McpToolDescriptor {
            origin: proxima_core::mcp::McpToolOrigin::Substrate,
            ..flavor_descriptor(protocol_tool::CORE_SEARCH_MEMORIES, None)
        };
        let registry = proxima_core::FlavorRegistry::new().freeze_or_panic_for_tests();
        let core_read = registry
            .mcp_tool(protocol_tool::CORE_SEARCH_MEMORIES)
            .expect("core search is registered");

        assert!(
            tool_allowed_for_auth(Caller::new(Some(&viewer)), &read),
            "a flavor tool that declares read_only must be visible to a viewer"
        );
        assert!(
            !tool_allowed_for_auth(Caller::new(Some(&viewer)), &write),
            "a flavor write tool must stay hidden from a viewer"
        );
        assert!(
            !tool_allowed_for_auth(Caller::new(Some(&viewer)), &silent_core),
            "a name is not a declaration"
        );
        assert!(
            tool_allowed_for_auth(Caller::new(Some(&viewer)), core_read),
            "core search declares its own read effect"
        );
    }

    #[test]
    fn a_bogus_action_scope_never_advertises_a_flat_tool() {
        use proxima_core::{AuthPath, AuthzContext, OwnerId, UserId};

        let owner = OwnerId::from(UserId::new(uuid::Uuid::now_v7()));
        let descriptor = flavor_descriptor("proxima-stub_search", Some(ToolEffect::ReadOnly));
        let auth = McpAuthContext {
            owner,
            authz: AuthzContext::for_subject(
                UserId::new(uuid::Uuid::now_v7()),
                AuthPath::HostBearer,
            )
            .with_tool_scope(palette(&["proxima-stub_search:bogus"])),
        };

        assert!(descriptor.action_arg_specs.is_empty());
        assert!(!tool_allowed_for_auth(
            Caller::new(Some(&auth)),
            &descriptor
        ));
    }

    #[test]
    fn a_viewer_sees_only_the_read_leaf_of_a_mixed_dispatcher() {
        use proxima_core::{AuthPath, AuthzContext, GroupId, OwnerId, UserId, access::Role};

        let owner = OwnerId::from(GroupId::new(uuid::Uuid::now_v7()));
        let viewer = McpAuthContext {
            owner,
            authz: AuthzContext::for_subject_with_role(
                UserId::new(uuid::Uuid::now_v7()),
                [(owner, Role::viewer())],
                AuthPath::HostBearer,
            ),
        };
        let no_fields = serde_json::json!({
            "type": "object", "properties": {}, "additionalProperties": false
        });
        let dispatcher = proxima_core::mcp::McpDispatcherSchema {
            discriminator: "action".to_owned(),
            description: None,
            actions: ["look", "touch"]
                .map(|action| proxima_core::mcp::McpActionSchema {
                    action: action.to_owned(),
                    description: None,
                    argument_schema: no_fields.clone(),
                })
                .to_vec(),
        };
        let mixed = McpToolDescriptor {
            args_schema: dispatcher.render(|_| true),
            dispatcher_schema: Some(dispatcher),
            action_arg_specs: MIXED_ACTIONS,
            // Parent says read; the write action must not inherit it.
            ..flavor_descriptor("proxima-stub_dispatch", Some(ToolEffect::ReadOnly))
        };
        assert!(
            tool_allowed_for_auth(Caller::new(Some(&viewer)), &mixed),
            "the read action keeps a mixed dispatcher visible"
        );
        let viewer_schema = project_dispatcher_actions_for_auth(Caller::new(Some(&viewer)), &mixed);
        assert_eq!(
            viewer_schema["properties"]["action"]["enum"],
            serde_json::json!(["look"]),
        );
        assert_eq!(
            annotations_for_auth(Caller::new(Some(&viewer)), &mixed)
                .and_then(|value| value.read_only),
            Some(true),
        );

        let writer = McpAuthContext {
            owner,
            authz: AuthzContext::for_subject_with_role(
                UserId::new(uuid::Uuid::now_v7()),
                [(owner, Role::editor())],
                AuthPath::HostBearer,
            )
            .with_tool_scope(ToolScope::All),
        };
        assert_eq!(
            project_dispatcher_actions_for_auth(Caller::new(Some(&writer)), &mixed)["properties"]["action"]
                ["enum"],
            serde_json::json!(["look", "touch"]),
        );
        assert_eq!(
            annotations_for_auth(Caller::new(Some(&writer)), &mixed)
                .and_then(|value| value.read_only),
            Some(false),
            "a mixed dispatcher is conservatively a write when both actions are visible",
        );
    }

    #[test]
    fn scoped_dispatcher_projection_preserves_allowed_argument_schema_metadata() {
        use proxima_core::access::Role;
        use proxima_core::{AuthPath, AuthzContext, FlavorRegistry, GroupId, OwnerId, UserId};

        let registry = FlavorRegistry::default().freeze_or_panic_for_tests();
        let descriptor = registry
            .list_mcp_tools()
            .iter()
            .find(|tool| tool.name == protocol_tool::CORE_MEMBERSHIP)
            .expect("core membership dispatcher");
        let owner = OwnerId::from(GroupId::new(uuid::Uuid::now_v7()));
        let viewer = McpAuthContext {
            owner,
            authz: AuthzContext::for_subject_with_role(
                UserId::new(uuid::Uuid::now_v7()),
                [(owner, Role::viewer())],
                AuthPath::HostBearer,
            ),
        };
        let projected = project_dispatcher_actions_for_auth(Caller::new(Some(&viewer)), descriptor);
        assert_eq!(
            projected["properties"]["action"]["enum"],
            serde_json::json!(["list_members"])
        );
        // The viewer's schema is `list_members`' own: its field, as declared,
        // and nothing `add_member` / `remove_member` alone take.
        let list_members = descriptor
            .action_argument_schema("list_members")
            .expect("list_members argument schema");
        let fields: Vec<&String> = projected["properties"]
            .as_object()
            .expect("properties")
            .keys()
            .filter(|field| *field != "action")
            .collect();
        assert_eq!(
            fields,
            list_members["properties"]
                .as_object()
                .expect("list_members properties")
                .keys()
                .collect::<Vec<_>>()
        );
        assert_eq!(
            projected["properties"]["group"],
            list_members["properties"]["group"]
        );
    }

    /// The argv vocabulary classifies per command too, and an unannotated
    /// command still falls back to the tool.
    ///
    /// The motivating shape is a dispatcher whose argv-keyed commands are
    /// mostly reads under a tool that must call itself writable because
    /// some of them write. Classifying the whole tool cost a
    /// read-capable-only owner every one of those reads.
    #[test]
    fn a_viewer_keeps_the_annotated_read_command_of_an_argv_dispatcher() {
        use proxima_core::{
            AuthPath, AuthzContext, GroupId, OwnerId, ToolScope, UserId, access::Role,
        };

        let owner = OwnerId::from(GroupId::new(uuid::Uuid::now_v7()));
        let viewer = McpAuthContext {
            owner,
            authz: AuthzContext::for_subject_with_role(
                UserId::new(uuid::Uuid::now_v7()),
                [(owner, Role::viewer())],
                AuthPath::HostBearer,
            )
            .with_tool_scope(ToolScope::All),
        };
        let cli = McpToolDescriptor {
            argv_action_specs: MIXED_ARGV_ACTIONS,
            // The tool writes; only the annotated command opts out.
            ..flavor_descriptor(
                "proxima-stub_cli",
                Some(ToolEffect::Additive(Replay::NonIdempotent)),
            )
        };

        assert!(
            action_allowed_for_auth(Caller::new(Some(&viewer)), &cli, "approval"),
            "an argv command that declares read_only must stay callable by a viewer"
        );
        assert!(
            !action_allowed_for_auth(Caller::new(Some(&viewer)), &cli, "approval-decide"),
            "an argv command that declares nothing classifies from the tool, which writes"
        );
        assert!(
            tool_allowed_for_auth(Caller::new(Some(&viewer)), &cli),
            "the read command keeps the dispatcher visible"
        );
        assert_eq!(
            annotations_for_auth(Caller::new(Some(&viewer)), &cli)
                .and_then(|value| value.read_only),
            Some(true),
            "only the read command is visible to a viewer",
        );

        let writer = McpAuthContext {
            owner,
            authz: AuthzContext::for_subject_with_role(
                UserId::new(uuid::Uuid::now_v7()),
                [(owner, Role::editor())],
                AuthPath::HostBearer,
            )
            .with_tool_scope(ToolScope::All),
        };
        assert_eq!(
            annotations_for_auth(Caller::new(Some(&writer)), &cli)
                .and_then(|value| value.read_only),
            Some(false),
            "a mixed argv dispatcher is conservatively a write when both commands are visible",
        );
    }

    /// An argv command classifies from its own effect, never the tool's:
    /// even a hand-built descriptor carrying a read-only tool-level effect
    /// (which `try_freeze` refuses on a dispatcher) lends it to no command.
    #[test]
    fn an_argv_command_classifies_from_its_own_effect_never_the_tools() {
        const WRITE: &[proxima_core::mcp::McpArgvActionSpec] =
            &[proxima_core::mcp::McpArgvActionSpec {
                action: "approval",
                argv_prefix: &["approval"],
                effect: ToolEffect::Additive(Replay::NonIdempotent),
                audience: proxima_core::mcp::McpToolAudience::Shared,
                title: "Test action",
            }];

        let tool = McpToolDescriptor {
            argv_action_specs: WRITE,
            ..flavor_descriptor("proxima-stub_cli", Some(ToolEffect::ReadOnly))
        };
        assert!(!tool.action_is_read_only("approval"));
        assert!(
            !tool.action_is_read_only("not-a-command"),
            "a key the vocabulary does not emit is a write"
        );
        assert_eq!(
            tool.effect(),
            Some(ToolEffect::Additive(Replay::NonIdempotent)),
            "the tool-level effect is its commands' join"
        );
    }

    #[test]
    fn denial_hints_exclude_actions_forbidden_by_the_owner_role() {
        use proxima_core::{AuthPath, AuthzContext, GroupId, OwnerId, UserId, access::Role};

        let registry = proxima_core::FlavorRegistry::new().freeze_or_panic_for_tests();
        let owner = OwnerId::from(GroupId::new(uuid::Uuid::now_v7()));
        let viewer = McpAuthContext {
            owner,
            authz: AuthzContext::for_subject_with_role(
                UserId::new(uuid::Uuid::now_v7()),
                [(owner, Role::viewer())],
                AuthPath::HostBearer,
            )
            .with_tool_scope(ToolScope::All),
        };
        let err = tool_invocation_error_to_error_data(
            &registry,
            McpToolError::NotAuthorized(protocol_action::CORE_MEMBERSHIP_ADD_MEMBER.into()).into(),
            Some(&viewer),
        );

        assert_eq!(
            err.message,
            "tool core_membership:add_member not authorized for this MCP token; allowed \
             core_membership actions: list_members",
        );
    }

    // Completeness gate: every substrate tool resolves an effect from its
    // descriptor — a flat tool's `EFFECT`, a dispatcher's join over its
    // action specs.
    #[test]
    fn every_core_tool_is_annotated() {
        let registry = proxima_core::FlavorRegistry::new().freeze_or_panic_for_tests();
        for descriptor in registry.list_mcp_tools() {
            if descriptor.name.starts_with("core_") {
                assert!(
                    descriptor.annotations().is_some(),
                    "core tool {} has no resolvable MCP annotations",
                    descriptor.name
                );
            }
        }
    }

    #[test]
    fn core_tool_effects_encode_expected_semantics() {
        let registry = proxima_core::FlavorRegistry::new().freeze_or_panic_for_tests();
        let hints = |name: &str| {
            registry
                .mcp_tool(name)
                .and_then(McpToolDescriptor::annotations)
                .unwrap_or_else(|| panic!("{name} resolves hints"))
        };

        // Closed substrate: open_world is always false.
        let read = hints(protocol_tool::CORE_SEARCH_MEMORIES);
        assert_eq!(read.read_only, Some(true));
        assert_eq!(read.open_world, Some(false));

        // Convergent additive write (required idempotency key).
        let derive = hints(protocol_tool::CORE_DERIVE);
        assert_eq!(derive.read_only, Some(false));
        assert_eq!(derive.destructive, Some(false));
        assert_eq!(derive.idempotent, Some(true));

        // Additive write with an OPTIONAL idempotency key: identical args
        // without a key create a new Fact, so it is not replay-safe.
        let remember = hints(protocol_tool::CORE_REMEMBER);
        assert_eq!(remember.read_only, Some(false));
        assert_eq!(remember.destructive, Some(false));
        assert_eq!(remember.idempotent, Some(false));

        // Grouped fact dispatcher contains only citation reads; the answer
        // is the join of its action specs, not duplicated here.
        assert_eq!(hints(protocol_tool::CORE_FACT).read_only, Some(true));

        // Idempotent by content: the interpretation's memory id folds the
        // claim, its confidence and its subjects, so re-asserting the same
        // judgment lands on one memory rather than a pile of duplicates.
        let interpret = hints(protocol_tool::CORE_INTERPRET);
        assert_eq!(interpret.read_only, Some(false));
        assert_eq!(interpret.destructive, Some(false));
        assert_eq!(interpret.idempotent, Some(true));

        // Grouped goal dispatcher joins write actions with mixed
        // idempotence: one non-idempotent action makes the tool one.
        let goal = hints(protocol_tool::CORE_GOAL);
        assert_eq!(goal.read_only, Some(false));
        assert_eq!(goal.destructive, Some(false));
        assert_eq!(goal.idempotent, Some(false));

        // One destructive action (`remove_member`) makes the dispatcher
        // destructive, so a client asks before auto-approving any of it.
        let membership = hints(protocol_tool::CORE_MEMBERSHIP);
        assert_eq!(membership.read_only, Some(false));
        assert_eq!(membership.destructive, Some(true));

        // Flavor-shipped / unknown tools are not in the substrate registry.
        assert!(registry.mcp_tool("company/upsert").is_none());
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

    /// Deepest argument tree a request can carry: `serde_json` rejects
    /// nesting at depth 128, so 127 is the maximum.
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
