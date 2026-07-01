use std::sync::Arc;

use crate::{EdgeId, GoalId, MemoryId};

use super::error::McpToolError;
use super::handles::{
    HandleTable, MemoryHandleClass, PrefixedUuidClass, format_prefixed_uuid, parse_prefixed_uuid,
};
use super::ids::{parse_any_prefixed_memory_uuid, parse_flavor_prefixed_uuid};
use super::types::{McpToolCtx, OutputMode};

impl McpToolCtx {
    /// `None` when the MCP server is running without a wired engine
    /// (early test scaffolds). Real deployments always wire an engine.
    #[must_use]
    pub fn engine(&self) -> Option<&crate::Engine> {
        self.engine.as_deref()
    }

    #[must_use]
    pub fn extension<T>(&self) -> Option<Arc<T>>
    where
        T: Send + Sync + 'static,
    {
        self.extensions.get::<T>()
    }

    fn handle_table(&self) -> &HandleTable {
        self.handles
            .as_ref()
            .expect("OutputMode::Handles requires a HandleTable")
    }

    #[must_use]
    pub fn format_memory_with_class(&self, id: MemoryId, class: MemoryHandleClass) -> String {
        match class {
            MemoryHandleClass::Fact => self.format_fact_memory(id),
            MemoryHandleClass::Abstraction => self.format_abstraction_memory(id),
            MemoryHandleClass::Perspective => self.format_perspective_memory(id),
        }
    }

    #[must_use]
    pub fn format_fact_memory(&self, id: MemoryId) -> String {
        match self.mode {
            OutputMode::Handles => self
                .handle_table()
                .assign_fact_memory(id)
                .as_str()
                .to_string(),
            OutputMode::RawIds => id.into_inner().to_string(),
            OutputMode::PrefixedIds => {
                format_prefixed_uuid(id.into_inner(), PrefixedUuidClass::Fact)
            }
        }
    }

    #[must_use]
    pub fn format_abstraction_memory(&self, id: MemoryId) -> String {
        match self.mode {
            OutputMode::Handles => self
                .handle_table()
                .assign_abstraction_memory(id)
                .as_str()
                .to_string(),
            OutputMode::RawIds => id.into_inner().to_string(),
            OutputMode::PrefixedIds => {
                format_prefixed_uuid(id.into_inner(), PrefixedUuidClass::Abstraction)
            }
        }
    }

    #[must_use]
    pub fn format_perspective_memory(&self, id: MemoryId) -> String {
        match self.mode {
            OutputMode::Handles => self
                .handle_table()
                .assign_perspective_memory(id)
                .as_str()
                .to_string(),
            OutputMode::RawIds => id.into_inner().to_string(),
            OutputMode::PrefixedIds => {
                format_prefixed_uuid(id.into_inner(), PrefixedUuidClass::Perspective)
            }
        }
    }

    #[must_use]
    pub fn format_goal(&self, id: GoalId) -> String {
        match self.mode {
            OutputMode::Handles => self.handle_table().assign_goal(id).as_str().to_string(),
            OutputMode::RawIds => id.into_inner().to_string(),
            OutputMode::PrefixedIds => {
                format_prefixed_uuid(id.into_inner(), PrefixedUuidClass::Goal)
            }
        }
    }

    #[must_use]
    pub fn format_edge(&self, id: EdgeId) -> String {
        match self.mode {
            OutputMode::Handles => self.handle_table().assign_edge(id).as_str().to_string(),
            OutputMode::RawIds => id.into_inner().to_string(),
            OutputMode::PrefixedIds => {
                format_prefixed_uuid(id.into_inner(), PrefixedUuidClass::Edge)
            }
        }
    }

    #[must_use]
    pub fn format_flavor_object(&self, kind: &str, id: uuid::Uuid, prefix: char) -> String {
        match self.mode {
            OutputMode::Handles => self
                .handle_table()
                .assign_flavor_object(kind, id, prefix)
                .as_str()
                .to_string(),
            OutputMode::RawIds => id.to_string(),
            OutputMode::PrefixedIds => format!("{prefix}:{id}"),
        }
    }

    /// Parse `raw` as a memory reference under the active mode.
    ///
    /// # Errors
    ///
    /// Returns `McpToolError::Resolve` in `Handles` mode if the handle
    /// is unknown or names the wrong kind, and `McpToolError::InvalidInput`
    /// in `RawIds` mode if `raw` is not a well-formed UUID.
    pub fn resolve_memory(&self, raw: &str) -> Result<MemoryId, McpToolError> {
        match self.mode {
            OutputMode::Handles => self
                .handle_table()
                .resolve_memory(raw)
                .map_err(McpToolError::Resolve),
            OutputMode::RawIds => raw
                .parse::<uuid::Uuid>()
                .map(MemoryId::new)
                .map_err(|e| McpToolError::InvalidInput(format!("not a uuid: {e}"))),
            OutputMode::PrefixedIds => parse_any_prefixed_memory_uuid(raw).map(MemoryId::new),
        }
    }

