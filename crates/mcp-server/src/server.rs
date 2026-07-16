use std::sync::Arc;

#[cfg(test)]
use proxima_core::AuthPath;
use proxima_core::mcp::core_tools::{
    get_graph::{GetGraphArgs, get_graph},
    get_memory::{GetMemoryArgs, get_memory},
    goal_reads::{ListGoalsArgs, get_goal, list_goals},
    list_change_events::{ListChangeEventsArgs, list_change_events},
    list_edge_types::{ListEdgeTypesArgs, list_edge_types},
    list_schemas::{ListSchemasArgs, list_schemas},
    list_substrate_tools::{ListSubstrateToolsArgs, list_substrate_tools},
    list_wake_candidates::{ListWakeCandidatesArgs, list_wake_candidates},
    read_edges::{ListEdgesArgs, get_edge, list_edges},
    walk_memory_lineage::{
        WalkMemoryLineageArgs, WalkMemoryLineageDirectionArg, walk_memory_lineage,
    },
};
use proxima_core::mcp::{
    McpAuthorContext, McpToolCtx, McpToolError, McpToolErrorKind, McpToolExtensions, Next,
    TerminalDispatch, ToolCall, tool_name_matches,
};
use proxima_core::protocol::{
    resource as protocol_resource, resource_path as protocol_resource_path,
};
use proxima_core::{Engine, FlavorRegistry, FlavorRegistryFrozen};
use serde::Serialize;

use crate::auth::McpAuthContext;

#[derive(Clone)]
pub struct McpToolHost {
    registry: Arc<FlavorRegistryFrozen>,
    extensions: McpToolExtensions,
    engine: Option<Arc<Engine>>,
}

impl std::fmt::Debug for McpToolHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpToolHost")
            .field("has_engine", &self.engine.is_some())
            .finish_non_exhaustive()
    }
}

impl McpToolHost {
    #[must_use]
    pub fn from_parts(registry: Arc<FlavorRegistryFrozen>, extensions: McpToolExtensions) -> Self {
        Self {
            registry,
            extensions,
            engine: None,
        }
    }

    #[must_use]
    pub fn from_engine(engine: Arc<Engine>, extensions: McpToolExtensions) -> Self {
        Self::from_parts(Arc::new(engine.registry().clone()), extensions).with_engine(engine)
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
        registry: FlavorRegistry,
    ) -> Result<Self, crate::McpServerError> {
        let pg = proxima_storage_pg::PgStorage::connect(database_url).await?;
        pg.run_migrations().await?;
        let frozen = registry.try_freeze()?;
        let engine = Arc::new(
            Engine::new(frozen.clone()).with_storage_ports(Arc::new(pg.clone()).storage_ports()),
        );
        Ok(Self::from_engine(engine, McpToolExtensions::default()))
    }

    #[must_use]
    pub fn registry(&self) -> &FlavorRegistryFrozen {
        &self.registry
    }

