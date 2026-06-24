//! Engine composite — wires `FlavorRegistryFrozen` behind the typed
//! verb surfaces of docs/14-protocol-surface.md.

mod builder;
mod fact_retention;
mod ingest;
pub mod mcp_listener;
mod memory_authoring;
mod personality;
mod query;

use std::net::SocketAddr;
use std::sync::Arc;

use futures::future::BoxFuture;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;

use crate::authz::{AuthzContext, Role};
use crate::error::ProtocolError;
use crate::llm::{AnthropicClient, EmbeddingClient};
use crate::storage::{StorageError, StorageHandle};
use crate::verbs::schema::FlavorRegistryFrozen;
use crate::{Owner, Principal, SetWakeEntriesRequest, SetWakeEntriesResponse, WakeEntryDraft};

pub use ingest::EmbeddingDrainOutcome;
pub use mcp_listener::{EngineMcpListener, RunningMcpListener};
pub use memory_authoring::{AuthorDerivedEdgeInput, AuthorDerivedRequestInput};

pub struct Engine {
    registry: FlavorRegistryFrozen,
    storage: StorageHandle,
    anthropic: Option<Arc<dyn AnthropicClient>>,
    embed: Arc<RwLock<Option<Arc<dyn EmbeddingClient>>>>,
    embedding_reloader: Option<Arc<dyn EmbeddingClientReloader>>,
    pub(crate) mcp_listen_addr: SocketAddr,
    pub(crate) mcp_listener: Option<Arc<dyn EngineMcpListener>>,
    pub(crate) mcp_url: Arc<RwLock<Option<String>>>,
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

fn map_set_wake_entries_storage_err(
    err: StorageError,
    entries: &[WakeEntryDraft],
) -> ProtocolError {
    match err {
        StorageError::NotFound => ProtocolError::not_found("personality instance not found"),
        StorageError::ConstraintViolation(msg)
            if msg.contains("personality_wake_entries_active_trigger_uq") =>
        {
            let first = entries.first();
            ProtocolError::trigger_conflict(
                first.map_or("unknown", |entry| entry.trigger_kind.as_str()),
                first.map_or("unknown", |entry| entry.trigger_id.as_str()),
            )
        }
        other => ProtocolError::internal(other.to_string()),
    }
}

impl Engine {
    #[must_use]
    pub(crate) fn storage(&self) -> &StorageHandle {
        &self.storage
    }

    #[must_use]
    pub fn embed_client(&self) -> Option<Arc<dyn EmbeddingClient>> {
        self.embed.try_read().ok().and_then(|slot| slot.clone())
    }

    pub async fn set_embed_client(&self, embed: Option<Arc<dyn EmbeddingClient>>) {
        *self.embed.write().await = embed;
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

    /// Owner-scoped replacement of a personality instance's wake entries.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context cannot access `req.principal` or
    /// lacks the admin role; `InvalidArgument`,
    /// `DuplicateTriggerInRequest` on request validation; `NotFound`
    /// when the personality instance doesn't exist; `TriggerConflict`
    /// or `Internal` from storage.
    pub async fn set_wake_entries(
        &self,
        authz: &AuthzContext,
        req: &SetWakeEntriesRequest,
    ) -> Result<SetWakeEntriesResponse, ProtocolError> {
        authorize(authz, &req.principal, Role::Admin)?;
        let effective = req.clone();
        crate::personality::validate_wake_entries_detect_config(&effective.entries)?;
        self.storage
            .set_wake_entries(&effective)
            .await
            .map_err(|err| map_set_wake_entries_storage_err(err, &effective.entries))
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

    /// Upsert the shell-author personality for `master_token_id` under
    /// `owner`. Delegates to `Storage::ensure_master_token_personality`.
    /// Called by `McpToolHost::call_tool` (in the `mcp-server` crate)
    /// before dispatching master-token requests so the per-token identity
    /// is always minted before `caller_self_perspective` is defaulted.
    ///
    /// # Errors
    ///
    /// Propagates storage failures unchanged.
    pub async fn ensure_master_token_personality(
        &self,
        owner: &Owner,
        master_token_id: uuid::Uuid,
    ) -> Result<crate::storage::MasterTokenPersonality, StorageError> {
        self.storage
            .ensure_master_token_personality(owner, master_token_id)
            .await
    }

    /// Upsert the personality for `subject` under `owner`. Delegates to
    /// `Storage::ensure_subject_personality`.
    ///
    /// # Errors
    ///
    /// Propagates storage failures unchanged.
    pub async fn ensure_subject_personality(
        &self,
        owner: &Owner,
        subject: &Principal,
    ) -> Result<crate::storage::MasterTokenPersonality, StorageError> {
        self.storage
            .ensure_subject_personality(owner, subject)
            .await
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
            .field("storage", &"<dyn Storage>")
            .finish_non_exhaustive()
    }
}

pub(crate) fn authorize(
    authz: &AuthzContext,
    principal: &Principal,
    role: Role,
) -> Result<(), ProtocolError> {
    if !authz.identity.can_access_principal(principal) {
        return Err(ProtocolError::forbidden(
            "principal cannot access requested principal",
        ));
    }
    if !authz.capabilities.roles.has(role) {
        return Err(ProtocolError::forbidden(role.denied_message()));
    }
    Ok(())
}
