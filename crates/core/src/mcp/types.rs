use std::sync::Arc;

use crate::authz::AuthzContext;
use crate::{FlavorServices, MemoryId, Owner, verbs::schema::FlavorRegistryFrozen};

#[derive(Debug, Clone)]
pub struct McpAuthorContext {
    pub model_id: String,
    pub client_name: String,
    pub client_version: String,
    pub caller_self_perspective: Option<MemoryId>,
}

#[derive(Clone)]
pub struct McpToolCtx {
    pub owner: Owner,
    /// Caller's authorization context, threaded from the transport
    /// edge. Tools pass this to engine verbs — never a substituted
    /// engine identity (privilege-escalation guard).
    pub authz: AuthzContext,
    pub registry: Arc<FlavorRegistryFrozen>,
    pub author: McpAuthorContext,
    pub caller_self_perspective: Option<MemoryId>,
    /// Backend/flavor services supplied by the host. Core does not name
    /// concrete service types; PG-aware flavors may downcast their own
    /// dependencies here.
    pub services: FlavorServices,
    /// `Some` when the MCP server was constructed with `with_engine`.
    /// Tools that need to call engine verbs (CRUD-via-MCP) require this;
    /// pure read-only / projection tools can ignore it.
    pub engine: Option<Arc<crate::Engine>>,
}

impl std::fmt::Debug for McpToolCtx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpToolCtx")
            .field("owner", &self.owner)
            .field("author", &self.author)
            .finish_non_exhaustive()
    }
}
