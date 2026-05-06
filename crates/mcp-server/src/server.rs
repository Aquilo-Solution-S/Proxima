use std::sync::Arc;

use proxima_core::mcp::{HandleTable, McpAuthorContext, McpToolCtx, McpToolError};
use proxima_core::{FlavorRegistry, FlavorRegistryFrozen, Owner};

#[derive(Clone, Debug)]
pub struct DevMcpServer {
    pool: sqlx::PgPool,
    owner: Owner,
    handles: Arc<HandleTable>,
    registry: Arc<FlavorRegistryFrozen>,
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
        }
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
    pub fn ctx(&self, author: McpAuthorContext) -> McpToolCtx {
        McpToolCtx {
            pool: self.pool.clone(),
            owner: self.owner.clone(),
            handles: self.handles.clone(),
            registry: self.registry.clone(),
            author,
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
    ) -> Result<serde_json::Value, ToolInvocationError> {
        let descriptor = self
            .registry
            .list_mcp_tools()
            .iter()
            .find(|d| d.name == name)
            .ok_or_else(|| ToolInvocationError::ToolNotFound(name.to_string()))?;
        (descriptor.call)(self.ctx(author), args)
            .await
            .map_err(Into::into)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ToolInvocationError {
    #[error("tool not found: {0}")]
    ToolNotFound(String),
    #[error("tool error: {0}")]
    Tool(#[from] McpToolError),
}
