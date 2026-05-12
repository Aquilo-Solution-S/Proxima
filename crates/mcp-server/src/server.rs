use std::sync::Arc;
use std::time::Instant;

use proxima_core::mcp::{HandleTable, McpAuthorContext, McpToolCtx, McpToolError};
use proxima_core::personality::{
    PersonalityTool, PersonalityToolContext, substrate_pack, writeable_relations_for_palette,
    writeable_schemas_for_palette,
};
use proxima_core::{Engine, FlavorRegistry, FlavorRegistryFrozen, Owner, WakeInvocationLogDraft};

use crate::auth::McpAuthContext;

#[derive(Clone)]
pub struct McpToolHost {
    pool: sqlx::PgPool,
    owner: Owner,
    handles: Arc<HandleTable>,
    registry: Arc<FlavorRegistryFrozen>,
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
    pub fn from_pool(
        pool: sqlx::PgPool,
        owner: Owner,
        registry: Arc<FlavorRegistryFrozen>,
    ) -> Self {
        Self {
            pool,
            owner,
            handles: Arc::new(HandleTable::new()),
            registry,
            engine: None,
        }
    }

    #[must_use]
    pub fn with_engine(mut self, engine: Arc<Engine>) -> Self {
        self.engine = Some(engine);
        self
    }

    /// # Errors
    ///
    /// Returns storage or migration failures.
    pub async fn from_database_url(
        database_url: &str,
        owner: Owner,
        registry: FlavorRegistry,
    ) -> Result<Self, crate::McpServerError> {
        let pg = proxima_storage_pg::PgStorage::connect(database_url).await?;
        pg.run_migrations().await?;
        proxima_mcp_substrate::migrator().run(pg.pool()).await?;
        Ok(Self::from_pool(
            pg.pool().clone(),
            owner,
            Arc::new(registry.freeze()),
        ))
    }

    #[must_use]
    pub fn pool(&self) -> &sqlx::PgPool {
        &self.pool
    }

    #[must_use]
    pub fn registry(&self) -> &FlavorRegistryFrozen {
        &self.registry
    }

    #[must_use]
    pub fn substrate_tools(&self) -> &'static [Arc<dyn PersonalityTool>] {
        substrate_pack()
    }

    #[must_use]
    pub fn ctx(
        &self,
        author: McpAuthorContext,
        owner: Option<Owner>,
        master_token_id: Option<uuid::Uuid>,
    ) -> McpToolCtx {
        McpToolCtx {
            pool: self.pool.clone(),
            owner: owner.unwrap_or_else(|| self.owner.clone()),
            handles: self.handles.clone(),
            registry: self.registry.clone(),
            caller_self_perspective: author.caller_self_perspective,
            master_token_id,
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
        // M0: For master-token calls without an explicit caller_self_perspective,
        // ensure the per-token shell-author personality and default the field
        // to its Self-Perspective. Wake-token calls already carry
        // caller_self_perspective; explicit override args still win.
        let mut author = author;
        let master_token_id = auth.as_ref().and_then(|c| c.master_token_id);
        if author.caller_self_perspective.is_none()
            && let (Some(token_id), Some(engine), Some(auth_ctx)) =
                (master_token_id, self.engine.as_ref(), auth.as_ref())
        {
            let identity = engine
                .ensure_master_token_personality(&auth_ctx.owner, token_id)
                .await
                .map_err(|err| ToolInvocationError::Tool(McpToolError::Other(err.to_string())))?;
            author.caller_self_perspective = Some(identity.self_perspective_memory_id);
        }

        if let Some(descriptor) = self
            .registry
            .list_mcp_tools()
            .iter()
            .find(|d| d.name == name)
        {
            let owner = auth.as_ref().map(|ctx| ctx.owner.clone());
            let started = Instant::now();
            let result = (descriptor.call)(self.ctx(author, owner, master_token_id), args).await;
            if let (Some(engine), Some(auth)) = (self.engine.as_ref(), auth.as_ref()) {
                let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
                match &result {
                    Ok(content) => {
                        append_tool_log(
                            engine,
                            auth,
                            name,
                            "succeeded",
                            duration_ms,
                            Some(summarize_tool_content(content)),
                        )
                        .await;
                    }
                    Err(err) => {
                        append_tool_log(
                            engine,
                            auth,
                            name,
                            "failed",
                            duration_ms,
                            Some(tail_chars(&err.to_string(), 2_000)),
                        )
                        .await;
                    }
                }
            }
            return result.map_err(Into::into);
        }

        let Some(auth) = auth.filter(|ctx| ctx.wake.is_some()) else {
            return Err(ToolInvocationError::ToolNotFound(name.to_string()));
        };
        self.call_personality_tool(name, args, &auth).await
    }

    async fn call_personality_tool(
        &self,
        name: &str,
        args: serde_json::Value,
        auth: &McpAuthContext,
    ) -> Result<serde_json::Value, ToolInvocationError> {
        let Some(tool) = substrate_pack().iter().find(|tool| tool.tool_id() == name) else {
            return Err(ToolInvocationError::ToolNotFound(name.to_string()));
        };
        let Some(engine) = self.engine.as_ref() else {
            return Err(ToolInvocationError::Tool(McpToolError::Other(
                "wake-scoped substrate tools require an attached engine".into(),
            )));
        };
        let wake = auth.wake.as_ref().expect("checked by caller");
        let ctx = PersonalityToolContext::new(
            engine,
            &auth.owner,
            "core/wake-mcp",
            wake.personality_instance_id(),
            wake.current_root_perspective_memory_id,
            wake.triggering_event_memory_id,
            wake.triggering_event_depth,
            writeable_schemas_for_palette(engine, &wake.palette),
            writeable_relations_for_palette(engine, &wake.palette),
            substrate_pack(),
        )
        .with_wake_invocation(wake)
        .with_read_log(wake.read_log.clone());
        let started = Instant::now();
        let result = tool.invoke(&ctx, args).await;
        let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        match result {
            Ok(result) => {
                let message_tail = summarize_tool_content(&result.content);
                append_tool_log(
                    engine,
                    auth,
                    name,
                    "succeeded",
                    duration_ms,
                    Some(message_tail),
                )
                .await;
                Ok(result.content)
            }
            Err(err) => {
                append_tool_log(
                    engine,
                    auth,
                    name,
                    "failed",
                    duration_ms,
                    Some(tail_chars(&err.to_string(), 2_000)),
                )
                .await;
                Err(ToolInvocationError::Tool(McpToolError::Other(
                    err.to_string(),
                )))
            }
        }
    }
}

