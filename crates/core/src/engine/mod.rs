//! Engine composite — wires FlavorRegistryFrozen, MemoryStore, and
//! an AuthResolver behind the typed verb surfaces of
//! docs/14-protocol-surface.md.

mod builder;
mod dispatcher;
mod goals;
mod ingest;
pub mod mcp_listener;
mod query;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Mutex, RwLock, watch};
use tokio::task::JoinHandle;

use crate::auth::AuthResolver;
use crate::auth::Credentials;
use crate::error::ProtocolError;
use crate::llm::{AnthropicClient, EmbeddingClient};
use crate::storage::{StorageError, StorageHandle};
use crate::verbs::query::MemoryStore;
use crate::verbs::schema::FlavorRegistryFrozen;
use crate::wake::target_adapter::TargetAdapter;
use crate::wake::token_store::WakeTokenStore;
use crate::{
    BindInferenceTierRequest, BindInferenceTierResponse, InferenceTargetRow,
    InferenceTierBindingRow, Owner, RegisterInferenceTargetRequest,
    RegisterInferenceTargetResponse, RemoveInferenceTargetRequest, RemoveInferenceTargetResponse,
    SetWakeEntriesRequest, SetWakeEntriesResponse,
};

pub use mcp_listener::{EngineMcpListener, RunningMcpListener};

pub struct Engine {
    registry: FlavorRegistryFrozen,
    // TODO(M3.B): remove MemoryStore
    memories: MemoryStore,
    auth: Box<dyn AuthResolver>,
    storage: StorageHandle,
    anthropic: Option<Arc<dyn AnthropicClient>>,
    embed: Option<Arc<dyn EmbeddingClient>>,
    pub(crate) dispatch_interval: Duration,
    pub(crate) wake_token_ttl: Duration,
    pub(crate) mcp_listen_addr: SocketAddr,
    pub(crate) mcp_listener: Option<Arc<dyn EngineMcpListener>>,
    pub(crate) mcp_url: Arc<RwLock<Option<String>>>,
    pub(crate) wake_token_store: Arc<WakeTokenStore>,
    pub(crate) target_adapter: Arc<RwLock<Option<Arc<dyn TargetAdapter>>>>,
    pub(crate) dispatch_tick_lock: Arc<Mutex<()>>,
}

/// Owns the background tasks spawned by [`Engine::start`]. The engine
/// keeps no copy — `start` returns the only handle so the caller is
/// the single owner that can `stop()` it.
pub struct EngineHandle {
    pub mcp_join: Option<JoinHandle<()>>,
    pub dispatch_join: JoinHandle<()>,
    pub stop_tx: watch::Sender<bool>,
}

impl Engine {
    #[must_use]
    pub(crate) fn storage(&self) -> &StorageHandle {
        &self.storage
    }

    #[must_use]
    pub fn embed_client(&self) -> Option<&Arc<dyn EmbeddingClient>> {
        self.embed.as_ref()
    }

    #[must_use]
    pub(crate) fn anthropic(&self) -> Option<&Arc<dyn AnthropicClient>> {
        self.anthropic.as_ref()
    }

    fn authorize_owner(&self, creds: &Credentials, owner: &Owner) -> Result<(), ProtocolError> {
        let resolved = self
            .auth
            .resolve(creds)
            .map_err(|_| ProtocolError::auth_required())?;
        if resolved.can_access_owner(owner) {
            Ok(())
        } else {
            Err(ProtocolError::forbidden(
                "principal cannot access requested owner",
            ))
        }
    }

    pub async fn register_inference_target(
        &self,
        creds: &Credentials,
        req: &RegisterInferenceTargetRequest,
    ) -> Result<RegisterInferenceTargetResponse, ProtocolError> {
        self.authorize_owner(creds, &req.owner)?;
        crate::inference::register_inference_target::register_inference_target(
            self.storage.as_ref(),
            req,
        )
        .await
    }

    pub async fn list_inference_targets(
        &self,
        creds: &Credentials,
        owner: &Owner,
    ) -> Result<Vec<InferenceTargetRow>, ProtocolError> {
        self.authorize_owner(creds, owner)?;
        crate::inference::list_inference_targets::list_inference_targets(
            self.storage.as_ref(),
            owner,
        )
        .await
    }

    pub async fn remove_inference_target(
        &self,
        creds: &Credentials,
        req: &RemoveInferenceTargetRequest,
    ) -> Result<RemoveInferenceTargetResponse, ProtocolError> {
        self.authorize_owner(creds, &req.owner)?;
        crate::inference::remove_inference_target::remove_inference_target(
            self.storage.as_ref(),
            req,
        )
        .await
    }

    pub async fn bind_inference_tier(
        &self,
        creds: &Credentials,
        req: &BindInferenceTierRequest,
    ) -> Result<BindInferenceTierResponse, ProtocolError> {
        self.authorize_owner(creds, &req.owner)?;
        crate::inference::bind_inference_tier::bind_inference_tier(self.storage.as_ref(), req).await
    }

    pub async fn list_inference_tier_bindings(
        &self,
        creds: &Credentials,
        owner: &Owner,
    ) -> Result<Vec<InferenceTierBindingRow>, ProtocolError> {
        self.authorize_owner(creds, owner)?;
        crate::inference::list_inference_tier_bindings::list_inference_tier_bindings(
            self.storage.as_ref(),
            owner,
        )
        .await
    }