    /// Build a per-call `McpToolCtx` derived from the auth regime.
    ///
    /// All references cross the wire as typed prefixed uuids
    /// (`F:`/`A:`/`P:`/`G:`/`E:`).
    #[must_use]
    pub fn ctx_for(&self, author: McpAuthorContext, auth: &McpAuthContext) -> McpToolCtx {
        let owner = auth.owner;
        let authz = auth.authz.clone();
        McpToolCtx {
            owner,
            authz,
            registry: self.registry.clone(),
            caller_self_perspective: author.caller_self_perspective,
            extensions: self.extensions.clone(),
            author,
            engine: self.engine.clone(),
        }
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
        let auth = auth.ok_or_else(|| ToolInvocationError::NotAuthorized(name.to_string()))?;
        if let Some(descriptor) = self
            .registry
            .list_mcp_tools()
            .iter()
            .find(|d| tool_name_matches(d.name, name))
        {
            let ctx = self.ctx_for(author, &auth);
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
        let auth =
            auth.ok_or_else(|| ToolInvocationError::NotAuthorized(parsed.scope_key().to_string()))?;
        let ctx = self.ctx_for(author, &auth);
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
        ParsedResource::WakeCandidates(args) => {
            resource_output_value(list_wake_candidates(ctx, args).await?)
        }
        ParsedResource::Goals(args) => resource_output_value(list_goals(ctx, args).await?),
        ParsedResource::Goal(reference) => resource_output_value(get_goal(ctx, &reference).await?),
        ParsedResource::Edges(args) => resource_output_value(list_edges(ctx, args).await?),
        ParsedResource::Edge(reference) => resource_output_value(get_edge(ctx, &reference).await?),
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
    WakeCandidates(ListWakeCandidatesArgs),
    Goals(ListGoalsArgs),
    Goal(String),
    Edges(ListEdgesArgs),
    Edge(String),
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
            Self::WakeCandidates(_) => protocol_resource::WAKE_CANDIDATES,
            Self::Goals(_) => protocol_resource::GOALS,
            Self::Goal(_) => protocol_resource::GOAL,
            Self::Edges(_) => protocol_resource::EDGES,
            Self::Edge(_) => protocol_resource::EDGE,
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
        protocol_resource_path::GRAPH => Some(ParsedResource::Graph(GetGraphArgs {})),
        protocol_resource_path::CHANGE_EVENTS => {
            Some(ParsedResource::ChangeEvents(ListChangeEventsArgs {
                since: query_value(&query, "since").map(ToOwned::to_owned),
                limit: query_parse(&query, "limit").ok()?,
            }))
        }
        protocol_resource_path::WAKE_CANDIDATES => {
            Some(ParsedResource::WakeCandidates(ListWakeCandidatesArgs {
                fact: query_value(&query, "fact")
                    .filter(|fact| !fact.is_empty())?
                    .to_owned(),
                limit: query_parse(&query, "limit").ok()?,
            }))
        }
        protocol_resource_path::GOALS => Some(ParsedResource::Goals(ListGoalsArgs {
            state: query_value(&query, "state").map(ToOwned::to_owned),
            limit: query_parse(&query, "limit").ok()?,
            cursor: query_value(&query, "cursor").map(ToOwned::to_owned),
        })),
        protocol_resource_path::EDGES => Some(ParsedResource::Edges(ListEdgesArgs {
            relation: query_value(&query, "relation").map(ToOwned::to_owned),
            source: query_value(&query, "source").map(ToOwned::to_owned),
            target: query_value(&query, "target").map(ToOwned::to_owned),
            limit: query_parse(&query, "limit").ok()?,
            cursor: query_value(&query, "cursor").map(ToOwned::to_owned),
            payloads: query_parse(&query, "payloads").ok()?,
        })),
        path if path.starts_with("memory/") => parse_memory_resource_path(path, &query),
        path if path.starts_with("goal/") => {
            let id = path.strip_prefix("goal/")?;
            if id.is_empty() || id.contains('/') {
                return None;
            }
            Some(ParsedResource::Goal(id.to_string()))
        }
        path if path.starts_with("edge/") => {
            let id = path.strip_prefix("edge/")?;
            if id.is_empty() || id.contains('/') {
                return None;
            }
            Some(ParsedResource::Edge(id.to_string()))
        }
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
    use proxima_core::{AuthzContext, FlavorRegistry, Owner, OwnerRef, ToolScope, UserId};

    fn fake_owner() -> Owner {
        OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()))
    }

    fn make_server() -> McpToolHost {
        McpToolHost {
            registry: Arc::new(FlavorRegistry::new().freeze_or_panic_for_tests()),
            extensions: McpToolExtensions::default(),
            engine: None,
        }
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

        let wake = parse_resource_uri(
            "proxima://wake-candidates?fact=F:018f0000-0000-7000-8000-000000000001&limit=5",
        )
        .expect("wake-candidates resource");
        assert!(matches!(
            wake,
            ParsedResource::WakeCandidates(ListWakeCandidatesArgs { limit: Some(5), .. })
        ));
        assert!(parse_resource_uri("proxima://wake-candidates").is_none());
        assert!(parse_resource_uri("proxima://wake-candidates?fact=").is_none());
        assert!(
            parse_resource_uri("proxima://wake-candidates?fact=F:018f0000-0000-7000-8000-000000000001&limit=not-a-number")
                .is_none()
        );
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
            (
                "proxima://wake-candidates?fact=F:018f0000-0000-7000-8000-000000000001",
                protocol_resource::WAKE_CANDIDATES,
            ),
        ];

        for (uri, scope_key) in cases {
            let parsed = parse_resource_uri(uri).expect("resource parses");
            assert_eq!(parsed.scope_key(), scope_key);
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
    async fn ctx_for_speaks_prefixed_ids() {
        let server = make_server();
        let author = McpAuthorContext {
            model_id: "test-model".into(),
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
            model_id: None,
        };

        let ctx = server.ctx_for(author, &auth);
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

    #[tokio::test]
    async fn call_tool_without_bound_auth_is_denied() {
        let server = make_server();
        let author = McpAuthorContext {
            model_id: "test-model".into(),
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
