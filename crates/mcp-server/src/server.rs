use std::sync::Arc;

#[cfg(test)]
use proxima_core::AuthPath;
use proxima_core::flavor::flavor0::resource as core_resource;
use proxima_core::mcp::core_tools::{
    get_graph::{GetGraphArgs, get_graph},
    get_memories::{GetMemoriesArgs, get_memories},
    get_memory::{GetMemoryArgs, get_memory},
    goal_reads::{ListGoalsArgs, get_goal, list_goals},
    list_change_events::{ListChangeEventsArgs, list_change_events},
    list_schemas::{ListSchemasArgs, get_schema, list_schemas},
    list_substrate_tools::{ListSubstrateToolsArgs, list_substrate_tools},
    list_wake_candidates::{ListWakeCandidatesArgs, list_wake_candidates},
    walk_memory_lineage::{
        WalkMemoryLineageArgs, WalkMemoryLineageDirectionArg, walk_memory_lineage,
    },
};
use proxima_core::mcp::{
    McpAuthorContext, McpHostToolCall, McpToolCtx, McpToolDescriptor, McpToolError,
    McpToolErrorKind, Next, TerminalDispatch, ToolCall, ToolReply, resolve_operator_label,
    tool_name_matches,
};
use proxima_core::protocol::resource as protocol_resource;
use proxima_core::{
    Engine, FlavorRegistry, FlavorRegistryFrozen, FlavorServices, StorageError,
    composed_schema_names,
};
use serde::Serialize;

use crate::auth::McpAuthContext;
use crate::host_tools::{McpHostTool, McpHostTools};
use crate::request_scope::RequestHeaderAllowlist;
use crate::tool_list::ToolListNotifier;

#[derive(Clone)]
pub struct McpToolHost {
    registry: Arc<FlavorRegistryFrozen>,
    services: FlavorServices,
    engine: Option<Arc<Engine>>,
    request_headers: RequestHeaderAllowlist,
    host_tools: Option<Arc<dyn McpHostTools>>,
    record_calls: bool,
    /// Bounds call-record writes in flight; a saturated host drops the
    /// record (and says so) rather than queueing without limit.
    record_permits: Arc<tokio::sync::Semaphore>,
    tool_list: Option<ToolListNotifier>,
}

/// Longest host tool name served.
pub const MAX_HOST_TOOL_NAME_CHARS: usize = 128;

/// Call-record writes in flight per tool host.
const RECORD_PERMITS: usize = 64;

impl std::fmt::Debug for McpToolHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpToolHost")
            .field("has_engine", &self.engine.is_some())
            .field("host_tools", &self.host_tools)
            .field("record_calls", &self.record_calls)
            .field("tool_list_notifications", &self.tool_list.is_some())
            .finish_non_exhaustive()
    }
}

impl McpToolHost {
    #[must_use]
    pub fn from_parts(registry: Arc<FlavorRegistryFrozen>, services: FlavorServices) -> Self {
        Self {
            registry,
            services,
            engine: None,
            request_headers: RequestHeaderAllowlist::default(),
            host_tools: None,
            record_calls: false,
            record_permits: Arc::new(tokio::sync::Semaphore::new(RECORD_PERMITS)),
            tool_list: None,
        }
    }

    #[must_use]
    pub fn from_engine(engine: Arc<Engine>, services: FlavorServices) -> Self {
        Self::from_parts(Arc::new(engine.registry().clone()), services).with_engine(engine)
    }

    #[must_use]
    pub fn with_engine(mut self, engine: Arc<Engine>) -> Self {
        self.engine = Some(engine);
        self
    }

    /// Serve `tools` beside the registry's (see [`McpHostTools`]).
    #[must_use]
    pub fn with_host_tools(mut self, tools: Arc<dyn McpHostTools>) -> Self {
        self.host_tools = Some(tools);
        self
    }

    /// Record every MCP `tools/call` as a `core/mcp-call-logged-v1` Fact
    /// under the call's owner: tool, outcome, latency, and the verified
    /// subject as the actor; no request or response body. Needs an engine;
    /// the write authorizes against the caller's own context, so a caller
    /// that cannot write the owner (a viewer) is not recorded. Default: off.
    #[must_use]
    pub fn with_call_recording(mut self, record: bool) -> Self {
        self.record_calls = record;
        self
    }

    /// Advertise `tools.listChanged` and register every caller with
    /// `notifier` under its owner, so [`ToolListNotifier::notify`] reaches
    /// that owner's connected clients. A host attaches one only if it calls
    /// `notify` whenever an owner's tool list changes: the capability is a
    /// promise to. Default: none.
    #[must_use]
    pub fn with_tool_list_notifier(mut self, notifier: ToolListNotifier) -> Self {
        self.tool_list = Some(notifier);
        self
    }

    pub(crate) const fn tool_list_notifier(&self) -> Option<&ToolListNotifier> {
        self.tool_list.as_ref()
    }

    /// Whether [`Self::with_call_recording`] is on.
    #[must_use]
    pub const fn records_calls(&self) -> bool {
        self.record_calls
    }

    pub(crate) fn record_permit(&self) -> Option<tokio::sync::OwnedSemaphorePermit> {
        Arc::clone(&self.record_permits).try_acquire_owned().ok()
    }

    /// The engine tools run against, when one was attached.
    #[must_use]
    pub fn engine(&self) -> Option<&Arc<Engine>> {
        self.engine.as_ref()
    }

    /// The host tools `auth` may be shown, before palette, owner-role and
    /// behavior filtering: [`McpHostTools::list`] minus every name that is not
    /// 1..=[`MAX_HOST_TOOL_NAME_CHARS`] characters of `[A-Za-z0-9_.-]`
    /// (so its wire name is itself, and it can be neither a `tool:action`
    /// leaf nor a `resource:` key), that a registry tool serves under its
    /// canonical or wire name, or that the list already named. One call
    /// reads the source once: a request that needs the catalog holds it.
    ///
    /// # Errors
    ///
    /// The source's [`McpToolError`], unchanged; no host source means an
    /// empty catalog, a failing one never does.
    pub async fn host_tools_for(
        &self,
        auth: &McpAuthContext,
    ) -> Result<Vec<McpHostTool>, McpToolError> {
        let Some(source) = &self.host_tools else {
            return Ok(Vec::new());
        };
        let mut served: Vec<McpHostTool> = Vec::new();
        for tool in source.list(auth).await? {
            match host_tool_name_refusal(&self.registry, &tool.name, &served) {
                Some(reason) => tracing::warn!(tool = %tool.name, reason, "host tool not served"),
                None => served.push(tool),
            }
        }
        Ok(served)
    }

    /// The host's contribution to the `instructions` served to `auth`
    /// ([`McpHostTools::instructions`]); `None` without a host source.
    pub(crate) async fn host_instructions(&self, auth: &McpAuthContext) -> Option<String> {
        self.host_tools.as_ref()?.instructions(auth).await
    }

    /// Copy the allowlisted inbound headers of each served call into its
    /// [`RequestHeaders`](proxima_core::RequestHeaders) service. Default:
    /// none.
    #[must_use]
    pub fn with_request_headers(mut self, allowlist: RequestHeaderAllowlist) -> Self {
        self.request_headers = allowlist;
        self
    }

    /// The per-request service set of one served call: the
    /// [`FlavorServices`] a host middleware placed in the request's
    /// extensions, plus the allowlisted headers. Merged onto the boot set by
    /// [`Self::call_tool_in_request`] / [`Self::read_resource_in_request`].
    ///
    /// # Errors
    ///
    /// [`McpToolError::InvalidInput`] for a repeated or non-ASCII allowlisted
    /// header; [`McpToolError::Other`] when the extension bag carries a
    /// `RequestHeaders` of its own.
    pub fn request_services(
        &self,
        headers: &http::HeaderMap,
        extensions: &http::Extensions,
    ) -> Result<FlavorServices, McpToolError> {
        crate::request_scope::request_services(&self.request_headers, headers, extensions)
    }

