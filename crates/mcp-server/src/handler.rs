//! rmcp 1.6 dynamic tool projection.
//!
//! The SDK exposes dynamic tools through direct
//! `ServerHandler::list_tools` / `call_tool` overrides. This adapter
//! projects the frozen build-time `FlavorRegistry` tool descriptors
//! into MCP tool metadata at request time.

use std::borrow::Cow;
use std::sync::Arc;

use proxima_core::mcp::provider_safe_tool_name;
use proxima_core::{McpAuthorContext, MemoryId};
use rmcp::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, Content, ErrorData, Implementation, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool,
};
use rmcp::service::{MaybeSendFuture, RequestContext, RoleServer};

use crate::auth::McpAuthContext;
use crate::server::McpToolHost;
use proxima_core::ToolScope;

#[derive(Clone, Debug)]
pub struct DynamicHandler {
    pub server: McpToolHost,
}

impl ServerHandler for DynamicHandler {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info.server_info = Implementation::from_build_env();
        info
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, ErrorData>> + MaybeSendFuture + '_ {
        let auth = auth_context(&context);
        let scope = auth.as_ref().map(|ctx| &ctx.authz.capabilities.tool_scope);
        let mut tools: Vec<Tool> = self
            .server
            .registry()
            .list_mcp_tools()
            .iter()
            .filter(|descriptor| scope_allows(scope, descriptor.name))
            .map(|descriptor| {
                Tool::new(
                    Cow::Owned(provider_safe_tool_name(descriptor.name)),
                    Cow::Borrowed(descriptor.description),
                    Arc::new(rmcp::model::object(descriptor.args_schema.clone())),
                )
            })
            .collect();
        if auth.as_ref().and_then(|ctx| ctx.wake.as_ref()).is_some() {
            tools.extend(
                self.server
                    .substrate_tools()
                    .iter()
                    .filter(|tool| scope_allows(scope, tool.tool_id()))
                    .map(|tool| {
                        Tool::new(
                            Cow::Owned(provider_safe_tool_name(tool.tool_id())),
                            Cow::Borrowed(tool.description()),
                            Arc::new(rmcp::model::object(tool.args_schema())),
                        )
                    }),
            );
        }
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
                Tool::new(
                    Cow::Owned(provider_safe_tool_name(descriptor.name)),
                    Cow::Borrowed(descriptor.description),
                    Arc::new(rmcp::model::object(descriptor.args_schema.clone())),
                )
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
            Ok(CallToolResult::success(vec![Content::text(text)]))
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
        .or_else(|| {
            server
                .substrate_tools()
                .iter()
                .find(|tool| tool_name_matches(tool.tool_id(), request_name))
                .map(|tool| tool.tool_id().to_string())
        })
}

/// Resolve the token scope from the request auth context. Returns `None`
/// when no token-bearing layer ran ahead of rmcp (direct handler tests).
/// Wake tokens carry a palette scope; local master tokens carry all-tools
/// scope.
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
        // No auth context bound to the request — preserve direct handler
        // tests. Token-required posture is enforced one layer up by
        // `mcp_auth_layer` whenever the HTTP transport wires it in.
        None => true,
    }
}

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
    let caller_self_perspective = caller_self_perspective_from_args(args)?.or_else(|| {
        auth.and_then(|ctx| {
            ctx.wake
                .as_ref()
                .map(|wake| wake.current_root_perspective_memory_id)
        })
    });
    Ok(McpAuthorContext {
        model_id,
        client_name: "unknown".into(),
        client_version: "0".into(),
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
}
