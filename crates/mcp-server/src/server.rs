use std::sync::Arc;

#[cfg(test)]
use proxima_core::AuthPath;
use proxima_core::mcp::core_tools::{
    get_graph::{GetGraphArgs, get_graph},
    get_memory::{GetMemoryArgs, get_memory},
    list_change_events::{ListChangeEventsArgs, list_change_events},
    list_edge_types::{ListEdgeTypesArgs, list_edge_types},
    list_schemas::{ListSchemasArgs, list_schemas},
    list_substrate_tools::{ListSubstrateToolsArgs, list_substrate_tools},
    walk_memory_lineage::{
        WalkMemoryLineageArgs, WalkMemoryLineageDirectionArg, walk_memory_lineage,
    },
};
use proxima_core::mcp::{
    McpAuthorContext, McpToolCtx, McpToolError, McpToolErrorKind, McpToolExtensions, Next,
    OutputMode, TerminalDispatch, ToolCall, tool_name_matches,
};
use proxima_core::protocol::{
    resource as protocol_resource, resource_path as protocol_resource_path,
};
use proxima_core::{AuthzContext, Engine, FlavorRegistry, FlavorRegistryFrozen, Owner};
use serde::Serialize;

type ResolvedAuthz = AuthzContext;

use crate::auth::McpAuthContext;

#[derive(Clone)]
pub struct McpToolHost {
    owner: Owner,
    registry: Arc<FlavorRegistryFrozen>,
    extensions: McpToolExtensions,
    engine: Option<Arc<Engine>>,
}

impl std::fmt::Debug for McpToolHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpToolHost")
            .field("owner", &self.owner)
            .field("has_engine", &self.engine.is_some())
            .finish_non_exhaustive()
    }
}

impl McpToolHost {
    #[must_use]
    pub fn from_parts(
        owner: Owner,
        registry: Arc<FlavorRegistryFrozen>,
        extensions: McpToolExtensions,
    ) -> Self {
        Self {
            owner,
            registry,
            extensions,
            engine: None,
        }
    }

    #[must_use]
    pub fn from_engine(engine: Arc<Engine>, owner: Owner, extensions: McpToolExtensions) -> Self {
        Self::from_parts(owner, Arc::new(engine.registry().clone()), extensions).with_engine(engine)
    }

    #[must_use]
    pub fn with_engine(mut self, engine: Arc<Engine>) -> Self {
        self.engine = Some(engine);
        self
    }

    /// # Errors
    ///
    /// Returns storage or migration failures.
    ///
    /// Runs only the substrate migrations. Flavor sidecar migrations
    /// (including core memory agent-note tables) are the
    /// composing host's responsibility — run each linked flavor's
    /// `migrator()` before serving tool calls.
    pub async fn from_database_url(
        database_url: &str,
        owner: Owner,
        registry: FlavorRegistry,
    ) -> Result<Self, crate::McpServerError> {
        let pg = proxima_storage_pg::PgStorage::connect(database_url).await?;
        pg.run_migrations().await?;
        let frozen = registry.try_freeze()?;
        let engine = Arc::new(
            Engine::new(frozen.clone()).with_storage_ports(Arc::new(pg.clone()).storage_ports()),
        );
        Ok(Self::from_engine(
            engine,
            owner,
            McpToolExtensions::default(),
        ))
    }

    #[must_use]
    pub fn registry(&self) -> &FlavorRegistryFrozen {
        &self.registry
    }

    /// Build a per-call `McpToolCtx` derived from the auth regime.
    ///
    /// Master-token, host-bearer, and unauthenticated test calls receive
    /// no handle table and `OutputMode::PrefixedIds`.
    #[must_use]
    pub fn ctx_for(
        &self,
        author: McpAuthorContext,
        owner: Option<Owner>,
        auth: Option<&McpAuthContext>,
    ) -> McpToolCtx {
        let owner = owner.unwrap_or(self.owner);
        // Wire requests always carry Some(auth): `mcp_auth_layer` 401s
        // unauthenticated requests before dispatch, and the facade always
        // passes Some(authz). A None here is either an in-crate test
        // scaffold (test builds) or a transport that nested `/mcp` without
        // the auth layer (a regression) — see `unauthenticated_authz`.
        let authz = match auth {
            Some(a) => a.authz.clone(),
            None => Self::unauthenticated_authz(&owner),
        };
        let master_token_id = auth.and_then(|c| c.master_token_id);
        McpToolCtx {
            owner,
            authz,
            handles: None,
            mode: OutputMode::PrefixedIds,
            registry: self.registry.clone(),
            caller_self_perspective: author.caller_self_perspective,
            master_token_id,
            extensions: self.extensions.clone(),
            author,
            engine: self.engine.clone(),
        }
    }