    /// Parse `raw` as a fact-memory reference under the active mode.
    ///
    /// # Errors
    ///
    /// See [`McpToolCtx::resolve_memory`].
    pub fn resolve_fact_memory(&self, raw: &str) -> Result<MemoryId, McpToolError> {
        match self.mode {
            OutputMode::Handles => self
                .handle_table()
                .resolve_fact_memory(raw)
                .map_err(McpToolError::Resolve),
            OutputMode::RawIds => raw
                .parse::<uuid::Uuid>()
                .map(MemoryId::new)
                .map_err(|e| McpToolError::InvalidInput(format!("not a uuid: {e}"))),
            OutputMode::PrefixedIds => parse_prefixed_uuid(raw, PrefixedUuidClass::Fact)
                .map(MemoryId::new)
                .map_err(|e| McpToolError::InvalidInput(e.to_string())),
        }
    }

    /// Parse `raw` as an abstraction-memory reference under the active mode.
    ///
    /// # Errors
    ///
    /// See [`McpToolCtx::resolve_memory`].
    pub fn resolve_abstraction_memory(&self, raw: &str) -> Result<MemoryId, McpToolError> {
        match self.mode {
            OutputMode::Handles => self
                .handle_table()
                .resolve_abstraction_memory(raw)
                .map_err(McpToolError::Resolve),
            OutputMode::RawIds => raw
                .parse::<uuid::Uuid>()
                .map(MemoryId::new)
                .map_err(|e| McpToolError::InvalidInput(format!("not a uuid: {e}"))),
            OutputMode::PrefixedIds => parse_prefixed_uuid(raw, PrefixedUuidClass::Abstraction)
                .map(MemoryId::new)
                .map_err(|e| McpToolError::InvalidInput(e.to_string())),
        }
    }

    /// Parse `raw` as a perspective-memory reference under the active mode.
    ///
    /// # Errors
    ///
    /// See [`McpToolCtx::resolve_memory`].
    pub fn resolve_perspective_memory(&self, raw: &str) -> Result<MemoryId, McpToolError> {
        match self.mode {
            OutputMode::Handles => self
                .handle_table()
                .resolve_perspective_memory(raw)
                .map_err(McpToolError::Resolve),
            OutputMode::RawIds => raw
                .parse::<uuid::Uuid>()
                .map(MemoryId::new)
                .map_err(|e| McpToolError::InvalidInput(format!("not a uuid: {e}"))),
            OutputMode::PrefixedIds => parse_prefixed_uuid(raw, PrefixedUuidClass::Perspective)
                .map(MemoryId::new)
                .map_err(|e| McpToolError::InvalidInput(e.to_string())),
        }
    }

    /// Parse `raw` as a goal reference under the active mode.
    ///
    /// # Errors
    ///
    /// See [`McpToolCtx::resolve_memory`].
    pub fn resolve_goal(&self, raw: &str) -> Result<GoalId, McpToolError> {
        match self.mode {
            OutputMode::Handles => self
                .handle_table()
                .resolve_goal(raw)
                .map_err(McpToolError::Resolve),
            OutputMode::RawIds => raw
                .parse::<uuid::Uuid>()
                .map(GoalId::new)
                .map_err(|e| McpToolError::InvalidInput(format!("not a uuid: {e}"))),
            OutputMode::PrefixedIds => parse_prefixed_uuid(raw, PrefixedUuidClass::Goal)
                .map(GoalId::new)
                .map_err(|e| McpToolError::InvalidInput(e.to_string())),
        }
    }

    /// Parse `raw` as an edge reference under the active mode.
    ///
    /// # Errors
    ///
    /// See [`McpToolCtx::resolve_memory`].
    pub fn resolve_edge(&self, raw: &str) -> Result<EdgeId, McpToolError> {
        match self.mode {
            OutputMode::Handles => self
                .handle_table()
                .resolve_edge(raw)
                .map_err(McpToolError::Resolve),
            OutputMode::RawIds => raw
                .parse::<uuid::Uuid>()
                .map(EdgeId::new)
                .map_err(|e| McpToolError::InvalidInput(format!("not a uuid: {e}"))),
            OutputMode::PrefixedIds => parse_prefixed_uuid(raw, PrefixedUuidClass::Edge)
                .map(EdgeId::new)
                .map_err(|e| McpToolError::InvalidInput(e.to_string())),
        }
    }

    /// Parse `raw` as a flavor-object reference of the given `kind`
    /// under the active mode.
    ///
    /// # Errors
    ///
    /// See [`McpToolCtx::resolve_memory`].
    pub fn resolve_flavor_object(&self, raw: &str, kind: &str) -> Result<uuid::Uuid, McpToolError> {
        match self.mode {
            OutputMode::Handles => self
                .handle_table()
                .resolve_flavor_object(raw, kind)
                .map_err(McpToolError::Resolve),
            OutputMode::RawIds => raw
                .parse::<uuid::Uuid>()
                .map_err(|e| McpToolError::InvalidInput(format!("not a uuid: {e}"))),
            OutputMode::PrefixedIds => parse_flavor_prefixed_uuid(raw),
        }
    }
}