    /// Connect a runtime role and a separately authorized migration/platform
    /// role. A single DSN cannot serve both purposes once owner RLS is active.
    ///
    /// # Errors
    ///
    /// Returns storage, migration, registry, or platform-admission failures.
    pub async fn from_database_urls(
        runtime_url: &str,
        platform_url: &str,
        registry: FlavorRegistry,
    ) -> Result<Self, crate::McpServerError> {
        let pool_config = proxima_storage_pg::PgPoolConfig::from_env()?;
        let tuning = proxima_storage_pg::PgTuning::from_env()?;
        let migration = proxima_storage_pg::PgStorage::connect_for_migrations_with_config(
            platform_url,
            pool_config,
            tuning,
        )
        .await?;
        migration.run_migrations().await?;
        let frozen = registry.try_freeze()?;
        let schema_names = composed_schema_names(&frozen);
        let schema_refs: Vec<&str> = schema_names.iter().map(String::as_str).collect();
        let platform_pool = sqlx::PgPool::connect(platform_url)
            .await
            .map_err(|error| StorageError::Unavailable(error.to_string()))?;
        let platform =
            proxima_storage_pg::PgPlatformScope::new(platform_pool, &schema_refs).await?;
        let pg =
            proxima_storage_pg::PgStorage::connect_with_config(runtime_url, pool_config, tuning)
                .await?
                .with_platform_scope(platform);
        proxima_storage_pg::assert_runtime_rls(&pg.clone_pool_for_backend(), &schema_refs).await?;
        let engine =
            Arc::new(Engine::new(frozen.clone()).with_storage_ports(Arc::new(pg).storage_ports()));
        Ok(Self::from_engine(engine, FlavorServices::default()))
    }

    #[must_use]
    pub fn registry(&self) -> &FlavorRegistryFrozen {
        &self.registry
    }

    /// Build a per-call `McpToolCtx` derived from the auth regime.
    ///
    /// All references cross the wire as typed prefixed uuids
    /// (`F:`/`A:`/`P:`/`G:`).
    ///
    /// The author context is re-reconciled against the authenticated one
    /// rather than trusted as given. Both halves of the invariant
    /// [`McpAuthorContext`] documents — the bound identity, and
    /// `model_id` equalling it when present — are restored here, because
    /// an out-of-tree host builds this struct by hand and
    /// [`ToolCaller`](proxima_core::ToolCaller) hands `model_id` on to
    /// flavor tools as the label to record.
    ///
    /// # Errors
    ///
    /// [`McpToolError::InvalidInput`] when the author names a model other
    /// than the one the authenticated token binds. In-tree transports
    /// resolved that at the edge already, so reaching this is a host
    /// assembling an author context the credential does not support.
    pub fn ctx_for(
        &self,
        author: McpAuthorContext,
        auth: &McpAuthContext,
    ) -> Result<McpToolCtx, McpToolError> {
        self.ctx_for_request(author, auth, FlavorServices::default())
    }

    /// [`Self::ctx_for`] with a per-request service set merged onto the boot
    /// set ([`FlavorServices::try_extend`]).
    ///
    /// # Errors
    ///
    /// As [`Self::ctx_for`], and [`McpToolError::Other`] when `request`
    /// repeats a type the boot set already holds — a host wiring fault, never
    /// a caller one, so a request can never replace a boot service.
    pub fn ctx_for_request(
        &self,
        mut author: McpAuthorContext,
        auth: &McpAuthContext,
        request: FlavorServices,
    ) -> Result<McpToolCtx, McpToolError> {
        let mut services = self.services.clone();
        services
            .try_extend(request)
            .map_err(|err| McpToolError::Other(format!("request services: {err}")))?;
        let owner = auth.owner;
        let authz = auth.authz.clone();
        let trusted = authz.trusted_model_id();
        author.model_id = resolve_operator_label(trusted, Some(&author.model_id))
            .map_err(|conflict| McpToolError::InvalidInput(conflict.detail("model_id")))?;
        author.trusted_model_id = trusted.map(ToString::to_string);
        Ok(McpToolCtx {
            owner,
            authz,
            registry: self.registry.clone(),
            caller_self_perspective: author.caller_self_perspective,
            services,
            author,
            engine: self.engine.clone(),
        })
    }

    /// # Errors
    ///
    /// Returns `ToolNotFound`, the called tool error, or an internal error
    /// when the final reply after request behaviors is not a JSON object.
    pub async fn call_tool(
        &self,
        name: &str,
        args: serde_json::Value,
        author: McpAuthorContext,
        auth: Option<McpAuthContext>,
    ) -> Result<serde_json::Value, ToolInvocationError> {
        self.call_tool_in_request(name, args, author, auth, FlavorServices::default())
            .await
    }

    /// [`Self::call_tool`] with a per-request service set (see
    /// [`Self::request_services`]) merged onto the boot set.
    ///
    /// # Errors
    ///
    /// As [`Self::call_tool`] and [`Self::ctx_for_request`].
    pub async fn call_tool_in_request(
        &self,
        name: &str,
        args: serde_json::Value,
        author: McpAuthorContext,
        auth: Option<McpAuthContext>,
        request: FlavorServices,
    ) -> Result<serde_json::Value, ToolInvocationError> {
        let reply = self
            .call_tool_reply_in_request(name, args, author, auth, request)
            .await?;
        Ok(json_reply(name, reply)?)
    }

    /// [`Self::call_tool_in_request`] with the tool's whole [`ToolReply`]: a
    /// host tool may answer content blocks or a content-bearing failure,
    /// which a JSON-only surface cannot carry.
    ///
    /// Resolves `name` once: a registry tool without listing the host
    /// source, any other name with one [`Self::host_tools_for`].
    ///
    /// # Errors
    ///
    /// As [`Self::call_tool_in_request`], and the host source's list error
    /// for a name the registry does not serve. A [`ToolReply::Structured`]
    /// that is not an object, or a [`ToolReply::Content`] from a tool that
    /// declared an output schema, is an internal error.
    pub async fn call_tool_reply_in_request(
        &self,
        name: &str,
        args: serde_json::Value,
        author: McpAuthorContext,
        auth: Option<McpAuthContext>,
        request: FlavorServices,
    ) -> Result<ToolReply, ToolInvocationError> {
        let auth = auth.ok_or_else(|| ToolInvocationError::NotAuthorized(name.to_string()))?;
        let target = self.resolve_tool(&auth, name).await?;
        self.call_resolved(target, args, author, auth, request)
            .await
    }

