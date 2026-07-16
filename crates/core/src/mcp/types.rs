use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::Arc;

use crate::authz::AuthzContext;
use crate::{MemoryId, Owner, ToolServices, verbs::schema::FlavorRegistryFrozen};

#[derive(Debug, Clone)]
pub struct McpAuthorContext {
    pub model_id: String,
    pub client_name: String,
    pub client_version: String,
    pub caller_self_perspective: Option<MemoryId>,
}

#[derive(Clone, Default)]
pub struct McpToolExtensions {
    values: Arc<HashMap<TypeId, Arc<dyn Any + Send + Sync>>>,
}

impl std::fmt::Debug for McpToolExtensions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpToolExtensions")
            .field("len", &self.values.len())
            .finish_non_exhaustive()
    }
}

impl McpToolExtensions {
    #[must_use]
    pub fn with<T>(value: T) -> Self
    where
        T: Send + Sync + 'static,
    {
        let mut extensions = Self::default();
        extensions.insert(value);
        extensions
    }

    pub fn insert<T>(&mut self, value: T) -> Option<Arc<T>>
    where
        T: Send + Sync + 'static,
    {
        Arc::make_mut(&mut self.values)
            .insert(TypeId::of::<T>(), Arc::new(value))
            .and_then(|old| old.downcast::<T>().ok())
    }

    #[must_use]
    pub fn get<T>(&self) -> Option<Arc<T>>
    where
        T: Send + Sync + 'static,
    {
        self.values
            .get(&TypeId::of::<T>())
            .cloned()
            .and_then(|value| value.downcast::<T>().ok())
    }

    #[must_use]
    pub(crate) fn into_tool_services(self) -> ToolServices {
        ToolServices::from_values(self.values)
    }
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
    pub extensions: McpToolExtensions,
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
