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
pub struct DevMcpServer {
    pool: sqlx::PgPool,
    owner: Owner,
    handles: Arc<HandleTable>,
    registry: Arc<FlavorRegistryFrozen>,
    engine: Option<Arc<Engine>>,
}

impl std::fmt::Debug for DevMcpServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DevMcpServer")
            .field("owner", &self.owner)
            .field("has_engine", &self.engine.is_some())
            .finish_non_exhaustive()
    }
}

impl DevMcpServer {
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
    pub fn ctx(&self, author: McpAuthorContext, owner: Option<Owner>) -> McpToolCtx {
        McpToolCtx {
            pool: self.pool.clone(),
            owner: owner.unwrap_or_else(|| self.owner.clone()),
            handles: self.handles.clone(),
            registry: self.registry.clone(),
            caller_self_perspective: author.caller_self_perspective,
            author,
            master_token_id: None,
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
        if let Some(descriptor) = self
            .registry
            .list_mcp_tools()
            .iter()
            .find(|d| d.name == name)
        {
            let owner = auth.as_ref().map(|ctx| ctx.owner.clone());
            let started = Instant::now();
            let result = (descriptor.call)(self.ctx(author, owner), args).await;
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
                            summarize_tool_content(content),
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
                append_tool_log(engine, auth, name, "succeeded", duration_ms, message_tail).await;
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

fn summarize_tool_content(content: &serde_json::Value) -> Option<String> {
    if let Some(error) = content.get("error").and_then(serde_json::Value::as_str) {
        return Some(tail_chars(error, 2_000));
    }
    let keys = content
        .as_object()
        .map(|obj| obj.keys().cloned().collect::<Vec<_>>().join(", "));
    Some(match keys {
        Some(keys) if !keys.is_empty() => format!("ok: keys [{keys}]"),
        _ => "ok".to_string(),
    })
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