    /// The tool `name` calls for `auth`: the registry's, else the host
    /// source's, listing it once.
    pub(crate) async fn resolve_tool(
        &self,
        auth: &McpAuthContext,
        name: &str,
    ) -> Result<ToolTarget<'_>, ToolInvocationError> {
        if let Some(descriptor) = self
            .registry
            .list_mcp_tools()
            .iter()
            .find(|d| tool_name_matches(d.name, name))
        {
            return Ok(ToolTarget::Registry(descriptor));
        }
        if let Some(tool) = self
            .host_tools_for(auth)
            .await?
            .into_iter()
            .find(|tool| tool_name_matches(&tool.name, name))
        {
            return Ok(ToolTarget::Host(Box::new(tool)));
        }
        Err(ToolInvocationError::ToolNotFound(name.to_string()))
    }

    /// Run a [`Self::resolve_tool`] result through the request behaviors.
    pub(crate) async fn call_resolved(
        &self,
        target: ToolTarget<'_>,
        args: serde_json::Value,
        author: McpAuthorContext,
        auth: McpAuthContext,
        mut request: FlavorServices,
    ) -> Result<ToolReply, ToolInvocationError> {
        let declares_output_schema = target.declares_output_schema();
        let (name, terminal): (String, TerminalDispatch<'_>) = match target {
            ToolTarget::Registry(descriptor) => {
                let call_fn = descriptor.call;
                (
                    descriptor.name.to_string(),
                    Box::new(move |call| {
                        let ToolCall { args, ctx, .. } = call;
                        Box::pin(async move { call_fn(ctx, args).await.map(ToolReply::Structured) })
                    }),
                )
            }
            ToolTarget::Host(tool) => {
                let Some(source) = &self.host_tools else {
                    // A host tool only exists in a source's list.
                    return Err(ToolInvocationError::ToolNotFound(tool.name));
                };
                request
                    .try_insert(McpHostToolCall::new(tool.name.clone(), tool.effect))
                    .map_err(|err| McpToolError::Other(format!("request services: {err}")))?;
                let source = Arc::clone(source);
                (
                    tool.name,
                    Box::new(move |call| Box::pin(async move { source.call(call).await })),
                )
            }
        };
        let ctx = self.ctx_for_request(author, &auth, request)?;
        // Validate once for every transport before a behavior can log
        // arguments or a tool can pass them to storage.
        reject_nul_in_args(&args)?;
        let reply = self
            .dispatch_through_behaviors(name.clone(), args, ctx, terminal)
            .await?;
        Ok(checked_reply(&name, declares_output_schema, reply)?)
    }

    /// # Errors
    ///
    /// Returns `NotAuthorized` or the resource body error.
    pub async fn read_resource(
        &self,
        uri: &str,
        author: McpAuthorContext,
        auth: Option<McpAuthContext>,
    ) -> Result<serde_json::Value, ToolInvocationError> {
        self.read_resource_in_request(uri, author, auth, FlavorServices::default())
            .await
    }

    /// [`Self::read_resource`] with a per-request service set merged onto
    /// the boot set, so request behaviors see the same values on both paths.
    ///
    /// # Errors
    ///
    /// As [`Self::read_resource`] and [`Self::ctx_for_request`]; a resource
    /// is JSON, so a request behavior that answers anything but
    /// [`ToolReply::Structured`] makes it an internal error.
    pub async fn read_resource_in_request(
        &self,
        uri: &str,
        author: McpAuthorContext,
        auth: Option<McpAuthContext>,
        request: FlavorServices,
    ) -> Result<serde_json::Value, ToolInvocationError> {
        let parsed = parse_resource_uri(uri).map_err(|err| err.into_invocation_error(uri))?;
        let auth =
            auth.ok_or_else(|| ToolInvocationError::NotAuthorized(parsed.scope_key().to_string()))?;
        let ctx = self.ctx_for_request(author, &auth, request)?;
        let scope_key = parsed.scope_key();

        let terminal: TerminalDispatch<'_> = Box::new(move |call| {
            Box::pin(async move {
                dispatch_resource(parsed, call.ctx)
                    .await
                    .map(ToolReply::Structured)
            })
        });
        let reply = self
            .dispatch_through_behaviors(
                scope_key.to_string(),
                serde_json::json!({ "uri": uri }),
                ctx,
                terminal,
            )
            .await?;
        Ok(json_reply(scope_key, reply)?)
    }

    /// Run `terminal` inside the registry's `RequestBehavior` onion (scope
    /// gate first), as `call_tool` and `read_resource` do. `name` is the
    /// scope key the gate judges; `ctx` comes from [`Self::ctx_for_request`].
    ///
    /// # Errors
    ///
    /// A behavior's refusal or the terminal's error.
    pub async fn dispatch_through_behaviors<'a>(
        &'a self,
        name: String,
        args: serde_json::Value,
        ctx: McpToolCtx,
        terminal: TerminalDispatch<'a>,
    ) -> Result<ToolReply, ToolInvocationError> {
        Next::new(self.registry.request_behaviors(), terminal)
            .run(ToolCall { name, args, ctx })
            .await
            .map_err(Into::into)
    }
}

/// What a `tools/call` name resolved to.
pub(crate) enum ToolTarget<'a> {
    Registry(&'a McpToolDescriptor),
    Host(Box<McpHostTool>),
}

impl ToolTarget<'_> {
    /// The canonical name: what the gate judges and a call record carries.
    pub(crate) fn name(&self) -> &str {
        match self {
            Self::Registry(descriptor) => descriptor.name,
            Self::Host(tool) => &tool.name,
        }
    }

    /// Every registry tool declares an output schema; a host tool may not.
    const fn declares_output_schema(&self) -> bool {
        match self {
            Self::Registry(_) => true,
            Self::Host(tool) => tool.output_schema.is_some(),
        }
    }
}

/// The final reply of a tool call, checked where it leaves the behaviors: a
/// structured answer is a JSON object, and only a tool that declared no
/// output schema answers content, which a schema-validating client could not
/// check. A failure carries content by definition and is not held to either.
fn checked_reply(
    tool: &str,
    declares_output_schema: bool,
    reply: ToolReply,
) -> Result<ToolReply, McpToolError> {
    match &reply {
        ToolReply::Structured(output) if !output.is_object() => {
            tracing::error!(tool = %tool, "tool output after request behaviors must be a JSON object");
            Err(McpToolError::Other(
                "tool output must be a JSON object".into(),
            ))
        }
        ToolReply::Content(_) if declares_output_schema => {
            tracing::error!(tool = %tool, "a tool that declares an output schema must answer structured content");
            Err(McpToolError::Other(
                "tool output must be structured content".into(),
            ))
        }
        ToolReply::Structured(_) | ToolReply::Content(_) | ToolReply::Failure(_) => Ok(reply),
    }
}

/// The JSON of a reply for a surface that carries nothing else (a resource
/// read, the REST surface, `CoreMcpTools`).
fn json_reply(name: &str, reply: ToolReply) -> Result<serde_json::Value, McpToolError> {
    reply.into_structured().map_err(|_| {
        tracing::error!(tool = %name, "this surface carries JSON; the tool answered content");
        McpToolError::Other("tool reply must be a JSON object".into())
    })
}

fn host_tool_name_refusal(
    registry: &FlavorRegistryFrozen,
    name: &str,
    served: &[McpHostTool],
) -> Option<&'static str> {
    if name.is_empty()
        || name.chars().count() > MAX_HOST_TOOL_NAME_CHARS
        || proxima_core::provider_safe_tool_name(name) != name
    {
        return Some("a host tool name is 1..=128 characters of [A-Za-z0-9_.-]");
    }
    if registry.list_mcp_tools().iter().any(|descriptor| {
        descriptor.name == name || proxima_core::provider_safe_tool_name(descriptor.name) == name
    }) {
        return Some("a registry tool serves this name");
    }
    if served.iter().any(|tool| tool.name == name) {
        return Some("the host listed this name twice");
    }
    None
}

