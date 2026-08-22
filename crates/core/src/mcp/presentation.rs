use std::sync::Arc;

use crate::{GoalId, MemoryId, ToolCtx, ToolError};

use super::handles::{
    MemoryHandleClass, PrefixedUuidClass, format_prefixed_uuid, parse_prefixed_uuid,
};
use super::ids::parse_flavor_prefixed_uuid;
use super::types::McpToolCtx;

/// MCP adapter presentation service made available to generic flavor [`crate::Tool`]
/// implementations through [`crate::ToolCtx::service`].
///
/// `ToolCtx` stays transport-neutral and opaque; the MCP wire-reference
/// grammar (typed `F:`/`A:`/`P:`/`G:` prefixed uuids) remains in this
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

/// Reach the wire-reference grammar from a generic [`ToolCtx`].
///
/// [`McpToolCtx`] carries `format_*`/`resolve_*` as inherent methods, but a
/// flavor implementing [`crate::Tool`] is handed a transport-neutral
/// [`ToolCtx`] and has to go through [`ToolCtx::service`] instead. Every
/// flavor that mints handles would otherwise write the same forwarding
/// shim, once per tool. This is that shim, once, in the crate that owns
/// the grammar.
///
/// `ToolCtx` itself stays transport-neutral: import this trait and the
/// methods appear, don't and they don't exist.
///
/// The `format_*` methods panic when the presentation service is absent,
/// which is the contract they inherit: [`crate::mcp::McpTool`]'s
/// blanket impl over [`crate::Tool`] always installs the service, so absence
/// means the tool was invoked outside the MCP adapter and has no business
/// minting MCP wire references. `resolve_*` reports the same condition as
/// an error because it has a channel for one.
pub trait McpPresentationExt {
    /// The MCP presentation service, or the one canonical error naming its
    /// absence.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::Other`] when the call did not arrive through the
    /// MCP adapter.
    fn mcp_presentation(&self) -> Result<Arc<McpToolPresentation>, ToolError>;

    fn format_fact_memory(&self, id: MemoryId) -> String;
    fn format_abstraction_memory(&self, id: MemoryId) -> String;
    fn format_perspective_memory(&self, id: MemoryId) -> String;
    fn format_goal(&self, id: GoalId) -> String;
    fn format_flavor_object(&self, kind: &str, id: uuid::Uuid, prefix: char) -> String;

    /// # Errors
    ///
    /// Returns invalid-input errors when `raw` is not a well-formed `F:<uuid>`
    /// reference, or [`ToolError::Other`] when the presentation service is absent.
    fn resolve_fact_memory(&self, raw: &str) -> Result<MemoryId, ToolError>;

    /// # Errors
    ///
    /// Returns invalid-input errors when `raw` is not a well-formed `A:<uuid>`
    /// reference, or [`ToolError::Other`] when the presentation service is absent.
    fn resolve_abstraction_memory(&self, raw: &str) -> Result<MemoryId, ToolError>;

    /// # Errors
    ///
    /// Returns invalid-input errors when `raw` is not a well-formed `P:<uuid>`
    /// reference, or [`ToolError::Other`] when the presentation service is absent.
    fn resolve_perspective_memory(&self, raw: &str) -> Result<MemoryId, ToolError>;

    /// # Errors
    ///
    /// Returns invalid-input errors when `raw` is not a well-formed
    /// `<prefix>:<uuid>` reference, or [`ToolError::Other`] when the
    /// presentation service is absent.
    fn resolve_flavor_object(&self, raw: &str, kind: &str) -> Result<uuid::Uuid, ToolError>;
}

/// The message a flavor sees when it mints a wire reference outside MCP.
const NO_PRESENTATION: &str = "MCP tools require the presentation service";

impl McpPresentationExt for ToolCtx {
    fn mcp_presentation(&self) -> Result<Arc<McpToolPresentation>, ToolError> {
        self.service::<McpToolPresentation>()
            .ok_or_else(|| ToolError::Other(NO_PRESENTATION.into()))
    }

    fn format_fact_memory(&self, id: MemoryId) -> String {
        self.mcp_presentation()
            .expect(NO_PRESENTATION)
            .format_fact_memory(id)
    }

    fn format_abstraction_memory(&self, id: MemoryId) -> String {
        self.mcp_presentation()
            .expect(NO_PRESENTATION)
            .format_abstraction_memory(id)
    }

    fn format_perspective_memory(&self, id: MemoryId) -> String {
        self.mcp_presentation()
            .expect(NO_PRESENTATION)
            .format_perspective_memory(id)
    }

    fn format_goal(&self, id: GoalId) -> String {
        self.mcp_presentation()
            .expect(NO_PRESENTATION)
            .format_goal(id)
    }

    fn format_flavor_object(&self, kind: &str, id: uuid::Uuid, prefix: char) -> String {
        self.mcp_presentation()
            .expect(NO_PRESENTATION)
            .format_flavor_object(kind, id, prefix)
    }

    fn resolve_fact_memory(&self, raw: &str) -> Result<MemoryId, ToolError> {
        self.mcp_presentation()?.resolve_fact_memory(raw)
    }

    fn resolve_abstraction_memory(&self, raw: &str) -> Result<MemoryId, ToolError> {
        self.mcp_presentation()?.resolve_abstraction_memory(raw)
    }

    fn resolve_perspective_memory(&self, raw: &str) -> Result<MemoryId, ToolError> {
        self.mcp_presentation()?.resolve_perspective_memory(raw)
    }

    fn resolve_flavor_object(&self, raw: &str, kind: &str) -> Result<uuid::Uuid, ToolError> {
        self.mcp_presentation()?.resolve_flavor_object(raw, kind)
    }
}
