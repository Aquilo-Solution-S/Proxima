//! Engine composite — wires `FlavorRegistryFrozen` behind the typed
//! verb surfaces of docs/14-protocol-surface.md.

#[allow(dead_code)]
mod access_admin;
mod access_sets;
mod builder;
mod compliance;
mod errors;
mod fact_retention;
mod goal_write;
mod ingest;
pub mod mcp_listener;
mod memory_authoring;
mod pin_read;
mod pipeline;
mod query;
mod read_verbs;
mod source_cursors;
#[cfg(test)]
mod storage_port_tests;
mod unit_of_work;
mod upload;

use std::net::SocketAddr;
use std::sync::Arc;

use futures::future::BoxFuture;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;

use crate::Owner;
use crate::authz::{EngineAuthority, EngineOperationAuthority, context_for_engine_operation};
use crate::error::ProtocolError;
use crate::llm::EmbeddingClient;
use crate::storage_ports::{CitedObjectErasePort, EngineStoragePorts, OwnerWritePermit};
use crate::verbs::schema::FlavorRegistryFrozen;

pub use access_admin::GroupMemberPage;
#[allow(unused_imports)]
pub(in crate::engine) use access_sets::AccessSets;
pub use goal_write::{
    GoalCreatePayloadWriteRequest, GoalDecomposeRequest, GoalMarkAchievedRequest,
    GoalModifyRequest, GoalTransitionRequest,
};
pub use ingest::EmbeddingDrainOutcome;
pub use mcp_listener::{EngineMcpListener, RunningMcpListener};
pub use memory_authoring::{AuthorDerivedAuthorizedOutcome, AuthorDerivedRequestInput};
pub use pipeline::{MemoryPermit, PermitMode};
pub use read_verbs::{
    FactCitationReadRequest, FactsCitingObjectReadRequest, GetGraphReadRequest,
    GetGraphReadResponse, GetMemoriesReadRequest, GetMemoriesReadResponse, GetMemoryReadRequest,
    GetMemoryReadResponse, ListChangeEventsReadRequest, ListChangeEventsReadResponse,
    ListWakeCandidatesReadRequest, ListWakeCandidatesReadResponse, MAX_WAKE_CANDIDATE_LIMIT,
    SearchReadRequest, SearchReadResponse,
};
pub use unit_of_work::{TypedFactIngest, UnitOfWork};
pub use upload::UploadCompleted;

pub struct Engine {
    registry: FlavorRegistryFrozen,
    system_authority_binding: crate::authz::SystemAuthorityBinding,
    delegation_runtime_binding: crate::authz::DelegationRuntimeBinding,
    storage: EngineStoragePorts,
    deployment_tool_scope: crate::authz::ToolScope,
    embed: Arc<RwLock<Option<Arc<dyn EmbeddingClient>>>>,
    embedding_runtime_policy: crate::llm::EmbeddingRuntimePolicy,
    embedding_reloader: Option<Arc<dyn EmbeddingClientReloader>>,
    cited_object_erase: Option<Arc<dyn CitedObjectErasePort>>,
    pub(crate) mcp_listen_addr: SocketAddr,
    pub(crate) mcp_listener: Option<Arc<dyn EngineMcpListener>>,
    pub(crate) mcp_url: Arc<RwLock<Option<String>>>,
}

#[derive(Debug)]
struct RequestTimeoutEmbeddingClient {
    inner: Arc<dyn EmbeddingClient>,
    request_timeout: std::time::Duration,
}

#[async_trait::async_trait]
impl EmbeddingClient for RequestTimeoutEmbeddingClient {
    async fn embed(&self, text: &str) -> Result<Vec<f32>, crate::llm::LlmError> {
        crate::llm::embed_with_timeout(self.inner.as_ref(), text, self.request_timeout).await
    }

    async fn embed_many(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, crate::llm::LlmError> {
        crate::llm::embed_many_with_timeout(self.inner.as_ref(), texts, self.request_timeout).await
    }

    fn model_id(&self) -> &str {
        self.inner.model_id()
    }

    fn dim(&self) -> usize {
        self.inner.dim()
    }
}

pub trait EmbeddingClientReloader: Send + Sync + std::fmt::Debug {
    fn reload<'a>(
        &'a self,
        owner: &'a Owner,
    ) -> BoxFuture<'a, Result<Option<Arc<dyn EmbeddingClient>>, String>>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingReloadOutcome {
    pub active: bool,
    pub model_id: Option<String>,
    pub dim: Option<usize>,
}

/// Owns the background tasks spawned by [`Engine::start`]. The engine
/// keeps no copy — `start` returns the only handle so the caller is
/// the single owner that can `stop()` it.
#[derive(Debug)]
pub struct EngineHandle {
    pub mcp_join: Option<JoinHandle<()>>,
}

