use std::sync::Arc;

use crate::{GoalId, MemoryId};

use super::error::McpToolError;
use super::handles::MemoryHandleClass;
use super::types::McpToolCtx;

#[allow(clippy::unused_self)]
impl McpToolCtx {
    /// `None` when the MCP server is running without a wired engine
    /// (early test scaffolds). Real deployments always wire an engine.
    #[must_use]
    pub fn engine(&self) -> Option<&crate::Engine> {
        self.engine.as_deref()
    }

    /// The wired engine, or the one canonical "engine unavailable" error.
    /// Every tool that needs storage goes through this so a missing
    /// engine reads identically everywhere.
    ///
    /// # Errors
    ///
    /// Returns [`McpToolError::Other`] when no engine is wired.
    pub fn require_engine(&self) -> Result<&crate::Engine, McpToolError> {
        self.engine()
            .ok_or_else(|| McpToolError::Other("engine unavailable".into()))
    }

    #[must_use]
    pub fn service<T>(&self) -> Option<Arc<T>>
    where
        T: Send + Sync + 'static,
    {
        self.services.get::<T>()
    }

    /// Parse a bare memory UUID. Kind and visibility are checked by the engine.
    ///
    /// # Errors
    ///
    /// Returns invalid input for any other spelling.
    pub fn resolve_memory(&self, raw: &str) -> Result<MemoryId, McpToolError> {
        super::ids::parse_wire_uuid(raw).map(MemoryId::new)
    }

    /// Parse a bare Goal UUID; the Goal operation resolves its stored row.
    ///
    /// # Errors
    ///
    /// Returns invalid input for any other spelling.
    pub fn resolve_goal(&self, raw: &str) -> Result<GoalId, McpToolError> {
        super::ids::parse_wire_uuid(raw).map(GoalId::new)
    }

    /// Resolve the layer of a readable stored memory, without trusting the caller.
    ///
    /// # Errors
    ///
    /// Returns invalid input, not found, or an engine read failure.
    pub async fn resolve_memory_with_kind(
        &self,
        raw: &str,
    ) -> Result<(MemoryId, MemoryHandleClass), McpToolError> {
        let memory_id = self.resolve_memory(raw)?;
        self.resolve_memory_id_with_kind(memory_id).await
    }

    pub(crate) async fn resolve_memory_id_with_kind(
        &self,
        memory_id: MemoryId,
    ) -> Result<(MemoryId, MemoryHandleClass), McpToolError> {
        self.resolve_memory_ids_with_kind(&[memory_id])
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| {
                McpToolError::NotFound(format!("memory {} not found", memory_id.into_inner()))
            })
    }

    /// [`Self::resolve_memory_id_with_kind`] for each of `ids`, in order, with
    /// one authorization lookup for all of them; the first unreadable id is
    /// the error.
    pub(crate) async fn resolve_memory_ids_with_kind(
        &self,
        ids: &[MemoryId],
    ) -> Result<Vec<(MemoryId, MemoryHandleClass)>, McpToolError> {
        self.require_engine()?
            .memory_kinds(&self.authz, ids)
            .await?
            .into_iter()
            .map(|(id, kind)| {
                MemoryHandleClass::from_entity_kind(kind)
                    .map(|class| (id, class))
                    .ok_or_else(|| McpToolError::InvalidInput("expected a memory".into()))
            })
            .collect()
    }
}
