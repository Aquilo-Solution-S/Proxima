use std::sync::Arc;

use crate::{EdgeId, GoalId, MemoryId, ToolError};

use super::handles::{
    HandleTable, MemoryHandleClass, PrefixedUuidClass, format_prefixed_uuid, parse_prefixed_uuid,
};
use super::ids::parse_flavor_prefixed_uuid;
use super::types::{McpToolCtx, OutputMode};

/// MCP adapter presentation service made available to generic flavor [`crate::Tool`]
/// implementations through [`crate::ToolCtx::service`].
///
/// `ToolCtx` stays transport-neutral and opaque; MCP-specific handle projection
/// remains in this module and is only injected by the MCP adapter.
#[derive(Debug, Clone)]
pub struct McpToolPresentation {
    handles: Option<Arc<HandleTable>>,
    mode: OutputMode,
}

impl McpToolPresentation {
    #[must_use]
    pub fn new(handles: Option<Arc<HandleTable>>, mode: OutputMode) -> Self {
        Self { handles, mode }
    }

    #[must_use]
    pub fn from_ctx(ctx: &McpToolCtx) -> Self {
        Self {
            handles: ctx.handles.clone(),
            mode: ctx.mode,
        }
    }

    fn handle_table(&self) -> Result<&HandleTable, ToolError> {
        self.handles.as_deref().ok_or_else(|| {
            ToolError::Other("OutputMode::Handles requires a HandleTable".to_string())
        })
    }

    fn handle_table_for_format(&self) -> &HandleTable {
        self.handles
            .as_deref()
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
                .handle_table_for_format()
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
                .handle_table_for_format()
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
                .handle_table_for_format()
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
            OutputMode::Handles => self
                .handle_table_for_format()
                .assign_goal(id)
                .as_str()
                .to_string(),
            OutputMode::RawIds => id.into_inner().to_string(),
            OutputMode::PrefixedIds => {
                format_prefixed_uuid(id.into_inner(), PrefixedUuidClass::Goal)
            }
        }
    }

    #[must_use]
    pub fn format_edge(&self, id: EdgeId) -> String {
        match self.mode {
            OutputMode::Handles => self
                .handle_table_for_format()
                .assign_edge(id)
                .as_str()
                .to_string(),
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
                .handle_table_for_format()
                .assign_flavor_object(kind, id, prefix)
                .as_str()
                .to_string(),
            OutputMode::RawIds => id.to_string(),
            OutputMode::PrefixedIds => format!("{prefix}:{id}"),
        }
    }

    /// # Errors
    ///
    /// Returns invalid-input errors when `raw` cannot be resolved in the
    /// active MCP output mode.
    pub fn resolve_fact_memory(&self, raw: &str) -> Result<MemoryId, ToolError> {
        match self.mode {
            OutputMode::Handles => self
                .handle_table()?
                .resolve_fact_memory(raw)
                .map_err(|e| ToolError::InvalidInput(e.to_string())),
            OutputMode::RawIds => raw
                .parse::<uuid::Uuid>()
                .map(MemoryId::new)
                .map_err(|e| ToolError::InvalidInput(format!("not a uuid: {e}"))),
            OutputMode::PrefixedIds => parse_prefixed_uuid(raw, PrefixedUuidClass::Fact)
                .map(MemoryId::new)
                .map_err(|e| ToolError::InvalidInput(e.to_string())),
        }
    }

    /// # Errors
    ///
    /// Returns invalid-input errors when `raw` cannot be resolved in the
    /// active MCP output mode.
    pub fn resolve_abstraction_memory(&self, raw: &str) -> Result<MemoryId, ToolError> {
        match self.mode {
            OutputMode::Handles => self
                .handle_table()?
                .resolve_abstraction_memory(raw)
                .map_err(|e| ToolError::InvalidInput(e.to_string())),
            OutputMode::RawIds => raw
                .parse::<uuid::Uuid>()
                .map(MemoryId::new)
                .map_err(|e| ToolError::InvalidInput(format!("not a uuid: {e}"))),
            OutputMode::PrefixedIds => parse_prefixed_uuid(raw, PrefixedUuidClass::Abstraction)
                .map(MemoryId::new)
                .map_err(|e| ToolError::InvalidInput(e.to_string())),
        }
    }

    /// # Errors
    ///
    /// Returns invalid-input errors when `raw` cannot be resolved in the
    /// active MCP output mode.
    pub fn resolve_perspective_memory(&self, raw: &str) -> Result<MemoryId, ToolError> {
        match self.mode {
            OutputMode::Handles => self
                .handle_table()?
                .resolve_perspective_memory(raw)
                .map_err(|e| ToolError::InvalidInput(e.to_string())),
            OutputMode::RawIds => raw
                .parse::<uuid::Uuid>()
                .map(MemoryId::new)
                .map_err(|e| ToolError::InvalidInput(format!("not a uuid: {e}"))),
            OutputMode::PrefixedIds => parse_prefixed_uuid(raw, PrefixedUuidClass::Perspective)
                .map(MemoryId::new)
                .map_err(|e| ToolError::InvalidInput(e.to_string())),
        }
    }

    /// # Errors
    ///
    /// Returns invalid-input errors when `raw` cannot be resolved in the
    /// active MCP output mode.
    pub fn resolve_flavor_object(&self, raw: &str, kind: &str) -> Result<uuid::Uuid, ToolError> {
        match self.mode {
            OutputMode::Handles => self
                .handle_table()?
                .resolve_flavor_object(raw, kind)
                .map_err(|e| ToolError::InvalidInput(e.to_string())),
            OutputMode::RawIds => raw
                .parse::<uuid::Uuid>()
                .map_err(|e| ToolError::InvalidInput(format!("not a uuid: {e}"))),
            OutputMode::PrefixedIds => {
                parse_flavor_prefixed_uuid(raw).map_err(|e| ToolError::InvalidInput(e.to_string()))
            }
        }
    }
}

/// MCP adapter caller metadata service for generic flavor tools.
#[derive(Debug, Clone)]
pub struct McpToolCaller {
    model_id: String,
    is_master_token: bool,
}

impl McpToolCaller {
    #[must_use]
    pub fn new(model_id: String, is_master_token: bool) -> Self {
        Self {
            model_id,
            is_master_token,
        }
    }

    #[must_use]
    pub fn from_ctx(ctx: &McpToolCtx) -> Self {
        Self {
            model_id: ctx.author.model_id.clone(),
            is_master_token: ctx.master_token_id.is_some(),
        }
    }

    #[must_use]
    pub fn model_id(&self) -> &str {
        &self.model_id
    }

    #[must_use]
    pub const fn is_master_token(&self) -> bool {
        self.is_master_token
    }
}