    /// Authz for a host call that arrived without a bound
    /// `McpAuthContext`.
    ///
    /// Release builds never legitimately reach this: the wire path is
    /// gated by `mcp_auth_layer` (401 before dispatch) and the facade
    /// always passes `Some(authz)`. If a future transport nests `/mcp`
    /// without the auth layer and dispatches here, fail closed with a
    /// zero-capability context instead of minting System admin. The
    /// permissive test arm below is compiled out of release builds, so
    /// the admin fallback cannot silently return.
    #[cfg(not(test))]
    fn unauthenticated_authz(owner: &Owner) -> ResolvedAuthz {
        AuthzContext::denied_for_owner(owner)
    }

    /// Test scaffolds call the host directly without an auth layer and
    /// rely on a full single-owner context. Compiled out of release
    /// builds (see the release arm above).
    #[cfg(test)]
    fn unauthenticated_authz(owner: &Owner) -> ResolvedAuthz {
        AuthzContext::single_owner(owner, AuthPath::HostBearer)
    }

    /// # Errors
    ///
    /// Returns `ToolNotFound` or the called tool error.
    pub async fn call_tool(
        &self,
        name: &str,
        args: serde_json::Value,
        author: McpAuthorContext,
        auth: Option<McpAuthContext>,
    ) -> Result<serde_json::Value, ToolInvocationError> {
        if let Some(descriptor) = self
            .registry
            .list_mcp_tools()
            .iter()
            .find(|d| tool_name_matches(d.name, name))
        {
            let owner = auth.as_ref().map(|ctx| ctx.owner);
            let ctx = self.ctx_for(author, owner, auth.as_ref());
            let call_fn = descriptor.call;
            let terminal: TerminalDispatch<'_> = Box::new(move |call| {
                let ToolCall { args, ctx, .. } = call;
                call_fn(ctx, args)
            });
            return self
                .dispatch_through_behaviors(descriptor.name.to_string(), args, ctx, terminal)
                .await;
        }

        Err(ToolInvocationError::ToolNotFound(name.to_string()))
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
        let parsed = parse_resource_uri(uri).ok_or_else(|| {
            ToolInvocationError::Tool(McpToolError::InvalidInput(format!(
                "unknown resource {uri}"
            )))
        })?;
        let owner = auth.as_ref().map(|ctx| ctx.owner);
        let ctx = self.ctx_for(author, owner, auth.as_ref());
        let scope_key = parsed.scope_key();

        let terminal: TerminalDispatch<'_> = Box::new(move |call| {
            Box::pin(async move { dispatch_resource(parsed, call.ctx).await })
        });
        self.dispatch_through_behaviors(
            scope_key.to_string(),
            serde_json::json!({ "uri": uri }),
            ctx,
            terminal,
        )
        .await
    }

    /// Run a call through the shared `RequestBehavior` onion (currently just
    /// `ScopeGateBehavior`, plus any flavor-registered behaviors) and the
    /// given terminal dispatch. Both `call_tool` and `read_resource` funnel
    /// through here so allow/deny/log behavior matches for tools and
    /// resources alike — `read_resource` used to run its own hand-rolled
    /// scope check outside this chain.
    async fn dispatch_through_behaviors<'a>(
        &'a self,
        name: String,
        args: serde_json::Value,
        ctx: McpToolCtx,
        terminal: TerminalDispatch<'a>,
    ) -> Result<serde_json::Value, ToolInvocationError> {
        Next::new(self.registry.request_behaviors(), terminal)
            .run(ToolCall { name, args, ctx })
            .await
            .map_err(Into::into)
    }
}

