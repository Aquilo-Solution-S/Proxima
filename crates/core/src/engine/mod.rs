//! Engine composite — wires `FlavorRegistryFrozen` behind the typed
//! verb surfaces of docs/14-protocol-surface.md.

#[allow(dead_code)]
mod access_admin;
mod access_sets;
mod builder;
mod embeddings;
mod errors;
mod goal_write;
mod ingest;
pub mod mcp_listener;
mod memory_authoring;
mod owner_inverse;
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

use tokio::sync::RwLock;
use tokio::task::JoinHandle;

use crate::authz::{EngineAuthority, EngineOperationAuthority, context_for_engine_operation};
use crate::error::ProtocolError;
use crate::llm::EmbeddingRouter;
use crate::storage_ports::{EngineStoragePorts, OwnerWritePermit};
use crate::verbs::schema::FlavorRegistryFrozen;

pub use crate::storage_ports::{HostStateCommand, HostStateOutcome};
pub use access_admin::GroupMemberPage;
pub use embeddings::EmbeddingDrainOutcome;
pub use goal_write::{
    GoalCreatePayloadWriteRequest, GoalDecomposeRequest, GoalMarkAchievedRequest,
    GoalModifyRequest, GoalTransitionRequest,
};
pub use mcp_listener::{EngineMcpListener, RunningMcpListener};
pub use memory_authoring::{
    DerivationIdentity, DerivedMemory, DerivedMemoryOutcome, MemoryTarget, SeriesHandle,
};
pub use pipeline::{MemoryPermit, PermitMode, WritePermit};
pub use read_verbs::{
    FactCitationReadRequest, FactsCitingObjectReadRequest, GetGraphReadRequest,
    GetGraphReadResponse, GetMemoriesReadRequest, GetMemoriesReadResponse, GetMemoryReadRequest,
    GetMemoryReadResponse, ListChangeEventsReadRequest, ListChangeEventsReadResponse,
    ListWakeCandidatesReadRequest, ListWakeCandidatesReadResponse, MAX_WAKE_CANDIDATE_LIMIT,
    SearchReadRequest, SearchReadResponse,
};
pub use unit_of_work::{FactWrite, HostStateMaintenanceAuthority, HostStateUnitOfWork, UnitOfWork};
pub use upload::{UploadCompleted, UploadCompletionExpectation};

#[cfg(test)]
#[doc(hidden)]
pub(crate) use access_sets::tests::MembershipStorage;

pub struct Engine {
    registry: FlavorRegistryFrozen,
    system_authority_binding: crate::authz::SystemAuthorityBinding,
    delegation_runtime_binding: crate::authz::DelegationRuntimeBinding,
    storage: EngineStoragePorts,
    deployment_tool_scope: crate::authz::ToolScope,
    embedding_router: Option<Arc<dyn EmbeddingRouter>>,
    embedding_runtime_policy: crate::llm::EmbeddingRuntimePolicy,
    /// Owners whose route or provider failed recently; the drain skips
    /// their jobs until the delay passes.
    embedding_backoff: embeddings::EmbeddingBackoff,
    /// Producer identity and capture bounds for listenable Fact schemas
    /// (docs/18). Default-constructed means "no source bound", which is
    /// correct for the overwhelming majority of deployments: nothing is
    /// listenable, so nothing needs one.
    publication: crate::publication::PublicationConfig,
    pub(crate) mcp_listen_addr: SocketAddr,
    pub(crate) mcp_listener: Option<Arc<dyn EngineMcpListener>>,
    pub(crate) mcp_url: Arc<RwLock<Option<String>>>,
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

    /// The host's embedding router, if one is installed.
    #[must_use]
    pub fn embedding_router(&self) -> Option<&Arc<dyn EmbeddingRouter>> {
        self.embedding_router.as_ref()
    }

    #[must_use]
    pub const fn embedding_runtime_policy(&self) -> crate::llm::EmbeddingRuntimePolicy {
        self.embedding_runtime_policy
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