#[async_trait::async_trait]
impl proxima_core::mcp::HarnessSubstrateBridge for McpToolHost {
    fn list_harness_tools(
        &self,
        palette: &[String],
    ) -> Vec<proxima_core::mcp::HarnessSubstrateToolSpec> {
        let allows = |name: &str| palette.iter().any(|allowed| allowed == name);
        let mut out = Vec::new();

        for desc in self.registry().list_mcp_tools() {
            if allows(desc.name) {
                out.push(proxima_core::mcp::HarnessSubstrateToolSpec {
                    canonical_name: desc.name.to_string(),
                    description: desc.description.to_string(),
                    args_schema: desc.args_schema.clone(),
                });
            }
        }

        for tool in self.substrate_tools() {
            if allows(tool.tool_id()) {
                out.push(proxima_core::mcp::HarnessSubstrateToolSpec {
                    canonical_name: tool.tool_id().to_string(),
                    description: tool.description().to_string(),
                    args_schema: tool.args_schema(),
                });
            }
        }

        out
    }

    async fn call_harness_tool(
        &self,
        call: proxima_core::mcp::HarnessSubstrateCall,
    ) -> Result<serde_json::Value, proxima_core::mcp::HarnessSubstrateError> {
        let Some(engine) = self.engine.as_ref() else {
            return Err(proxima_core::mcp::HarnessSubstrateError::Tool(
                "wake-scoped substrate dispatch requires an attached engine".into(),
            ));
        };
        let Some(wake) = engine.wake_token_store().resolve(call.wake_token).await else {
            return Err(proxima_core::mcp::HarnessSubstrateError::MissingWakeContext);
        };
        if wake.owner != call.owner {
            return Err(proxima_core::mcp::HarnessSubstrateError::Unauthorized(
                call.canonical_name,
            ));
        }
        if !wake.palette.iter().any(|tool| tool == &call.canonical_name) {
            return Err(proxima_core::mcp::HarnessSubstrateError::Unauthorized(
                call.canonical_name,
            ));
        }

        let mut author = call.author;
        if author.caller_self_perspective.is_none() {
            author.caller_self_perspective = Some(wake.current_root_perspective_memory_id);
        }

        let auth = crate::auth::McpAuthContext {
            owner: wake.owner.clone(),
            scope: crate::auth::McpToolScope::Palette(wake.palette.clone()),
            model_id: Some(wake.model_id.clone()),
            wake: Some(wake),
            master_token_id: None,
        };

        self.call_tool(&call.canonical_name, call.args, author, Some(auth))
            .await
            .map_err(|err| match err {
                crate::server::ToolInvocationError::ToolNotFound(name) => {
                    proxima_core::mcp::HarnessSubstrateError::ToolNotFound(name)
                }
                crate::server::ToolInvocationError::Tool(tool_err) => map_tool_error(tool_err),
            })
    }
}

