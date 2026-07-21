use std::sync::Arc;

use crate::{EdgeId, GoalId, MemoryId};

use super::error::McpToolError;
use super::handles::{
    MemoryHandleClass, PrefixedUuidClass, format_prefixed_uuid, parse_prefixed_uuid,
};
use super::ids::{parse_any_prefixed_memory_uuid, parse_flavor_prefixed_uuid};
use super::types::McpToolCtx;

// format_*/resolve_* stay instance methods even though the canonical
// prefixed form needs no per-session state: the ctx is the tool-facing
// seam for wire references, and method call sites stay stable if
// presentation ever grows state again.
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
    pub fn extension<T>(&self) -> Option<Arc<T>>
    where
        T: Send + Sync + 'static,
    {
        self.extensions.get::<T>()
    }

    #[must_use]
    pub fn format_memory_with_class(&self, id: MemoryId, class: MemoryHandleClass) -> String {
        format_prefixed_uuid(id.into_inner(), class.into())
    }

    #[must_use]
    pub fn format_fact_memory(&self, id: MemoryId) -> String {
        format_prefixed_uuid(id.into_inner(), PrefixedUuidClass::Fact)
    }

    #[must_use]
    pub fn format_abstraction_memory(&self, id: MemoryId) -> String {
        format_prefixed_uuid(id.into_inner(), PrefixedUuidClass::Abstraction)
    }

    #[must_use]
    pub fn format_perspective_memory(&self, id: MemoryId) -> String {
        format_prefixed_uuid(id.into_inner(), PrefixedUuidClass::Perspective)
    }

    #[must_use]
    pub fn format_goal(&self, id: GoalId) -> String {
        format_prefixed_uuid(id.into_inner(), PrefixedUuidClass::Goal)
    }

    #[must_use]
    pub fn format_edge(&self, id: EdgeId) -> String {
        format_prefixed_uuid(id.into_inner(), PrefixedUuidClass::Edge)
    }

    #[must_use]
    pub fn format_flavor_object(&self, _kind: &str, id: uuid::Uuid, prefix: char) -> String {
        format!("{prefix}:{id}")
    }

    /// Parse `raw` as a memory reference (`F:`/`A:`/`P:` prefixed uuid).
    ///
    /// # Errors
    ///
    /// Returns `McpToolError::InvalidInput` when `raw` is not a
    /// well-formed prefixed memory id.
    pub fn resolve_memory(&self, raw: &str) -> Result<MemoryId, McpToolError> {
        parse_any_prefixed_memory_uuid(raw).map(MemoryId::new)
    }

    /// Parse `raw` as a fact-memory reference (`F:<uuid>`).
    ///
    /// # Errors
    ///
    /// See [`McpToolCtx::resolve_memory`].
    pub fn resolve_fact_memory(&self, raw: &str) -> Result<MemoryId, McpToolError> {
        parse_prefixed_uuid(raw, PrefixedUuidClass::Fact)
            .map(MemoryId::new)
            .map_err(|e| McpToolError::InvalidInput(e.to_string()))
    }

    /// Parse `raw` as an abstraction-memory reference (`A:<uuid>`).
    ///
    /// # Errors
    ///
    /// See [`McpToolCtx::resolve_memory`].
    pub fn resolve_abstraction_memory(&self, raw: &str) -> Result<MemoryId, McpToolError> {
        parse_prefixed_uuid(raw, PrefixedUuidClass::Abstraction)
            .map(MemoryId::new)
            .map_err(|e| McpToolError::InvalidInput(e.to_string()))
    }

    /// Parse `raw` as a perspective-memory reference (`P:<uuid>`).
    ///
    /// # Errors
    ///
    /// See [`McpToolCtx::resolve_memory`].
    pub fn resolve_perspective_memory(&self, raw: &str) -> Result<MemoryId, McpToolError> {
        parse_prefixed_uuid(raw, PrefixedUuidClass::Perspective)
            .map(MemoryId::new)
            .map_err(|e| McpToolError::InvalidInput(e.to_string()))
    }

    /// Parse `raw` as a goal reference (`G:<uuid>`).
    ///
    /// # Errors
    ///
    /// See [`McpToolCtx::resolve_memory`].
    pub fn resolve_goal(&self, raw: &str) -> Result<GoalId, McpToolError> {
        parse_prefixed_uuid(raw, PrefixedUuidClass::Goal)
            .map(GoalId::new)
            .map_err(|e| McpToolError::InvalidInput(e.to_string()))
    }

    /// Parse `raw` as an edge reference (`E:<uuid>`).
    ///
    /// # Errors
    ///
    /// See [`McpToolCtx::resolve_memory`].
    pub fn resolve_edge(&self, raw: &str) -> Result<EdgeId, McpToolError> {
        parse_prefixed_uuid(raw, PrefixedUuidClass::Edge)
            .map(EdgeId::new)
            .map_err(|e| McpToolError::InvalidInput(e.to_string()))
    }

    /// Parse `raw` as a flavor-object reference of the given `kind`
    /// (`<prefix>:<uuid>` with a flavor-registered prefix).
    ///
    /// # Errors
    ///
    /// See [`McpToolCtx::resolve_memory`].
    pub fn resolve_flavor_object(
        &self,
        raw: &str,
        _kind: &str,
    ) -> Result<uuid::Uuid, McpToolError> {
        parse_flavor_prefixed_uuid(raw)
    }
}