impl Engine {
    pub(in crate::engine) fn operation_authority<'a, A>(
        &self,
        authority: &'a A,
    ) -> Result<EngineOperationAuthority<'a>, ProtocolError>
    where
        A: EngineAuthority + ?Sized,
    {
        let operation = context_for_engine_operation(authority)?;
        operation.validate_runtime_binding(Some(&self.delegation_runtime_binding))?;
        Ok(operation)
    }

    pub(in crate::engine) fn validate_write_permit(
        &self,
        permit: &OwnerWritePermit,
    ) -> Result<(), ProtocolError> {
        permit.validate_for_engine(&self.delegation_runtime_binding)
    }

    /// Storage handle, restricted to the engine module so the MCP tool
    /// layer cannot reach storage directly. Owner-scoped operations go
    /// through an engine verb that runs the authz pipeline.
    #[must_use]
    pub(in crate::engine) fn storage(&self) -> &EngineStoragePorts {
        &self.storage
    }

    #[must_use]
    pub fn embed_client(&self) -> Option<Arc<dyn EmbeddingClient>> {
        self.embed.try_read().ok().and_then(|slot| {
            slot.clone().map(|inner| {
                Arc::new(RequestTimeoutEmbeddingClient {
                    inner,
                    request_timeout: self.embedding_runtime_policy.request_timeout(),
                }) as Arc<dyn EmbeddingClient>
            })
        })
    }

    #[must_use]
    pub const fn embedding_runtime_policy(&self) -> crate::llm::EmbeddingRuntimePolicy {
        self.embedding_runtime_policy
    }

    pub async fn set_embed_client(&self, embed: Option<Arc<dyn EmbeddingClient>>) {
        *self.embed.write().await = embed;
    }

    /// Host-wired external object-store erase port. `None` when no blob backend
    /// is configured — owner-scope compliance erase then touches Postgres rows
    /// only (see [`crate::storage_ports::CitedObjectErasePort`]).
    #[must_use]
    pub fn cited_object_erase(&self) -> Option<Arc<dyn CitedObjectErasePort>> {
        self.cited_object_erase.clone()
    }

    /// Rebuilds the embedding client via the configured reload hook and
    /// swaps it into the engine.
    ///
    /// # Errors
    ///
    /// Returns `Internal` when no reload hook is wired into the engine or
    /// when the hook's reload itself fails.
    pub async fn reload_embedding_client(
        &self,
        owner: &Owner,
    ) -> Result<EmbeddingReloadOutcome, ProtocolError> {
        let reloader = self.embedding_reloader.as_ref().ok_or_else(|| {
            ProtocolError::internal("embedding reload hook not wired into engine")
        })?;
        let embed = reloader
            .reload(owner)
            .await
            .map_err(|e| ProtocolError::internal(format!("reload embedding client: {e}")))?;
        let outcome = EmbeddingReloadOutcome {
            active: embed.is_some(),
            model_id: embed.as_ref().map(|client| client.model_id().to_string()),
            dim: embed.as_ref().map(|client| client.dim()),
        };
        self.set_embed_client(embed).await;
        Ok(outcome)
    }

    /// Bound MCP URL after [`Engine::start`] succeeds. `None` before
    /// start, or after start if no [`EngineMcpListener`] was attached.
    #[must_use]
    pub fn mcp_url(&self) -> Option<String> {
        self.mcp_url.try_read().ok().and_then(|g| g.clone())
    }

    /// Override the bound MCP URL. Test + headless-wiring seam for
    /// callers that need to advertise a URL without spawning the
    /// listener task. Production callers go through `start` instead.
    pub async fn set_mcp_url(&self, url: String) {
        *self.mcp_url.write().await = Some(url);
    }

    /// Spawn the MCP listener if attached.
    ///
    /// Returns an [`EngineHandle`] the caller passes to
    /// [`Engine::stop`] to shut the task down cleanly. The engine
    /// itself does not keep a copy of the handle — single-owner
    /// shutdown is intentional so two callers can't race a stop.
    ///
    /// # Errors
    ///
    /// - the attached [`EngineMcpListener`] failed to bind/serve
    pub async fn start(self: Arc<Self>) -> Result<EngineHandle, ProtocolError> {
        let mcp_join = if let Some(listener) = self.mcp_listener.clone() {
            let running = listener.start(self.mcp_listen_addr, self.clone()).await?;
            let url = format!("http://{}/mcp", running.bound_addr);
            *self.mcp_url.write().await = Some(url);
            Some(running.join)
        } else {
            None
        };

        Ok(EngineHandle { mcp_join })
    }

    /// Abort the MCP listener if present. Safe to call once per
    /// [`EngineHandle`].
    pub fn stop(&self, handle: EngineHandle) {
        if let Some(mcp_join) = handle.mcp_join {
            mcp_join.abort();
        }
    }
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("registry", &self.registry)
            .field("storage", &"<storage ports>")
            .finish_non_exhaustive()
    }
}