fn map_tool_error(
    err: proxima_core::mcp::McpToolError,
) -> proxima_core::mcp::HarnessSubstrateError {
    match err {
        proxima_core::mcp::McpToolError::Storage(e) => {
            proxima_core::mcp::HarnessSubstrateError::Storage(e.to_string())
        }
        proxima_core::mcp::McpToolError::LayeringViolation(s) => {
            proxima_core::mcp::HarnessSubstrateError::Layering(s)
        }
        other => proxima_core::mcp::HarnessSubstrateError::Tool(other.to_string()),
    }
}

async fn append_tool_log(
    engine: &Engine,
    auth: &McpAuthContext,
    tool_id: &str,
    status: &str,
    duration_ms: u64,
    message_tail: Option<String>,
) {
    let Some(wake) = auth.wake.as_ref() else {
        return;
    };
    let log = WakeInvocationLogDraft {
        owner: auth.owner.clone(),
        personality_instance_id: wake.personality_instance_id(),
        wake_entry_id: wake.wake_entry_id,
        change_event_seq: wake.change_event_seq,
        phase: "tool_call".to_string(),
        tool_id: Some(tool_id.to_string()),
        status: status.to_string(),
        duration_ms: Some(duration_ms),
        message_tail,
    };
    if let Err(err) = engine.append_wake_invocation_log(&log).await {
        tracing::warn!(error = %err, tool_id, "failed to persist wake tool-call log");
    }
}

fn summarize_tool_content(content: &serde_json::Value) -> String {
    if let Some(error) = content.get("error").and_then(serde_json::Value::as_str) {
        return tail_chars(error, 2_000);
    }
    let keys = content
        .as_object()
        .map(|obj| obj.keys().cloned().collect::<Vec<_>>().join(", "));
    match keys {
        Some(keys) if !keys.is_empty() => format!("ok: keys [{keys}]"),
        _ => "ok".to_string(),
    }
}

fn tail_chars(value: &str, max_chars: usize) -> String {
    let chars: Vec<char> = value.chars().collect();
    let start = chars.len().saturating_sub(max_chars);
    chars[start..].iter().collect()
}

#[derive(Debug, thiserror::Error)]
pub enum ToolInvocationError {
    #[error("tool not found: {0}")]
    ToolNotFound(String),
    #[error("tool error: {0}")]
    Tool(#[from] McpToolError),
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use proxima_core::mcp::{HandleTable, McpAuthorContext};
    use proxima_core::{FlavorRegistry, OrgId, Owner, Principal, UserId};

    fn fake_owner() -> Owner {
        Owner {
            principal: Principal::User(UserId::new(uuid::Uuid::now_v7())),
            org_id: OrgId::new(uuid::Uuid::now_v7()),
        }
    }

    fn make_server() -> McpToolHost {
        let pool = sqlx::PgPool::connect_lazy("postgres://placeholder/db").expect("lazy pool");
        McpToolHost {
            owner: fake_owner(),
            handles: Arc::new(HandleTable::new()),
            registry: Arc::new(FlavorRegistry::new().freeze()),
            pool,
            engine: None,
        }
    }

    #[tokio::test]
    async fn ctx_threads_master_token_id() {
        let server = make_server();
        let author = McpAuthorContext {
            model_id: "test-model".into(),
            client_name: "test-client".into(),
            client_version: "0.1.0".into(),
            caller_self_perspective: None,
        };
        let token = uuid::Uuid::now_v7();

        let ctx = server.ctx(author.clone(), None, Some(token));
        assert_eq!(ctx.master_token_id, Some(token));

        let ctx_no_token = server.ctx(author, None, None);
        assert_eq!(ctx_no_token.master_token_id, None);
    }
}