/// Reject NUL in the entire argument tree before behaviors or tool code.
/// `PostgreSQL` text cannot store U+0000, though JSON can encode it. Treat it
/// as the existing caller-input error instead of a later database fault.
/// Rejection preserves the request; silently stripping the character would
/// execute a different query. The same rule applies to object keys.
///
/// # Errors
///
/// [`McpToolError::InvalidInput`] naming whether a value or a key held NUL.
pub fn reject_nul_in_args(args: &serde_json::Value) -> Result<(), McpToolError> {
    // An explicit worklist keeps the validator stack-safe independently of
    // serde_json's parser depth limit, including direct host callers.
    let mut stack = vec![args];
    while let Some(value) = stack.pop() {
        match value {
            serde_json::Value::String(text) => {
                if text.contains('\0') {
                    return Err(McpToolError::InvalidInput(
                        "arguments must not contain NUL (U+0000)".to_string(),
                    ));
                }
            }
            serde_json::Value::Array(items) => stack.extend(items.iter()),
            serde_json::Value::Object(map) => {
                for (key, item) in map {
                    if key.contains('\0') {
                        return Err(McpToolError::InvalidInput(
                            "argument names must not contain NUL (U+0000)".to_string(),
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

async fn dispatch_resource(
    parsed: ParsedResource,
    ctx: McpToolCtx,
) -> Result<serde_json::Value, McpToolError> {
    match parsed {
        ParsedResource::Schemas(args) => resource_output_value(list_schemas(ctx, args).await?),
        ParsedResource::Schema {
            schema_id,
            schema_version,
        } => resource_output_value(get_schema(ctx, &schema_id, schema_version).await?),
        ParsedResource::Tools(args) => {
            resource_output_value(list_substrate_tools(ctx, args).await?)
        }
        ParsedResource::Graph(args) => resource_output_value(get_graph(ctx, args).await?),
        ParsedResource::Memory(args) => resource_output_value(get_memory(ctx, args).await?),
        ParsedResource::Memories(args) => resource_output_value(get_memories(ctx, args).await?),
        ParsedResource::MemoryLineage(args) => {
            resource_output_value(walk_memory_lineage(ctx, args).await?)
        }
        ParsedResource::ChangeEvents(args) => {
            resource_output_value(list_change_events(ctx, args).await?)
        }
        ParsedResource::WakeCandidates(args) => {
            resource_output_value(list_wake_candidates(ctx, args).await?)
        }
        ParsedResource::Goals(args) => resource_output_value(list_goals(ctx, args).await?),
        ParsedResource::Goal(reference) => resource_output_value(get_goal(ctx, &reference).await?),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ToolInvocationError {
    #[error("tool not authorized: {0}")]
    NotAuthorized(String),
    #[error("tool not found: {0}")]
    ToolNotFound(String),
    #[error("tool error: {0}")]
    Tool(McpToolError),
}

impl From<McpToolError> for ToolInvocationError {
    fn from(err: McpToolError) -> Self {
        match err {
            McpToolError::NotAuthorized(tool) => Self::NotAuthorized(tool),
            err => Self::Tool(err),
        }
    }
}

impl ToolInvocationError {
    #[must_use]
    pub fn kind(&self) -> McpToolErrorKind {
        match self {
            Self::NotAuthorized(_) => McpToolErrorKind::InvalidRequest,
            Self::ToolNotFound(_) => McpToolErrorKind::InvalidInput,
            Self::Tool(inner) => inner.kind(),
        }
    }
}

/// Why a `proxima://` URI failed to parse into a resource read. Each
/// failure keeps its shape instead of collapsing into a generic "unknown
/// resource": an unmatched path is a JSON-RPC `resource_not_found`, while
/// a bad or missing query parameter on a known template is an
/// `invalid_params` naming the parameter.
#[derive(Debug, PartialEq, Eq)]
enum ResourceUriError {
    /// The URI matches no resource template.
    UnknownPath,
    /// A query pair without a `=` separator.
    MalformedQueryPair { pair: String },
    /// A required query parameter is absent or empty.
    MissingParam { param: &'static str },
    /// A query parameter failed to parse.
    InvalidParam {
        param: &'static str,
        value: String,
        expected: &'static str,
    },
}

impl ResourceUriError {
    fn into_invocation_error(self, uri: &str) -> ToolInvocationError {
        let invalid =
            |message: String| ToolInvocationError::Tool(McpToolError::InvalidInput(message));
        match self {
            Self::UnknownPath => ToolInvocationError::ToolNotFound(uri.to_string()),
            Self::MalformedQueryPair { pair } => invalid(format!(
                "resource {uri}: malformed query parameter '{pair}': expected key=value"
            )),
            Self::MissingParam { param } => invalid(format!(
                "resource {uri}: missing required parameter `{param}`"
            )),
            Self::InvalidParam {
                param,
                value,
                expected,
            } => invalid(format!(
                "resource {uri}: invalid parameter `{param}`: expected {expected}, got '{value}'"
            )),
        }
    }
}

#[derive(Debug)]
enum ParsedResource {
    Schemas(ListSchemasArgs),
    Schema {
        schema_id: String,
        schema_version: u32,
    },
    Tools(ListSubstrateToolsArgs),
    Graph(GetGraphArgs),
    Memory(GetMemoryArgs),
    Memories(GetMemoriesArgs),
    MemoryLineage(WalkMemoryLineageArgs),
    ChangeEvents(ListChangeEventsArgs),
    WakeCandidates(ListWakeCandidatesArgs),
    Goals(ListGoalsArgs),
    Goal(String),
}

impl ParsedResource {
    const fn scope_key(&self) -> &'static str {
        match self {
            Self::Schemas(_) => protocol_resource::SCHEMAS,
            Self::Schema { .. } => protocol_resource::SCHEMA,
            Self::Tools(_) => protocol_resource::TOOLS,
            Self::Graph(_) => protocol_resource::GRAPH,
            Self::Memory(_) => protocol_resource::MEMORY,
            Self::Memories(_) => protocol_resource::MEMORIES,
            Self::MemoryLineage(_) => protocol_resource::MEMORY_LINEAGE,
            Self::ChangeEvents(_) => protocol_resource::CHANGE_EVENTS,
            Self::WakeCandidates(_) => protocol_resource::WAKE_CANDIDATES,
            Self::Goals(_) => protocol_resource::GOALS,
            Self::Goal(_) => protocol_resource::GOAL,
        }
    }
}

/// The dispatch paths, read out of flavor #0's declaration at compile time.
///
/// A `match` arm needs a constant, so the paths are named here — but each
/// name is a projection of the same `ResourceContract` that supplies the
/// advertised URI template and the palette key, not a second table that has
/// to be kept in step with them.
mod resource_path {
    use super::{core_resource, protocol_resource};

    pub const SCHEMAS: &str = core_resource(protocol_resource::SCHEMAS).path;
    pub const SCHEMA: &str = core_resource(protocol_resource::SCHEMA).path;
    pub const TOOLS: &str = core_resource(protocol_resource::TOOLS).path;
    pub const GRAPH: &str = core_resource(protocol_resource::GRAPH).path;
    pub const CHANGE_EVENTS: &str = core_resource(protocol_resource::CHANGE_EVENTS).path;
    pub const WAKE_CANDIDATES: &str = core_resource(protocol_resource::WAKE_CANDIDATES).path;
    pub const MEMORIES: &str = core_resource(protocol_resource::MEMORIES).path;
    pub const MEMORY: &str = core_resource(protocol_resource::MEMORY).path;
    pub const GOALS: &str = core_resource(protocol_resource::GOALS).path;
    pub const GOAL: &str = core_resource(protocol_resource::GOAL).path;

    /// Everything after `<declared path>/`, or `None` when `path` is not
    /// that resource. The id-bearing resources match by prefix rather than
    /// by equality, and a declared `path` is a path — the separator belongs
    /// to the parser, not to the declaration.
    pub fn tail<'a>(path: &'a str, resource: &str) -> Option<&'a str> {
        path.strip_prefix(resource)
            .and_then(|rest| rest.strip_prefix('/'))
    }
}

fn parse_resource_uri(uri: &str) -> Result<ParsedResource, ResourceUriError> {
    let rest = uri
        .strip_prefix("proxima://")
        .ok_or(ResourceUriError::UnknownPath)?;
    let (path, query) = rest
        .split_once('?')
        .map_or((rest, None), |(path, query)| (path, Some(query)));
    let query = parse_query(query)?;

    match path {
        resource_path::SCHEMAS => Ok(ParsedResource::Schemas(ListSchemasArgs {
            kind: query_value(&query, "kind").map(ToOwned::to_owned),
        })),
        resource_path::TOOLS => Ok(ParsedResource::Tools(ListSubstrateToolsArgs {})),
        resource_path::GRAPH => Ok(ParsedResource::Graph(GetGraphArgs {})),
        resource_path::CHANGE_EVENTS => Ok(ParsedResource::ChangeEvents(ListChangeEventsArgs {
            since: query_value(&query, "since").map(ToOwned::to_owned),
            limit: query_parse(&query, "limit", "a non-negative integer")?,
        })),
        resource_path::WAKE_CANDIDATES => {
            Ok(ParsedResource::WakeCandidates(ListWakeCandidatesArgs {
                fact: query_value(&query, "fact")
                    .filter(|fact| !fact.is_empty())
                    .ok_or(ResourceUriError::MissingParam { param: "fact" })?
                    .to_owned(),
                limit: query_parse(&query, "limit", "a non-negative integer")?,
            }))
        }
        resource_path::MEMORIES => {
            let ids = query_value(&query, "ids")
                .filter(|ids| !ids.is_empty())
                .ok_or(ResourceUriError::MissingParam { param: "ids" })?;
            Ok(ParsedResource::Memories(GetMemoriesArgs {
                memories: ids.split(',').map(ToOwned::to_owned).collect(),
            }))
        }
        resource_path::GOALS => Ok(ParsedResource::Goals(ListGoalsArgs {
            state: query_value(&query, "state").map(ToOwned::to_owned),
            limit: query_parse(&query, "limit", "a non-negative integer")?,
            cursor: query_value(&query, "cursor").map(ToOwned::to_owned),
        })),
        // A schema id carries its own `/` separators (`core/note`), so the
        // version is split off the END of the tail, not the start.
        path if resource_path::tail(path, resource_path::SCHEMA).is_some() => {
            let tail = resource_path::tail(path, resource_path::SCHEMA).unwrap_or_default();
            let (schema_id, version) =
                tail.rsplit_once('/')
                    .ok_or(ResourceUriError::MissingParam {
                        param: "schema_version",
                    })?;
            if schema_id.is_empty() {
                return Err(ResourceUriError::UnknownPath);
            }
            let schema_version =
                version
                    .parse::<u32>()
                    .map_err(|_| ResourceUriError::InvalidParam {
                        param: "schema_version",
                        value: version.to_string(),
                        expected: "a non-negative integer",
                    })?;
            Ok(ParsedResource::Schema {
                schema_id: schema_id.to_string(),
                schema_version,
            })
        }
        path if resource_path::tail(path, resource_path::MEMORY).is_some() => {
            parse_memory_resource_path(path, &query)
        }
        path if resource_path::tail(path, resource_path::GOAL).is_some() => {
            let id = resource_path::tail(path, resource_path::GOAL).unwrap_or_default();
            if id.is_empty() || id.contains('/') {
                return Err(ResourceUriError::UnknownPath);
            }
            Ok(ParsedResource::Goal(id.to_string()))
        }
        _ => Err(ResourceUriError::UnknownPath),
    }
}

fn parse_memory_resource_path(
    path: &str,
    query: &[(&str, &str)],
) -> Result<ParsedResource, ResourceUriError> {
    let rest =
        resource_path::tail(path, resource_path::MEMORY).ok_or(ResourceUriError::UnknownPath)?;
    if let Some(id) = rest.strip_suffix("/lineage") {
        if id.is_empty() || id.contains('/') {
            return Err(ResourceUriError::UnknownPath);
        }
        return Ok(ParsedResource::MemoryLineage(WalkMemoryLineageArgs {
            memory: id.to_string(),
            direction: query_lineage_direction(query)?,
            depth: query_parse(query, "depth", "a non-negative integer (clamped to 1..=8)")?
                .unwrap_or(3),
            limit: query_parse(query, "limit", "a non-negative integer")?.unwrap_or(50),
            cursor: query_value(query, "cursor").map(ToOwned::to_owned),
        }));
    }
    if rest.is_empty() || rest.contains('/') {
        return Err(ResourceUriError::UnknownPath);
    }
    Ok(ParsedResource::Memory(GetMemoryArgs {
        memory: rest.to_string(),
        expand_neighbors: query_bool(query, "expand_neighbors"),
        space: None,
    }))
}

fn parse_query(query: Option<&str>) -> Result<Vec<(&str, &str)>, ResourceUriError> {
    let Some(query) = query else {
        return Ok(Vec::new());
    };
    if query.is_empty() {
        return Ok(Vec::new());
    }
    query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            pair.split_once('=')
                .ok_or_else(|| ResourceUriError::MalformedQueryPair {
                    pair: pair.to_string(),
                })
        })
        .collect()
}

fn query_value<'a>(query: &'a [(&str, &str)], key: &str) -> Option<&'a str> {
    query
        .iter()
        .find_map(|(candidate, value)| (*candidate == key).then_some(*value))
}

fn query_bool(query: &[(&str, &str)], key: &str) -> bool {
    query_value(query, key) == Some("true")
}

fn query_parse<T>(
    query: &[(&str, &str)],
    key: &'static str,
    expected: &'static str,
) -> Result<Option<T>, ResourceUriError>
where
    T: std::str::FromStr,
{
    query_value(query, key).map_or(Ok(None), |value| {
        value
            .parse::<T>()
            .map(Some)
            .map_err(|_| ResourceUriError::InvalidParam {
                param: key,
                value: value.to_string(),
                expected,
            })
    })
}

fn query_lineage_direction(
    query: &[(&str, &str)],
) -> Result<WalkMemoryLineageDirectionArg, ResourceUriError> {
    match query_value(query, "direction") {
        None | Some("ancestors") => Ok(WalkMemoryLineageDirectionArg::Ancestors),
        Some("descendants") => Ok(WalkMemoryLineageDirectionArg::Descendants),
        Some(other) => Err(ResourceUriError::InvalidParam {
            param: "direction",
            value: other.to_string(),
            expected: "'ancestors' or 'descendants'",
        }),
    }
}

fn resource_output_value<T>(output: T) -> Result<serde_json::Value, McpToolError>
where
    T: Serialize,
{
    serde_json::to_value(output)
        .map_err(|err| McpToolError::Other(format!("serialize resource output: {err}")))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::auth::McpAuthContext;
    use proxima_core::mcp::{McpAuthorContext, ToolContent};
    use proxima_core::{AuthzContext, FlavorRegistry, Owner, OwnerRef, ToolScope, UserId};

    fn fake_owner() -> Owner {
        OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()))
    }

    #[derive(Debug)]
    struct RecordingBehavior(Arc<std::sync::Mutex<Vec<String>>>);

    #[async_trait::async_trait]
    impl proxima_core::RequestBehavior for RecordingBehavior {
        async fn handle(&self, call: ToolCall, next: Next<'_>) -> Result<ToolReply, McpToolError> {
            self.0.lock().expect("lock").push(call.name.clone());
            next.run(call).await
        }
    }

    #[derive(Debug)]
    struct EchoHostTools;

    #[derive(Debug)]
    struct OutputHostTools(serde_json::Value);

    #[async_trait::async_trait]
    impl McpHostTools for OutputHostTools {
        async fn list(&self, _: &McpAuthContext) -> Result<Vec<McpHostTool>, McpToolError> {
            Ok(vec![host_tool("host_output", READ)])
        }

        async fn call(&self, _: ToolCall) -> Result<ToolReply, McpToolError> {
            Ok(ToolReply::Structured(self.0.clone()))
        }
    }

    #[tokio::test]
    async fn direct_host_tool_calls_require_object_outputs() {
        let owner = fake_owner();
        let auth = McpAuthContext {
            owner,
            authz: AuthzContext::single_owner(&owner, AuthPath::HostBearer),
        };
        for output in [
            serde_json::json!(null),
            serde_json::json!("private-output"),
            serde_json::json!(42),
            serde_json::json!(true),
            serde_json::json!(["private-output"]),
            serde_json::json!({"answer": [42], "nested": {"value": null}}),
        ] {
            let server = make_server().with_host_tools(Arc::new(OutputHostTools(output.clone())));
            let result = server
                .call_tool(
                    "host_output",
                    serde_json::json!({}),
                    author(),
                    Some(auth.clone()),
                )
                .await;
            if output.is_object() {
                assert_eq!(result.unwrap(), output);
            } else {
                let Err(ToolInvocationError::Tool(error)) = result else {
                    panic!("non-object output must return a tool error: {result:?}");
                };
                assert!(matches!(&error, McpToolError::Other(_)));
                assert_eq!(error.kind(), McpToolErrorKind::Internal);
                assert_eq!(error.client_message(), "internal server error");
                assert!(!error.to_string().contains("private-output"));
            }
        }
    }

    const READ: proxima_core::ToolEffect = proxima_core::ToolEffect::ReadOnly;
    const WRITE: proxima_core::ToolEffect =
        proxima_core::ToolEffect::Additive(proxima_core::Replay::NonIdempotent);

    fn host_tool(name: &str, effect: proxima_core::ToolEffect) -> McpHostTool {
        McpHostTool::new(
            name,
            format!("{name} fixture"),
            serde_json::json!({"type": "object"}),
            effect,
        )
        .with_output_schema(serde_json::json!({"type": "object"}))
    }

    #[async_trait::async_trait]
    impl McpHostTools for EchoHostTools {
        async fn list(&self, _auth: &McpAuthContext) -> Result<Vec<McpHostTool>, McpToolError> {
            Ok(vec![
                host_tool("host_echo", READ),
                host_tool("host_write", WRITE),
                // Shadowed by the registry's own tool of this name.
                host_tool("core_memory_spaces", READ),
                // A registry tool's name in another spelling, a scope-key
                // shape, an action-leaf shape, a repeat, and an overlong name.
                host_tool("core:memory_spaces", READ),
                host_tool("resource:graph", WRITE),
                host_tool("core_goal:set", WRITE),
                host_tool("host_echo", WRITE),
                host_tool(&"h".repeat(MAX_HOST_TOOL_NAME_CHARS + 1), READ),
            ])
        }

        async fn call(&self, call: ToolCall) -> Result<ToolReply, McpToolError> {
            let marker = call
                .ctx
                .services
                .get::<McpHostToolCall>()
                .ok_or_else(|| McpToolError::Other("no host-call marker".into()))?;
            Ok(ToolReply::Structured(serde_json::json!({
                "tool": call.name,
                "marker": marker.name(),
                "args": call.args,
            })))
        }
    }

    fn author() -> McpAuthorContext {
        McpAuthorContext {
            model_id: "test".into(),
            trusted_model_id: None,
            client_name: "test".into(),
            client_version: "0".into(),
            caller_self_perspective: None,
        }
    }

    /// A host tool runs through the registry's behaviors, scope gate first,
    /// and the gate classifies it from the host's declaration.
    #[tokio::test]
    async fn a_host_tool_dispatches_through_the_request_behaviors() {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut registry = FlavorRegistry::new();
        registry.add_request_behavior(RecordingBehavior(Arc::clone(&seen)));
        let server = McpToolHost::from_parts(
            Arc::new(registry.freeze_or_panic_for_tests()),
            FlavorServices::default(),
        )
        .with_host_tools(Arc::new(EchoHostTools));
        let owner = fake_owner();
        let auth = McpAuthContext {
            owner,
            authz: AuthzContext::single_owner(&owner, AuthPath::HostBearer),
        };
        let listed: Vec<String> = server
            .host_tools_for(&auth)
            .await
            .expect("the source lists")
            .into_iter()
            .map(|tool| tool.name)
            .collect();
        assert_eq!(
            listed,
            ["host_echo", "host_write"],
            "only well-formed, unshadowed, first-listed names are served"
        );

        let output = server
            .call_tool(
                "host_echo",
                serde_json::json!({"x": 1}),
                author(),
                Some(auth.clone()),
            )
            .await
            .expect("owner calls a host tool");
        assert_eq!(
            output,
            serde_json::json!({"tool": "host_echo", "marker": "host_echo", "args": {"x": 1}})
        );
        assert_eq!(*seen.lock().expect("lock"), ["host_echo"]);

        // The palette gates a host tool by its name.
        let narrowed = McpAuthContext {
            owner,
            authz: auth
                .authz
                .clone()
                .with_tool_scope(ToolScope::Palette(vec!["host_write".into()])),
        };
        assert!(matches!(
            server
                .call_tool("host_echo", serde_json::json!({}), author(), Some(narrowed))
                .await,
            Err(ToolInvocationError::NotAuthorized(name)) if name == "host_echo"
        ));

        // A viewer reads through a read-only host tool and cannot write.
        let group = OwnerRef::Group(proxima_core::GroupId::new(uuid::Uuid::now_v7()));
        let viewer = McpAuthContext {
            owner: group,
            authz: AuthzContext::for_subject_with_role(
                UserId::new(uuid::Uuid::now_v7()),
                [(group, proxima_core::Role::viewer())],
                AuthPath::HostBearer,
            )
            .narrowed_to_owner(group)
            .expect("viewer narrows"),
        };
        server
            .call_tool(
                "host_echo",
                serde_json::json!({}),
                author(),
                Some(viewer.clone()),
            )
            .await
            .expect("a read-only host tool admits a viewer");
        assert!(matches!(
            server
                .call_tool("host_write", serde_json::json!({}), author(), Some(viewer))
                .await,
            Err(ToolInvocationError::NotAuthorized(name)) if name == "host_write"
        ));
        assert!(matches!(
            server
                .call_tool(
                    "host_echo",
                    serde_json::json!({"a": "\0"}),
                    author(),
                    Some(auth)
                )
                .await,
            Err(ToolInvocationError::Tool(McpToolError::InvalidInput(_)))
        ));
    }

    fn make_server() -> McpToolHost {
        McpToolHost::from_parts(
            Arc::new(FlavorRegistry::new().freeze_or_panic_for_tests()),
            FlavorServices::default(),
        )
    }

    #[tokio::test]
    async fn nul_validation_preserves_host_auth_and_unknown_tool_priority() {
        let server = make_server();
        let owner = fake_owner();
        let auth = McpAuthContext {
            owner,
            authz: AuthzContext::single_owner(&owner, AuthPath::HostBearer),
        };
        let author = McpAuthorContext {
            model_id: "test".into(),
            trusted_model_id: None,
            client_name: "test".into(),
            client_version: "0".into(),
            caller_self_perspective: None,
        };
        let args = serde_json::json!({"value": "bad\0input"});
        assert!(matches!(
            server
                .call_tool("core_memory_spaces", args.clone(), author.clone(), None)
                .await,
            Err(ToolInvocationError::NotAuthorized(_))
        ));
        assert!(matches!(
            server
                .call_tool(
                    "unknown_tool",
                    args.clone(),
                    author.clone(),
                    Some(auth.clone())
                )
                .await,
            Err(ToolInvocationError::ToolNotFound(_))
        ));
        assert!(matches!(
            server.call_tool("core_memory_spaces", args, author, Some(auth)).await,
            Err(ToolInvocationError::Tool(McpToolError::InvalidInput(message)))
                if message == "arguments must not contain NUL (U+0000)"
        ));
    }

    /// The author context is a per-call struct an out-of-tree host builds
    /// by hand, and `ToolCaller::model_id` — the label a flavor tool
    /// records — is copied straight out of it. Restoring only
    /// `trusted_model_id` would leave the documented invariant ("when
    /// present, `model_id` equals it") false for exactly the callers that
    /// did not go through a transport edge.
    #[test]
    fn ctx_for_reconciles_both_model_fields_against_the_credential() {
        let server = make_server();
        let owner = fake_owner();
        let auth = McpAuthContext {
            owner,
            authz: AuthzContext::single_owner(&owner, AuthPath::HostBearer)
                .with_trusted_model_id("acme/runner-v3")
                .expect("a well-formed runner id binds"),
        };
        let author = |model_id: &str| McpAuthorContext {
            model_id: model_id.into(),
            trusted_model_id: None,
            client_name: "test".into(),
            client_version: "0".into(),
            caller_self_perspective: None,
        };

        let ctx = server
            .ctx_for(author("acme/runner-v3"), &auth)
            .expect("an agreeing author is accepted");
        assert_eq!(ctx.author.model_id, "acme/runner-v3");
        assert_eq!(
            ctx.author.trusted_model_id.as_deref(),
            Some("acme/runner-v3"),
            "the binding is restored even though the author omitted it"
        );

        let err = server
            .ctx_for(author("openai/gpt-9"), &auth)
            .expect_err("a stale label may not reach a flavor as the one to record");
        assert_eq!(err.kind(), McpToolErrorKind::InvalidInput);
    }

    /// Without a binding the author's own label stands, and an author that
    /// invented a trusted id does not keep it.
    #[test]
    fn ctx_for_strips_a_binding_the_credential_does_not_carry() {
        let server = make_server();
        let owner = fake_owner();
        let auth = McpAuthContext {
            owner,
            authz: AuthzContext::single_owner(&owner, AuthPath::HostBearer),
        };

        let ctx = server
            .ctx_for(
                McpAuthorContext {
                    model_id: "caller/model".into(),
                    trusted_model_id: Some("acme/runner-v3".into()),
                    client_name: "test".into(),
                    client_version: "0".into(),
                    caller_self_perspective: None,
                },
                &auth,
            )
            .expect("no binding, no conflict");

        assert_eq!(ctx.author.model_id, "caller/model");
        assert_eq!(ctx.author.trusted_model_id, None);
    }

    #[test]
    fn parse_resource_uri_projects_known_resources() {
        let memory = parse_resource_uri(
            "proxima://memory/F:018f0000-0000-7000-8000-000000000001?expand_neighbors=true",
        )
        .expect("memory resource");
        assert!(matches!(
            memory,
            ParsedResource::Memory(GetMemoryArgs {
                expand_neighbors: true,
                ..
            })
        ));

        let lineage =
            parse_resource_uri("proxima://memory/A:018f0000-0000-7000-8000-000000000001/lineage?direction=descendants&depth=2&limit=7")
                .expect("lineage resource");
        assert!(matches!(
            lineage,
            ParsedResource::MemoryLineage(WalkMemoryLineageArgs {
                direction: WalkMemoryLineageDirectionArg::Descendants,
                depth: 2,
                limit: 7,
                ..
            })
        ));

        assert_eq!(
            parse_resource_uri("proxima://memory//lineage").unwrap_err(),
            ResourceUriError::UnknownPath
        );
        assert_eq!(
            parse_resource_uri("proxima://memory/F:one/two/lineage").unwrap_err(),
            ResourceUriError::UnknownPath
        );
        assert_eq!(
            parse_resource_uri("proxima://change-events?limit=not-a-number").unwrap_err(),
            ResourceUriError::InvalidParam {
                param: "limit",
                value: "not-a-number".into(),
                expected: "a non-negative integer",
            }
        );

        let wake = parse_resource_uri(
            "proxima://wake-candidates?fact=F:018f0000-0000-7000-8000-000000000001&limit=5",
        )
        .expect("wake-candidates resource");
        assert!(matches!(
            wake,
            ParsedResource::WakeCandidates(ListWakeCandidatesArgs { limit: Some(5), .. })
        ));
        assert_eq!(
            parse_resource_uri("proxima://wake-candidates").unwrap_err(),
            ResourceUriError::MissingParam { param: "fact" }
        );
        assert_eq!(
            parse_resource_uri("proxima://wake-candidates?fact=").unwrap_err(),
            ResourceUriError::MissingParam { param: "fact" }
        );
        assert_eq!(
            parse_resource_uri("proxima://wake-candidates?fact=F:018f0000-0000-7000-8000-000000000001&limit=not-a-number")
                .unwrap_err(),
            ResourceUriError::InvalidParam {
                param: "limit",
                value: "not-a-number".into(),
                expected: "a non-negative integer",
            }
        );
    }

    /// Each parse-failure class carries its own wire shape: unknown paths
    /// surface as resource-not-found, while bad or missing parameters on a
    /// known template name the parameter (backed by `invalid_params` at
    /// the rmcp layer) — no failure may collapse into a generic
    /// "unknown resource".
    #[test]
    fn parse_resource_uri_distinguishes_error_classes() {
        assert_eq!(
            parse_resource_uri("proxima://no-such-resource").unwrap_err(),
            ResourceUriError::UnknownPath
        );
        assert_eq!(
            parse_resource_uri("nothing://schemas").unwrap_err(),
            ResourceUriError::UnknownPath
        );
        // depth=300 does not collapse into "unknown resource": it parses
        // as a wide integer and the tool clamps it to the documented 1..=8.
        let lineage = parse_resource_uri(
            "proxima://memory/F:018f0000-0000-7000-8000-000000000001/lineage?depth=300",
        )
        .expect("oversized depth parses; the tool clamps");
        assert!(matches!(
            lineage,
            ParsedResource::MemoryLineage(WalkMemoryLineageArgs { depth: 300, .. })
        ));
        assert_eq!(
            parse_resource_uri(
                "proxima://memory/F:018f0000-0000-7000-8000-000000000001/lineage?direction=sideways"
            )
            .unwrap_err(),
            ResourceUriError::InvalidParam {
                param: "direction",
                value: "sideways".into(),
                expected: "'ancestors' or 'descendants'",
            }
        );
        assert_eq!(
            parse_resource_uri("proxima://goals?limit").unwrap_err(),
            ResourceUriError::MalformedQueryPair {
                pair: "limit".into()
            }
        );

        let unknown = ResourceUriError::UnknownPath.into_invocation_error("proxima://nope");
        assert!(
            matches!(unknown, ToolInvocationError::ToolNotFound(uri) if uri == "proxima://nope")
        );
        let missing = ResourceUriError::MissingParam { param: "fact" }
            .into_invocation_error("proxima://wake-candidates");
        match missing {
            ToolInvocationError::Tool(McpToolError::InvalidInput(message)) => {
                assert!(message.contains("missing required parameter `fact`"));
                assert!(message.contains("proxima://wake-candidates"));
            }
            other => panic!("expected InvalidInput, got {other:?}"),
        }
    }

    /// `read_resource` now traverses the same `RequestBehavior`
    /// onion (`ScopeGateBehavior`) as `call_tool`, instead of a hand-rolled
    /// scope check outside the chain. An out-of-palette caller must still
    /// be denied, and denial must still surface as `NotAuthorized` keyed by
    /// the resource's scope key — matching the pre-refactor error shape.
    #[tokio::test]
    async fn read_resource_denies_out_of_palette_scope() {
        let server = make_server();
        let owner = fake_owner();
        let author = McpAuthorContext {
            model_id: "test-model".into(),
            trusted_model_id: None,
            client_name: "test-client".into(),
            client_version: "0.1.0".into(),
            caller_self_perspective: None,
        };
        let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer)
            .with_tool_scope(ToolScope::Palette(Vec::new()));
        let auth = McpAuthContext { owner, authz };

        let err = server
            .read_resource("proxima://schemas", author, Some(auth))
            .await
            .expect_err("empty palette must deny resource reads");

        assert!(
            matches!(
                err,
                ToolInvocationError::NotAuthorized(ref key) if key == protocol_resource::SCHEMAS
            ),
            "unexpected error: {err:?}"
        );
    }

    #[tokio::test]
    async fn ctx_for_speaks_prefixed_ids() {
        let server = make_server();
        let author = McpAuthorContext {
            model_id: "test-model".into(),
            trusted_model_id: None,
            client_name: "test-client".into(),
            client_version: "0.1.0".into(),
            caller_self_perspective: None,
        };
        let owner = fake_owner();
        let auth = McpAuthContext {
            owner,
            authz: AuthzContext::single_owner(&owner, AuthPath::HostBearer)
                .narrowed_to_owner(owner)
                .expect("personal owner narrows"),
        };

        let ctx = server
            .ctx_for(author, &auth)
            .expect("no binding, no conflict");
        let id = proxima_core::MemoryId::new(uuid::Uuid::now_v7());
        let wire = ctx.format_fact_memory(id);
        assert_eq!(wire, format!("F:{}", id.into_inner()));
        assert_eq!(ctx.resolve_fact_memory(&wire).expect("round trip"), id);
        assert!(
            ctx.resolve_fact_memory(&id.into_inner().to_string())
                .is_err(),
            "bare uuids must not be accepted on the wire"
        );
    }

    /// A host source that counts its catalog reads and answers every call
    /// with one scripted reply.
    #[derive(Debug)]
    struct ScriptedHostTools {
        lists: Arc<std::sync::atomic::AtomicUsize>,
        catalog: Result<Vec<McpHostTool>, &'static str>,
        reply: ToolReply,
    }

    #[async_trait::async_trait]
    impl McpHostTools for ScriptedHostTools {
        async fn list(&self, _: &McpAuthContext) -> Result<Vec<McpHostTool>, McpToolError> {
            self.lists.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.catalog
                .clone()
                .map_err(|message| McpToolError::Unavailable(message.into()))
        }

        async fn call(&self, _: ToolCall) -> Result<ToolReply, McpToolError> {
            Ok(self.reply.clone())
        }
    }

    fn scripted(
        catalog: Result<Vec<McpHostTool>, &'static str>,
        reply: ToolReply,
    ) -> (McpToolHost, Arc<std::sync::atomic::AtomicUsize>) {
        let lists = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let server = make_server().with_host_tools(Arc::new(ScriptedHostTools {
            lists: Arc::clone(&lists),
            catalog,
            reply,
        }));
        (server, lists)
    }

    fn owner_auth() -> McpAuthContext {
        let owner = fake_owner();
        McpAuthContext {
            owner,
            authz: AuthzContext::single_owner(&owner, AuthPath::HostBearer),
        }
    }

    fn lists(counter: &std::sync::atomic::AtomicUsize) -> usize {
        counter.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// One request, one catalog: a host call reads the source once to find
    /// its tool, and a registry tool is found without reading it at all.
    #[tokio::test]
    async fn a_host_call_lists_the_catalog_once_and_a_registry_call_never() {
        let (server, counter) = scripted(
            Ok(vec![host_tool("host_echo", READ)]),
            ToolReply::Structured(serde_json::json!({"ok": true})),
        );
        let auth = owner_auth();

        server
            .call_tool(
                "host_echo",
                serde_json::json!({}),
                author(),
                Some(auth.clone()),
            )
            .await
            .expect("the host tool answers");
        assert_eq!(lists(&counter), 1, "one catalog read for the call");

        // The registry's own tool is found first; its outcome is not the
        // subject, the source staying unread is.
        let _ = server
            .call_tool(
                "core_memory_spaces",
                serde_json::json!({}),
                author(),
                Some(auth),
            )
            .await;
        assert_eq!(lists(&counter), 1, "a registry call reads no catalog");
    }

    /// A catalog that cannot be read is an error, never an empty list: the
    /// call that needed it fails with the host's error, and so does asking
    /// for the catalog.
    #[tokio::test]
    async fn a_failing_catalog_fails_the_call_that_needed_it() {
        let (server, counter) = scripted(
            Err("catalog down"),
            ToolReply::Structured(serde_json::json!({})),
        );
        let auth = owner_auth();

        for name in ["host_echo", "no_such_tool"] {
            let result = server
                .call_tool(name, serde_json::json!({}), author(), Some(auth.clone()))
                .await;
            assert!(
                matches!(
                    &result,
                    Err(ToolInvocationError::Tool(McpToolError::Unavailable(message)))
                        if message == "catalog down"
                ),
                "{name}: {result:?}"
            );
        }
        assert!(matches!(
            server.host_tools_for(&auth).await,
            Err(McpToolError::Unavailable(message)) if message == "catalog down"
        ));
        assert_eq!(lists(&counter), 3);
    }

    /// The final reply is checked once, where it leaves the behaviors.
    #[tokio::test]
    async fn replies_are_checked_where_they_leave_the_behaviors() {
        let auth = owner_auth();
        let content = ToolReply::Content(vec![ToolContent::text("read me")]);
        let failure = ToolReply::Failure(vec![ToolContent::text("token sk-live-1234 rejected")]);
        let without_schema = || {
            McpHostTool::new(
                "host_out",
                "no output schema",
                serde_json::json!({"type": "object"}),
                READ,
            )
        };

        // A tool without an output schema may answer content.
        let (server, _) = scripted(Ok(vec![without_schema()]), content.clone());
        let reply = server
            .call_tool_reply_in_request(
                "host_out",
                serde_json::json!({}),
                author(),
                Some(auth.clone()),
                FlavorServices::default(),
            )
            .await
            .expect("content from a tool that declared no schema");
        assert_eq!(reply, content);
        // A JSON-only surface cannot carry it.
        let json = server
            .call_tool(
                "host_out",
                serde_json::json!({}),
                author(),
                Some(auth.clone()),
            )
            .await;
        assert!(
            matches!(
                &json,
                Err(ToolInvocationError::Tool(McpToolError::Other(_)))
            ),
            "{json:?}"
        );

        // A tool that declared an output schema may not.
        let (server, _) = scripted(Ok(vec![host_tool("host_out", READ)]), content);
        let error = server
            .call_tool_reply_in_request(
                "host_out",
                serde_json::json!({}),
                author(),
                Some(auth.clone()),
                FlavorServices::default(),
            )
            .await
            .expect_err("content against a declared schema is refused");
        let ToolInvocationError::Tool(error) = error else {
            panic!("expected an internal tool error, got {error:?}");
        };
        assert_eq!(error.kind(), McpToolErrorKind::Internal);
        assert_eq!(error.client_message(), "internal server error");

        // A failure is the host's own content: delivered as built, schema or no.
        let (server, _) = scripted(Ok(vec![host_tool("host_out", READ)]), failure.clone());
        let reply = server
            .call_tool_reply_in_request(
                "host_out",
                serde_json::json!({}),
                author(),
                Some(auth),
                FlavorServices::default(),
            )
            .await
            .expect("a failure passes");
        assert_eq!(reply, failure);
    }

    /// A behavior that answers content for every call.
    #[derive(Debug)]
    struct ContentBehavior;

    #[async_trait::async_trait]
    impl proxima_core::RequestBehavior for ContentBehavior {
        async fn handle(&self, _: ToolCall, _: Next<'_>) -> Result<ToolReply, McpToolError> {
            Ok(ToolReply::Content(vec![ToolContent::text("replaced")]))
        }
    }

    /// Every registry tool declares an output schema, and a resource is JSON:
    /// a behavior that replaces either with content makes an internal error.
    #[tokio::test]
    async fn a_behavior_cannot_turn_a_registry_tool_or_a_resource_into_content() {
        let mut registry = FlavorRegistry::new();
        registry.add_request_behavior(ContentBehavior);
        let server = McpToolHost::from_parts(
            Arc::new(registry.freeze_or_panic_for_tests()),
            FlavorServices::default(),
        );
        let auth = owner_auth();

        let tool = server
            .call_tool_reply_in_request(
                "core_memory_spaces",
                serde_json::json!({}),
                author(),
                Some(auth.clone()),
                FlavorServices::default(),
            )
            .await;
        assert!(
            matches!(
                &tool,
                Err(ToolInvocationError::Tool(McpToolError::Other(_)))
            ),
            "{tool:?}"
        );
        let resource = server
            .read_resource("proxima://schemas", author(), Some(auth))
            .await;
        assert!(
            matches!(
                &resource,
                Err(ToolInvocationError::Tool(McpToolError::Other(_)))
            ),
            "{resource:?}"
        );
    }

    #[tokio::test]
    async fn call_tool_without_bound_auth_is_denied() {
        let server = make_server();
        let author = McpAuthorContext {
            model_id: "test-model".into(),
            trusted_model_id: None,
            client_name: "test-client".into(),
            client_version: "0.1.0".into(),
            caller_self_perspective: None,
        };

        let err = server
            .call_tool("core_search_memories", serde_json::json!({}), author, None)
            .await
            .expect_err("missing bound owner/auth must deny");

        assert!(matches!(err, ToolInvocationError::NotAuthorized(_)));
    }
}
