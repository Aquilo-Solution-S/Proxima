use std::sync::Arc;

#[cfg(test)]
use proxima_core::AuthPath;
use proxima_core::mcp::{
    McpAuthorContext, McpToolCtx, McpToolError, McpToolErrorKind, McpToolExtensions, OutputMode,
    tool_name_matches,
};
use proxima_core::{AuthzContext, Engine, FlavorRegistry, FlavorRegistryFrozen, Owner};

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
        let frozen = registry.freeze();
        let engine = Arc::new(Engine::new(frozen.clone()).with_storage(Arc::new(pg.clone())));
        Ok(Self::from_pool(pg.pool().clone(), owner, Arc::new(frozen)).with_engine(engine))
    }

    #[must_use]
    pub fn pool(&self) -> &sqlx::PgPool {
        &self.pool
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
        let owner = owner.unwrap_or_else(|| self.owner.clone());
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
            extensions: McpToolExtensions::with(self.pool.clone()),
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
    fn unauthenticated_authz(owner: &Owner) -> AuthzContext {
        AuthzContext::denied(owner)
    }

    /// Test scaffolds call the host directly without an auth layer and
    /// rely on a full single-owner context. Compiled out of release
    /// builds (see the release arm above).
    #[cfg(test)]
    fn unauthenticated_authz(owner: &Owner) -> AuthzContext {
        AuthzContext::single_owner(owner, AuthPath::System)
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
        // to its Self-Perspective. Subject-authorized calls below resolve
        // the subject personality and become authoritative unless the caller
        // supplied an explicit Self-Perspective.
        let mut author = author;
        let caller_supplied_self = author.caller_self_perspective.is_some();
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

        if let (Some(engine), Some(auth_ctx)) = (self.engine.as_ref(), auth.as_ref())
            && matches!(
                auth_ctx.authz.auth_path,
                proxima_core::AuthPath::HostBearer | proxima_core::AuthPath::MasterDev
            )
        {
            let identity = engine
                .ensure_subject_personality(&auth_ctx.owner, &auth_ctx.authz.identity.principal)
                .await
                .map_err(|err| ToolInvocationError::Tool(McpToolError::Other(err.to_string())))?;
            author.personality_instance_id = Some(identity.instance_id);
            if !caller_supplied_self {
                author.caller_self_perspective = Some(identity.self_perspective_memory_id);
            }
        }

        if let Some(descriptor) = self
            .registry
            .list_mcp_tools()
            .iter()
            .find(|d| tool_name_matches(d.name, name))
        {
            let owner = auth.as_ref().map(|ctx| ctx.owner.clone());
            return (descriptor.call)(self.ctx_for(author, owner, auth.as_ref()), args)
                .await
                .map_err(Into::into);
        }

        Err(ToolInvocationError::ToolNotFound(name.to_string()))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ToolInvocationError {
    #[error("tool not found: {0}")]
    ToolNotFound(String),
    #[error("tool error: {0}")]
    Tool(#[from] McpToolError),
}

impl ToolInvocationError {
    #[must_use]
    pub fn kind(&self) -> McpToolErrorKind {
        match self {
            Self::ToolNotFound(_) => McpToolErrorKind::InvalidInput,
            Self::Tool(inner) => inner.kind(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::auth::McpAuthContext;
    use proxima_core::mcp::McpAuthorContext;
    use proxima_core::{FlavorRegistry, Owner, Principal, UserId};

    fn fake_owner() -> Owner {
        Principal::User(UserId::new(uuid::Uuid::now_v7()))
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
        McpAuthContext::for_master(token, owner)
    }

    #[tokio::test]
    async fn ctx_for_threads_master_token_id_in_prefixed_ids_mode() {
        let server = make_server();
        let author = McpAuthorContext {
            model_id: "test-model".into(),
            client_name: "test-client".into(),
            client_version: "0.1.0".into(),
            personality_instance_id: None,
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
