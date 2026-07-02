//! Transport-neutral flavor tool SDK.

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::Arc;

use futures::future::BoxFuture;

use crate::access::AccessKind;
use crate::storage_ports::OwnerWritePermit;
use crate::{AuthzContext, Engine, FlavorRegistryFrozen, MemoryId, Owner};

#[derive(Clone, Default)]
pub struct ToolServices {
    values: Arc<HashMap<TypeId, Arc<dyn Any + Send + Sync>>>,
}

impl std::fmt::Debug for ToolServices {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolServices")
            .field("len", &self.values.len())
            .finish_non_exhaustive()
    }
}

impl ToolServices {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with<T>(value: T) -> Self
    where
        T: Send + Sync + 'static,
    {
        let mut services = Self::default();
        services.insert(value);
        services
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
    pub(crate) fn from_values(values: Arc<HashMap<TypeId, Arc<dyn Any + Send + Sync>>>) -> Self {
        Self { values }
    }
}

#[derive(Clone)]
pub struct ToolCtx {
    owner: Owner,
    authz: AuthzContext,
    registry: Arc<FlavorRegistryFrozen>,
    caller_self_perspective: Option<MemoryId>,
    services: ToolServices,
    engine: Option<Arc<Engine>>,
}

impl std::fmt::Debug for ToolCtx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolCtx")
            .field("owner", &self.owner)
            .field("caller_self_perspective", &self.caller_self_perspective)
            .field("services", &self.services)
            .field("has_engine", &self.engine.is_some())
            .finish_non_exhaustive()
    }
}

impl ToolCtx {
    #[must_use]
    pub fn new(
        owner: Owner,
        authz: AuthzContext,
        registry: Arc<FlavorRegistryFrozen>,
        services: ToolServices,
    ) -> Self {
        Self {
            owner,
            authz,
            registry,
            caller_self_perspective: None,
            services,
            engine: None,
        }
    }

    #[must_use]
    pub fn with_caller_self_perspective(mut self, memory_id: Option<MemoryId>) -> Self {
        self.caller_self_perspective = memory_id;
        self
    }

    #[must_use]
    pub fn with_engine(mut self, engine: Option<Arc<Engine>>) -> Self {
        self.engine = engine;
        self
    }

    #[must_use]
    pub(crate) fn from_parts(
        owner: Owner,
        authz: AuthzContext,
        registry: Arc<FlavorRegistryFrozen>,
        caller_self_perspective: Option<MemoryId>,
        services: ToolServices,
        engine: Option<Arc<Engine>>,
    ) -> Self {
        Self {
            owner,
            authz,
            registry,
            caller_self_perspective,
            services,
            engine,
        }
    }

    #[must_use]
    pub fn owner(&self) -> Owner {
        self.owner
    }

    #[must_use]
    pub fn authz(&self) -> &AuthzContext {
        &self.authz
    }

    #[must_use]
    pub fn registry(&self) -> &FlavorRegistryFrozen {
        &self.registry
    }

    #[must_use]
    pub fn caller_self_perspective(&self) -> Option<MemoryId> {
        self.caller_self_perspective
    }

    #[must_use]
    pub fn engine(&self) -> Option<Arc<Engine>> {
        self.engine.clone()
    }

    /// Authorize this tool context for a storage-tier owner write.
    ///
    /// The permit is minted by the engine from this context's real transport
    /// authorization and scoped owner; flavor code cannot construct it.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::Other`] when the tool was not wired with an engine
    /// and [`ToolError::Protocol`] when authorization fails.
    pub async fn owner_write_permit(
        &self,
        kind: AccessKind,
    ) -> Result<OwnerWritePermit, ToolError> {
        let engine = self
            .engine
            .as_ref()
            .ok_or_else(|| ToolError::Other("tool context has no engine".into()))?;
        engine
            .authorize_owner_write(&self.authz, &self.owner, kind)
            .await
            .map_err(ToolError::Protocol)
    }

    #[must_use]
    pub fn service<T: Send + Sync + 'static>(&self) -> Option<Arc<T>> {
        self.services.get::<T>()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("tool not authorized: {0}")]
    NotAuthorized(String),
    #[error("{0}")]
    Protocol(#[from] crate::error::ProtocolError),
    #[error("layering violation: {0}")]
    LayeringViolation(String),
    #[error("storage: {0}")]
    Storage(#[from] crate::StorageError),
    #[error("{0}")]
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolOrigin {
    Substrate,
    Flavor(String),
}

#[derive(Debug, Clone)]
pub struct ToolDescriptor {
    pub name: &'static str,
    pub description: &'static str,
    pub origin: ToolOrigin,
    pub produces_schema_ids: &'static [&'static str],
    pub args_schema: serde_json::Value,
    pub call: ToolCallFn,
}

#[derive(Debug)]
pub struct ToolCall {
    pub name: String,
    pub args: serde_json::Value,
    pub ctx: ToolCtx,
}

pub type ToolCallFn =
    fn(ToolCtx, serde_json::Value) -> BoxFuture<'static, Result<serde_json::Value, ToolError>>;

pub trait Tool: Send + Sync + 'static {
    const NAME: &'static str;
    const DESCRIPTION: &'static str;
    const PRODUCES_SCHEMA_IDS: &'static [&'static str] = &[];

    type Args: serde::de::DeserializeOwned + schemars::JsonSchema + Send + 'static;
    type Output: serde::Serialize + Send + 'static;

    fn call(ctx: ToolCtx, args: Self::Args) -> BoxFuture<'static, Result<Self::Output, ToolError>>;
}
