use crate::{EdgeId, GoalId, MemoryId, ToolError};

use super::handles::{
    MemoryHandleClass, PrefixedUuidClass, format_prefixed_uuid, parse_prefixed_uuid,
};
use super::ids::parse_flavor_prefixed_uuid;
use super::types::McpToolCtx;

/// MCP adapter presentation service made available to generic flavor [`crate::Tool`]
/// implementations through [`crate::ToolCtx::service`].
///
/// `ToolCtx` stays transport-neutral and opaque; the MCP wire-reference
/// grammar (typed `F:`/`A:`/`P:`/`G:`/`E:` prefixed uuids) remains in this
/// module and is only injected by the MCP adapter.
#[derive(Debug, Clone, Default)]
pub struct McpToolPresentation;

// format_*/resolve_* stay instance methods even though the canonical
// prefixed form needs no per-session state: the service is the flavor-facing
// seam for wire references, and method call sites stay stable if
// presentation ever grows state again.
#[allow(clippy::unused_self)]
impl McpToolPresentation {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    #[must_use]
    pub fn from_ctx(_ctx: &McpToolCtx) -> Self {
        Self
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

    /// # Errors
    ///
    /// Returns invalid-input errors when `raw` is not a well-formed
    /// `F:<uuid>` reference.
    pub fn resolve_fact_memory(&self, raw: &str) -> Result<MemoryId, ToolError> {
        parse_prefixed_uuid(raw, PrefixedUuidClass::Fact)
            .map(MemoryId::new)
            .map_err(|e| ToolError::InvalidInput(e.to_string()))
    }

    /// # Errors
    ///
    /// Returns invalid-input errors when `raw` is not a well-formed
    /// `A:<uuid>` reference.
    pub fn resolve_abstraction_memory(&self, raw: &str) -> Result<MemoryId, ToolError> {
        parse_prefixed_uuid(raw, PrefixedUuidClass::Abstraction)
            .map(MemoryId::new)
            .map_err(|e| ToolError::InvalidInput(e.to_string()))
    }

    /// # Errors
    ///
    /// Returns invalid-input errors when `raw` is not a well-formed
    /// `P:<uuid>` reference.
    pub fn resolve_perspective_memory(&self, raw: &str) -> Result<MemoryId, ToolError> {
        parse_prefixed_uuid(raw, PrefixedUuidClass::Perspective)
            .map(MemoryId::new)
            .map_err(|e| ToolError::InvalidInput(e.to_string()))
    }

    /// # Errors
    ///
    /// Returns invalid-input errors when `raw` is not a well-formed
    /// flavor-object reference (`<prefix>:<uuid>`).
    pub fn resolve_flavor_object(&self, raw: &str, _kind: &str) -> Result<uuid::Uuid, ToolError> {
        parse_flavor_prefixed_uuid(raw).map_err(|e| ToolError::InvalidInput(e.to_string()))
    }
}

/// MCP adapter caller metadata service for generic flavor tools.
#[derive(Debug, Clone)]
pub struct McpToolCaller {
    model_id: String,
}

impl McpToolCaller {
    #[must_use]
    pub fn new(model_id: String) -> Self {
        Self { model_id }
    }

    #[must_use]
    pub fn from_ctx(ctx: &McpToolCtx) -> Self {
        Self {
            model_id: ctx.author.model_id.clone(),
        }
    }

    #[must_use]
    pub fn model_id(&self) -> &str {
        &self.model_id
    }
}
