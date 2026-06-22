//! rmcp 1.6 dynamic tool projection.
//!
//! The SDK exposes dynamic tools through direct
//! `ServerHandler::list_tools` / `call_tool` overrides. This adapter
//! projects the frozen build-time `FlavorRegistry` tool descriptors
//! into MCP tool metadata at request time.

use std::borrow::Cow;
use std::collections::BTreeSet;
use std::sync::Arc;

use proxima_core::mcp::provider_safe_tool_name;
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
use crate::server::McpToolHost;
use proxima_core::ToolScope;

/// MCP behavior hints (`ToolAnnotations`) for a core tool, keyed by its
/// canonical id (`core/...`, pre-`provider_safe` form). Every core tool acts
/// on the closed memory substrate, so `open_world_hint = false` for all of
/// them; the per-tool split below sets read-only vs. write semantics.
///
/// Single source of truth for substrate annotations. Embedding hosts that
/// re-list these tools from their own endpoint (e.g. the Nexus gateway, which
/// pins this crate at a fixed rev and cannot see a trait-level const) keep a
/// mirrored table — keep the two in sync.
fn core_tool_annotations(canonical_name: &str) -> Option<ToolAnnotations> {
    // Closed-world base; reads/writes refine it below.
    let base = ToolAnnotations::new().open_world(false);
    let annotations = match canonical_name {
        // Reads — never modify the substrate.
        "core/citation_of_fact"
        | "core/citation_of_entity_head"
        | "core/facts_citing_object"
        | "core/get_graph"
        | "core/get_memory"
        | "core/get_personality"
        | "core/list_edge_types"
        | "core/list_events"
        | "core/list_personalities"
        | "core/list_read_scope"
        | "core/list_schemas"
        | "core/list_substrate_tools"
        | "core/list_wake_entries"
        | "core/search_memories"
        | "core/walk_memory_lineage" => base.read_only(true),

        // Additive writes that converge on replay: a required idempotency key
        // (goal_decompose), a set-to-value retention, or an id-keyed wake-entry
        // update — re-running with the same args lands the same state.
        "core/derive"
        | "core/goal_decompose"
        | "core/set_fact_retention"
        | "core/update_wake_entry" => base.read_only(false).destructive(false).idempotent(true),

        // Additive writes that are NOT replay-safe. remember / record_utterance
        // and the optional-key goal writes allocate a fresh id when the
        // (optional) idempotency_key is omitted, so identical args create a new
        // Fact/version rather than a no-op; link / add_wake_entry /
        // instantiate_personality mint a fresh entity each call; set_read_scope
        // converges its grant rows but emits a before/after audit Fact whose key
        // differs on the post-change replay.
        "core/remember"
        | "core/record_utterance"
        | "core/goal_set"
        | "core/goal_transition"
        | "core/goal_mark_achieved"
        | "core/goal_modify"
        | "core/set_read_scope"
        | "core/link"
        | "core/add_wake_entry"
        | "core/instantiate_personality" => {
            base.read_only(false).destructive(false).idempotent(false)
        }

        // Destructive writes that converge on replay: erase due facts /
        // remove one entry — a second same-args call is a no-op.
        "core/cleanup_facts" | "core/remove_wake_entry" => {
            base.read_only(false).destructive(true).idempotent(true)
        }

        // Destructive writes that are NOT replay-safe: set_wake_entries
        // allocates fresh ids for keyless entries (replace churns ids);
        // tombstone_personality emits a before-snapshot audit Fact that differs
        // once the personality is already tombstoned.
        "core/set_wake_entries" | "core/tombstone_personality" => {
            base.read_only(false).destructive(true).idempotent(false)
        }

        // Unknown / flavor-shipped tools: leave hints unset (client defaults).
        _ => return None,
    };
    Some(annotations)
}

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
                    Some(annotations) => tool.annotate(annotations),
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
                    Some(annotations) => tool.annotate(annotations),
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
                .map_err(|err| ErrorData::internal_error(err.to_string(), None))?;
            let text = serde_json::to_string(&output)
                .map_err(|err| ErrorData::internal_error(err.to_string(), None))?;
            let mut result = CallToolResult::success(vec![Content::text(text)]);
            result.structured_content = Some(output);
            Ok(result)
        }
    }
}

fn tool_name_matches(canonical: &str, request_name: &str) -> bool {
    canonical == request_name || provider_safe_tool_name(canonical) == request_name
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
        Some(scope) => scope.allows(name),
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

    // Completeness gate: every `core/*` tool the substrate registers must
    // carry MCP annotations, so a newly-added core tool cannot silently ship
    // with unset hints (client defaults: not-read-only, destructive,
    // open-world — wrong for this closed substrate).
    #[test]
    fn every_core_tool_is_annotated() {
        let registry = proxima_core::FlavorRegistry::new().freeze();
        for descriptor in registry.list_mcp_tools() {
            if descriptor.name.starts_with("core/") {
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
        let read = core_tool_annotations("core/search_memories").expect("read tool");
        assert_eq!(read.read_only_hint, Some(true));
        assert_eq!(read.open_world_hint, Some(false));

        // Convergent additive write (required idempotency key).
        let derive = core_tool_annotations("core/derive").expect("write tool");
        assert_eq!(derive.read_only_hint, Some(false));
        assert_eq!(derive.destructive_hint, Some(false));
        assert_eq!(derive.idempotent_hint, Some(true));

        // Additive write with an OPTIONAL idempotency key: identical args
        // without a key create a new Fact, so it is not replay-safe.
        let remember = core_tool_annotations("core/remember").expect("non-idempotent write");
        assert_eq!(remember.read_only_hint, Some(false));
        assert_eq!(remember.destructive_hint, Some(false));
        assert_eq!(remember.idempotent_hint, Some(false));

        // Destructive write that converges (a second call is a no-op).
        let cleanup = core_tool_annotations("core/cleanup_facts").expect("destructive tool");
        assert_eq!(cleanup.read_only_hint, Some(false));
        assert_eq!(cleanup.destructive_hint, Some(true));
        assert_eq!(cleanup.idempotent_hint, Some(true));

        // Destructive write that is NOT replay-safe (audit-Fact divergence).
        let tombstone =
            core_tool_annotations("core/tombstone_personality").expect("destructive tool");
        assert_eq!(tombstone.destructive_hint, Some(true));
        assert_eq!(tombstone.idempotent_hint, Some(false));

        // Create-new-each-call write is not replay-safe.
        let link = core_tool_annotations("core/link").expect("create tool");
        assert_eq!(link.idempotent_hint, Some(false));

        // Flavor-shipped / unknown tools get no substrate hints here.
        assert!(core_tool_annotations("company/upsert").is_none());
    }
}