    pub async fn set_wake_entries(
        &self,
        creds: &Credentials,
        req: &SetWakeEntriesRequest,
    ) -> Result<SetWakeEntriesResponse, ProtocolError> {
        self.authorize_owner(creds, &req.owner)?;
        let ctx = crate::inference::set_wake_entries::SetWakeEntriesContext {
            storage: self.storage.as_ref(),
            registry: self.registry(),
        };
        crate::inference::set_wake_entries::set_wake_entries(&ctx, req).await
    }

    /// Bound MCP URL after [`Engine::start`] succeeds. `None` before
    /// start, or after start if no [`EngineMcpListener`] was attached.
    #[must_use]
    pub fn mcp_url(&self) -> Option<String> {
        self.mcp_url.try_read().ok().and_then(|g| g.clone())
    }

    /// Override the bound MCP URL. Test + headless-wiring seam: the
    /// dispatcher's fire path injects `PROXIMA_MCP_URL` from this slot
    /// without requiring [`Engine::start`] to have spawned the
    /// listener task. Production callers go through `start` instead.
    pub async fn set_mcp_url(&self, url: String) {
        *self.mcp_url.write().await = Some(url);
    }

    /// Shared [`WakeTokenStore`] used by the dispatcher (mints) and the
    /// MCP listener's auth layer (resolves). Hosts pass this same `Arc`
    /// to `serve_streamable_http` so requests minted by a wake match
    /// the same store the dispatcher writes into.
    #[must_use]
    pub fn wake_token_store(&self) -> Arc<WakeTokenStore> {
        self.wake_token_store.clone()
    }

    /// Upsert the shell-author personality for `master_token_id` under
    /// `owner`. Delegates to [`Storage::ensure_master_token_personality`].
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

    /// Currently-installed wake harness adapter.
    #[must_use]
    pub fn target_adapter(&self) -> Option<Arc<dyn TargetAdapter>> {
        self.target_adapter.try_read().ok().and_then(|g| g.clone())
    }

    /// Override the installed wake harness adapter.
    pub async fn set_target_adapter(&self, adapter: Arc<dyn TargetAdapter>) {
        *self.target_adapter.write().await = Some(adapter);
    }

    /// Spawn the MCP listener (if attached) and start the dispatcher tick loop.
    ///
    /// Returns an [`EngineHandle`] the caller passes to
    /// [`Engine::stop`] to shut both tasks down cleanly. The engine
    /// itself does not keep a copy of the handle — single-owner
    /// shutdown is intentional so two callers can't race a stop.
    ///
    /// # Errors
    ///
    /// - the attached [`EngineMcpListener`] failed to bind/serve
    pub async fn start(self: Arc<Self>) -> Result<EngineHandle, ProtocolError> {
        // 1. Spawn MCP listener if a host attached one. Tests and
        // headless callers that don't need MCP can skip
        // `with_mcp_listener` and `mcp_url()` will stay `None`.
        let mcp_join = if let Some(listener) = self.mcp_listener.clone() {
            let running = listener
                .start(
                    self.mcp_listen_addr,
                    self.wake_token_store.clone(),
                    self.clone(),
                )
                .await?;
            let url = format!("http://{}/mcp", running.bound_addr);
            *self.mcp_url.write().await = Some(url);
            Some(running.join)
        } else {
            None
        };

        // 2. Spawn dispatcher tick loop.
        let (stop_tx, mut stop_rx) = watch::channel(false);
        let engine_for_dispatch = self.clone();
        let interval = self.dispatch_interval;
        let dispatch_join = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            // Skip the immediate first tick so callers that start +
            // stop quickly don't observe a tick they didn't intend.
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    changed = stop_rx.changed() => {
                        if changed.is_err() || *stop_rx.borrow() {
                            return;
                        }
                    }
                    _ = ticker.tick() => {
                        if let Err(e) = engine_for_dispatch.run_dispatcher_tick().await {
                            tracing::warn!(error = %e, "dispatcher tick failed");
                        }
                    }
                }
            }
        });

        Ok(EngineHandle {
            mcp_join,
            dispatch_join,
            stop_tx,
        })
    }

    /// Flip the dispatcher stop channel, await its join, and abort the
    /// MCP listener (if any). Safe to call once per [`EngineHandle`].
    pub async fn stop(&self, handle: EngineHandle) {
        let _ = handle.stop_tx.send(true);
        let _ = handle.dispatch_join.await;
        if let Some(mcp_join) = handle.mcp_join {
            mcp_join.abort();
        }
    }
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("registry", &self.registry)
            .field("memories", &self.memories)
            .field("auth", &"<dyn AuthResolver>")
            .field("storage", &"<dyn Storage>")
            .finish()
    }
}

pub(super) fn map_storage_err_for_goal_write(
    request_id: &str,
) -> impl FnOnce(StorageError) -> ProtocolError + '_ {
    move |e| match e {
        StorageError::ConstraintViolation(msg) if msg.starts_with("idempotency_conflict:") => {
            ProtocolError::idempotency_conflict(request_id)
        }
        StorageError::NotFound => ProtocolError::not_found("prior goal not found"),
        other => ProtocolError::internal(other.to_string()),
    }
}