async fn dispatch_resource(
    parsed: ParsedResource,
    ctx: McpToolCtx,
) -> Result<serde_json::Value, McpToolError> {
    match parsed {
        ParsedResource::Schemas(args) => resource_output_value(list_schemas(ctx, args).await?),
        ParsedResource::EdgeTypes(args) => resource_output_value(list_edge_types(ctx, args).await?),
        ParsedResource::Tools(args) => {
            resource_output_value(list_substrate_tools(ctx, args).await?)
        }
        ParsedResource::Graph(args) => resource_output_value(get_graph(ctx, args).await?),
        ParsedResource::Memory(args) => resource_output_value(get_memory(ctx, args).await?),
        ParsedResource::MemoryLineage(args) => {
            resource_output_value(walk_memory_lineage(ctx, args).await?)
        }
        ParsedResource::ChangeEvents(args) => {
            resource_output_value(list_change_events(ctx, args).await?)
        }
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

enum ParsedResource {
    Schemas(ListSchemasArgs),
    EdgeTypes(ListEdgeTypesArgs),
    Tools(ListSubstrateToolsArgs),
    Graph(GetGraphArgs),
    Memory(GetMemoryArgs),
    MemoryLineage(WalkMemoryLineageArgs),
    ChangeEvents(ListChangeEventsArgs),
}

impl ParsedResource {
    const fn scope_key(&self) -> &'static str {
        match self {
            Self::Schemas(_) => protocol_resource::SCHEMAS,
            Self::EdgeTypes(_) => protocol_resource::EDGE_TYPES,
            Self::Tools(_) => protocol_resource::TOOLS,
            Self::Graph(_) => protocol_resource::GRAPH,
            Self::Memory(_) => protocol_resource::MEMORY,
            Self::MemoryLineage(_) => protocol_resource::MEMORY_LINEAGE,
            Self::ChangeEvents(_) => protocol_resource::CHANGE_EVENTS,
        }
    }
}

fn parse_resource_uri(uri: &str) -> Option<ParsedResource> {
    let rest = uri.strip_prefix("proxima://")?;
    let (path, query) = rest
        .split_once('?')
        .map_or((rest, None), |(path, query)| (path, Some(query)));
    let query = parse_query(query)?;

    match path {
        protocol_resource_path::SCHEMAS => Some(ParsedResource::Schemas(ListSchemasArgs {
            kind: query_value(&query, "kind").map(ToOwned::to_owned),
        })),
        protocol_resource_path::EDGE_TYPES => Some(ParsedResource::EdgeTypes(ListEdgeTypesArgs {})),
        protocol_resource_path::TOOLS => Some(ParsedResource::Tools(ListSubstrateToolsArgs {})),
        protocol_resource_path::GRAPH => Some(ParsedResource::Graph(GetGraphArgs {
            include_tombstoned: query_bool(&query, "include_tombstoned"),
        })),
        protocol_resource_path::CHANGE_EVENTS => {
            Some(ParsedResource::ChangeEvents(ListChangeEventsArgs {
                since: query_value(&query, "since").map(ToOwned::to_owned),
                limit: query_parse(&query, "limit").ok()?,
            }))
        }
        path if path.starts_with("memory/") => parse_memory_resource_path(path, &query),
        _ => None,
    }
}

fn parse_memory_resource_path(path: &str, query: &[(&str, &str)]) -> Option<ParsedResource> {
    let rest = path
        .strip_prefix(protocol_resource_path::MEMORY)?
        .strip_prefix('/')?;
    if let Some(id) = rest.strip_suffix("/lineage") {
        if id.is_empty() || id.contains('/') {
            return None;
        }
        return Some(ParsedResource::MemoryLineage(WalkMemoryLineageArgs {
            memory: id.to_string(),
            direction: query_lineage_direction(query)?,
            depth: query_parse(query, "depth").ok()?.unwrap_or(3),
            limit: query_parse(query, "limit").ok()?.unwrap_or(50),
        }));
    }
    if rest.is_empty() || rest.contains('/') {
        return None;
    }
    Some(ParsedResource::Memory(GetMemoryArgs {
        memory: rest.to_string(),
        expand_neighbors: query_bool(query, "expand_neighbors"),
        space: None,
    }))
}

fn parse_query(query: Option<&str>) -> Option<Vec<(&str, &str)>> {
    let Some(query) = query else {
        return Some(Vec::new());
    };
    if query.is_empty() {
        return Some(Vec::new());
    }
    query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| pair.split_once('='))
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

fn query_parse<T>(query: &[(&str, &str)], key: &str) -> Result<Option<T>, ()>
where
    T: std::str::FromStr,
{
    query_value(query, key).map_or(Ok(None), |value| {
        value.parse::<T>().map(Some).map_err(|_| ())
    })
}

fn query_lineage_direction(query: &[(&str, &str)]) -> Option<WalkMemoryLineageDirectionArg> {
    match query_value(query, "direction") {
        None | Some("ancestors") => Some(WalkMemoryLineageDirectionArg::Ancestors),
        Some("descendants") => Some(WalkMemoryLineageDirectionArg::Descendants),
        Some(_) => None,
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
    use proxima_core::mcp::McpAuthorContext;
    use proxima_core::{FlavorRegistry, Owner, OwnerRef, ToolScope, UserId};

    fn fake_owner() -> Owner {
        OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()))
    }

    fn make_server() -> McpToolHost {
        McpToolHost {
            owner: fake_owner(),
            registry: Arc::new(FlavorRegistry::new().freeze_or_panic_for_tests()),
            extensions: McpToolExtensions::default(),
            engine: None,
        }
    }

    fn master_token_auth(owner: Owner, token: uuid::Uuid) -> McpAuthContext {
        McpAuthContext::for_master(token, owner)
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

        assert!(parse_resource_uri("proxima://memory//lineage").is_none());
        assert!(parse_resource_uri("proxima://memory/F:one/two/lineage").is_none());
        assert!(parse_resource_uri("proxima://change-events?limit=not-a-number").is_none());
    }

    #[test]
    fn resource_constants_match_server_resource_keys() {
        let cases = [
            ("proxima://schemas", protocol_resource::SCHEMAS),
            ("proxima://edge-types", protocol_resource::EDGE_TYPES),
            ("proxima://tools", protocol_resource::TOOLS),
            ("proxima://graph", protocol_resource::GRAPH),
            (
                "proxima://memory/F:018f0000-0000-7000-8000-000000000001",
                protocol_resource::MEMORY,
            ),
            (
                "proxima://memory/A:018f0000-0000-7000-8000-000000000001/lineage",
                protocol_resource::MEMORY_LINEAGE,
            ),
            ("proxima://change-events", protocol_resource::CHANGE_EVENTS),
        ];

        for (uri, scope_key) in cases {
            let parsed = parse_resource_uri(uri).expect("resource parses");
            assert_eq!(parsed.scope_key(), scope_key);
        }
    }

    /// Task 7: `read_resource` now traverses the same `RequestBehavior`
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
            client_name: "test-client".into(),
            client_version: "0.1.0".into(),
            caller_self_perspective: None,
        };
        let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer)
            .with_tool_scope(ToolScope::Palette(Vec::new()));
        let auth = McpAuthContext {
            owner,
            authz,
            model_id: None,
            master_token_id: None,
        };

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
    async fn ctx_for_threads_master_token_id_in_prefixed_ids_mode() {
        let server = make_server();
        let author = McpAuthorContext {
            model_id: "test-model".into(),
            client_name: "test-client".into(),
            client_version: "0.1.0".into(),
            caller_self_perspective: None,
        };
        let token = uuid::Uuid::now_v7();
        let auth = master_token_auth(fake_owner(), token);

        let ctx = server.ctx_for(author.clone(), None, Some(&auth));
        assert_eq!(ctx.master_token_id, Some(token));
        assert_eq!(ctx.mode, OutputMode::PrefixedIds);
        assert!(ctx.handles.is_none());

        let ctx_no_auth = server.ctx_for(author, None, None);
        assert_eq!(ctx_no_auth.master_token_id, None);
        assert_eq!(ctx_no_auth.mode, OutputMode::PrefixedIds);
        assert!(ctx_no_auth.handles.is_none());
    }
}
