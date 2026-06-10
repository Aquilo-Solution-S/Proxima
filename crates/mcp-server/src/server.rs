use std::sync::Arc;
use std::time::Instant;

use proxima_core::mcp::{McpAuthorContext, McpToolCtx, McpToolError, OutputMode};
use proxima_core::personality::{
    PersonalityTool, PersonalityToolContext, substrate_pack, writeable_relations_for_palette,
    writeable_schemas_for_palette,
};
use proxima_core::{
    Engine, FlavorRegistry, FlavorRegistryFrozen, Owner, WakeInvocationLogDraft,
    WakeInvocationLogStatus,
};

use crate::auth::McpAuthContext;

#[derive(Clone)]
pub struct McpToolHost {
    pool: sqlx::PgPool,
    owner: Owner,
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
    ///
    /// Runs only the substrate migrations. Flavor sidecar migrations
    /// (including proxima-mcp-substrate's agent-note tables) are the
    /// composing host's responsibility — run each linked flavor's
    /// `migrator()` before serving tool calls.
    pub async fn from_database_url(
        database_url: &str,
        owner: Owner,
        registry: FlavorRegistry,
    ) -> Result<Self, crate::McpServerError> {
        let pg = proxima_storage_pg::PgStorage::connect(database_url).await?;
        pg.run_migrations().await?;
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

    /// Build a per-call `McpToolCtx` derived from the auth regime.
    ///
    /// Wake-dispatched calls (`auth.wake.is_some()`) receive the
    /// wake's `HandleTable` and `OutputMode::Handles`. Master-token
    /// and unauthenticated calls receive no table and
    /// `OutputMode::RawIds`.
    #[must_use]
    pub fn ctx_for(
        &self,
        author: McpAuthorContext,
        owner: Option<Owner>,
        auth: Option<&McpAuthContext>,
    ) -> McpToolCtx {
        let (handles, mode) = match auth.and_then(|a| a.wake.as_ref()) {
            Some(wake) => (Some(wake.handles.clone()), OutputMode::Handles),
            None => (None, OutputMode::RawIds),
        };
        let master_token_id = auth.and_then(|c| c.master_token_id);
        McpToolCtx {
            pool: self.pool.clone(),
            owner: owner.unwrap_or_else(|| self.owner.clone()),
            handles,
            mode,
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
            let result = (descriptor.call)(self.ctx_for(author, owner, auth.as_ref()), args).await;
            if let (Some(engine), Some(auth)) = (self.engine.as_ref(), auth.as_ref()) {
                let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
                match &result {
                    Ok(content) => {
                        append_tool_log(
                            engine,
                            auth,
                            name,
                            WakeInvocationLogStatus::Succeeded,
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
                            WakeInvocationLogStatus::Failed,
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
            wake.handles.clone(),
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
                    WakeInvocationLogStatus::Succeeded,
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
                    WakeInvocationLogStatus::Failed,
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
        let allows =
            |name: &str| proxima_core::personality::palette_authorizes_internal_tool(palette, name);
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
                    args_schema: harness_substrate_args_schema(
                        self.registry(),
                        tool.tool_id(),
                        tool.args_schema(),
                    ),
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
        if !proxima_core::personality::palette_authorizes_internal_tool(
            &wake.palette,
            &call.canonical_name,
        ) {
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

fn harness_substrate_args_schema(
    registry: &proxima_core::FlavorRegistryFrozen,
    tool_id: &str,
    fallback: serde_json::Value,
) -> serde_json::Value {
    match tool_id {
        "core/emit_abstraction" => emit_memory_args_schema(
            registry,
            proxima_core::verbs::schema::PayloadKind::Abstraction,
            fallback,
        ),
        "core/emit_perspective" => emit_memory_args_schema(
            registry,
            proxima_core::verbs::schema::PayloadKind::Perspective,
            fallback,
        ),
        _ => fallback,
    }
}

fn emit_memory_args_schema(
    registry: &proxima_core::FlavorRegistryFrozen,
    kind: proxima_core::verbs::schema::PayloadKind,
    fallback: serde_json::Value,
) -> serde_json::Value {
    let registry_schemas = registry.list();
    let schemas = registry_schemas
        .iter()
        .filter(|schema| schema.kind == kind)
        .collect::<Vec<_>>();
    if schemas.is_empty() {
        return fallback;
    }

    let text_schema = serde_json::json!({
        "type": ["string", "null"],
        "description": "Optional authored text. Omit or null to derive text from payload."
    });
    let branches = schemas
        .iter()
        .map(|schema| {
            let payload_schema = registry
                .payload_json_schema(&schema.schema_id, schema.schema_version, kind)
                .cloned()
                .unwrap_or_else(|| {
                    serde_json::json!({
                        "type": "object",
                        "description": "Typed payload for the selected schema_id."
                    })
                });
            serde_json::json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["schema_id", "schema_version", "payload"],
                "properties": {
                    "schema_id": {
                        "type": "string",
                        "enum": [schema.schema_id.as_str()],
                        "description": "Registered schema id to emit."
                    },
                    "schema_version": {
                        "type": "integer",
                        "enum": [schema.schema_version.into_inner()],
                        "description": "Registered schema version."
                    },
                    "payload": payload_schema,
                    "text": text_schema.clone()
                }
            })
        })
        .collect::<Vec<_>>();

    if branches.len() == 1 {
        branches.into_iter().next().expect("one schema")
    } else {
        serde_json::json!({
            "type": "object",
            "oneOf": branches,
            "description": "Emit one registered typed memory payload."
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
    status: WakeInvocationLogStatus,
    duration_ms: u64,
    message_tail: Option<String>,
) {
    let Some(wake) = auth.wake.as_ref() else {
        return;
    };
    let log = WakeInvocationLogDraft {
        invocation_id: wake.invocation_id(),
        owner: auth.owner.clone(),
        personality_instance_id: wake.personality_instance_id(),
        wake_entry_id: wake.wake_entry_id,
        change_event_seq: wake.change_event_seq,
        phase: "tool_call".to_string(),
        tool_id: Some(tool_id.to_string()),
        status,
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
    use crate::auth::{McpAuthContext, McpToolScope};
    use proxima_core::mcp::{HarnessSubstrateBridge, McpAuthorContext};
    use proxima_core::{AbstractionPayload, FlavorRegistry, OrgId, Owner, Principal, UserId};

    #[derive(
        Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
    )]
    struct TestBriefPayload {
        acceptance_rubric: Vec<String>,
    }

    impl AbstractionPayload for TestBriefPayload {
        const SCHEMA_ID: &'static str = "test/brief-v1";
        const SCHEMA_VERSION: u32 = 1;

        fn sidecar_table() -> &'static str {
            "test.brief_v1"
        }

        fn json_schema() -> Option<serde_json::Value> {
            Some(
                serde_json::to_value(schemars::schema_for!(Self))
                    .expect("TestBriefPayload schema serializes"),
            )
        }
    }

    #[derive(
        Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
    )]
    struct TestOtherPayload {
        title: String,
    }

    impl AbstractionPayload for TestOtherPayload {
        const SCHEMA_ID: &'static str = "test/other-v1";
        const SCHEMA_VERSION: u32 = 1;

        fn sidecar_table() -> &'static str {
            "test.other_v1"
        }

        fn json_schema() -> Option<serde_json::Value> {
            Some(
                serde_json::to_value(schemars::schema_for!(Self))
                    .expect("TestOtherPayload schema serializes"),
            )
        }
    }

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
            registry: Arc::new(FlavorRegistry::new().freeze()),
            pool,
            engine: None,
        }
    }

    fn master_token_auth(owner: Owner, token: uuid::Uuid) -> McpAuthContext {
        McpAuthContext {
            owner,
            scope: McpToolScope::All,
            model_id: None,
            wake: None,
            master_token_id: Some(token),
        }
    }

    #[tokio::test]
    async fn emit_abstraction_harness_schema_uses_registered_payload_schema() {
        let mut registry = FlavorRegistry::new();
        registry.add_abstraction_schema::<TestBriefPayload>();
        registry.add_abstraction_schema::<TestOtherPayload>();
        let pool = sqlx::PgPool::connect_lazy("postgres://placeholder/db").expect("lazy pool");
        let server = McpToolHost {
            owner: fake_owner(),
            registry: Arc::new(registry.freeze()),
            pool,
            engine: None,
        };

        let tools =
            HarnessSubstrateBridge::list_harness_tools(&server, &["core/emit_abstraction".into()]);
        assert_eq!(tools.len(), 1);

        let args_schema = &tools[0].args_schema;
        let branch = args_schema
            .pointer("/oneOf")
            .and_then(serde_json::Value::as_array)
            .and_then(|branches| {
                branches.iter().find(|branch| {
                    branch
                        .pointer("/properties/schema_id/enum/0")
                        .and_then(serde_json::Value::as_str)
                        == Some("test/brief-v1")
                })
            })
            .expect("brief branch present");
        assert_eq!(
            branch
                .pointer("/properties/schema_id/enum/0")
                .and_then(serde_json::Value::as_str),
            Some("test/brief-v1")
        );
        assert_eq!(
            branch
                .pointer("/properties/payload/properties/acceptance_rubric/type")
                .and_then(serde_json::Value::as_str),
            Some("array")
        );
    }

    #[tokio::test]
    async fn ctx_for_threads_master_token_id_in_raw_ids_mode() {
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
        assert_eq!(ctx.mode, OutputMode::RawIds);
        assert!(ctx.handles.is_none());

        let ctx_no_auth = server.ctx_for(author, None, None);
        assert_eq!(ctx_no_auth.master_token_id, None);
        assert_eq!(ctx_no_auth.mode, OutputMode::RawIds);
        assert!(ctx_no_auth.handles.is_none());
    }

    #[tokio::test]
    async fn ctx_for_wake_dispatched_runs_in_handles_mode() {
        use proxima_core::MemoryId;
        use proxima_core::mcp::{HandleTable, MemoryHandleClass};
        use proxima_core::personality::WakeChainDepth;
        use proxima_core::wake::token_store::WakeTokenContext;

        let server = make_server();
        let author = McpAuthorContext {
            model_id: "test-model".into(),
            client_name: "test-client".into(),
            client_version: "0.1.0".into(),
            caller_self_perspective: None,
        };
        let owner = fake_owner();
        let wake_handles = Arc::new(HandleTable::new());
        let wake = WakeTokenContext {
            invocation_id: uuid::Uuid::now_v7(),
            personality_instance_id: uuid::Uuid::now_v7(),
            wake_entry_id: uuid::Uuid::now_v7(),
            change_event_seq: uuid::Uuid::now_v7(),
            owner: owner.clone(),
            palette: Vec::new(),
            model_id: "test/model".into(),
            max_rounds: 4,
            current_root_perspective_memory_id: MemoryId::new(uuid::Uuid::now_v7()),
            current_root_perspective_memory_class: MemoryHandleClass::Perspective,
            triggering_event_memory_id: MemoryId::new(uuid::Uuid::now_v7()),
            triggering_event_memory_class: MemoryHandleClass::Fact,
            triggering_event_depth: WakeChainDepth::new(0),
            read_log: Arc::new(tokio::sync::Mutex::new(Vec::new())),
            handles: wake_handles.clone(),
        };
        let auth = McpAuthContext {
            owner: owner.clone(),
            scope: McpToolScope::All,
            model_id: Some("test/model".into()),
            wake: Some(wake),
            master_token_id: None,
        };

        let ctx = server.ctx_for(author, None, Some(&auth));
        assert_eq!(ctx.mode, OutputMode::Handles);
        let ctx_handles = ctx.handles.expect("wake regime carries handles");
        assert!(Arc::ptr_eq(&ctx_handles, &wake_handles));
        assert_eq!(ctx.master_token_id, None);
    }
}
