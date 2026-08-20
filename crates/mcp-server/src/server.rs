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
    list_schemas::{ListSchemasArgs, list_schemas},
    list_substrate_tools::{ListSubstrateToolsArgs, list_substrate_tools},
    list_wake_candidates::{ListWakeCandidatesArgs, list_wake_candidates},
    walk_memory_lineage::{
        WalkMemoryLineageArgs, WalkMemoryLineageDirectionArg, walk_memory_lineage,
    },
};
use proxima_core::mcp::{
    McpAuthorContext, McpToolCtx, McpToolError, McpToolErrorKind, Next, TerminalDispatch, ToolCall,
    tool_name_matches,
};
use proxima_core::protocol::resource as protocol_resource;
use proxima_core::{Engine, FlavorRegistry, FlavorRegistryFrozen, FlavorServices};
use serde::Serialize;

use crate::auth::McpAuthContext;

#[derive(Clone)]
pub struct McpToolHost {
    registry: Arc<FlavorRegistryFrozen>,
    services: FlavorServices,
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
    pub fn from_parts(registry: Arc<FlavorRegistryFrozen>, services: FlavorServices) -> Self {
        Self {
            registry,
            services,
            engine: None,
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
    #[must_use]
    pub fn ctx_for(&self, author: McpAuthorContext, auth: &McpAuthContext) -> McpToolCtx {
        let owner = auth.owner;
        let authz = auth.authz.clone();
        McpToolCtx {
            owner,
            authz,
            registry: self.registry.clone(),
            caller_self_perspective: author.caller_self_perspective,
            services: self.services.clone(),
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
        let parsed = parse_resource_uri(uri).map_err(|err| err.into_invocation_error(uri))?;
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

    /// Shared `RequestBehavior` onion for `call_tool` and `read_resource`.
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
    use proxima_core::mcp::McpAuthorContext;
    use proxima_core::{AuthzContext, FlavorRegistry, Owner, OwnerRef, ToolScope, UserId};

    fn fake_owner() -> Owner {
        OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()))
    }

    fn make_server() -> McpToolHost {
        McpToolHost {
            registry: Arc::new(FlavorRegistry::new().freeze_or_panic_for_tests()),
            services: FlavorServices::default(),
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
        // depth=300 no longer collapses into "unknown resource": it parses
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

    #[test]
    fn resource_constants_match_server_resource_keys() {
        let cases = [
            ("proxima://schemas", protocol_resource::SCHEMAS),
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
