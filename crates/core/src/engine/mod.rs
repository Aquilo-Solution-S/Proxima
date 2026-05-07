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
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{RwLock, watch};
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
    InferenceTierBindingRow, Owner, Principal, RegisterInferenceTargetRequest,
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
    recipes_root: PathBuf,
    anthropic: Option<Arc<dyn AnthropicClient>>,
    embed: Option<Arc<dyn EmbeddingClient>>,
    pub(crate) dispatch_interval: Duration,
    pub(crate) wake_token_ttl: Duration,
    pub(crate) mcp_listen_addr: SocketAddr,
    pub(crate) goose_bin: Option<PathBuf>,
    pub(crate) mcp_listener: Option<Arc<dyn EngineMcpListener>>,
    pub(crate) mcp_url: Arc<RwLock<Option<String>>>,
    pub(crate) wake_token_store: Arc<WakeTokenStore>,
    pub(crate) target_adapter: Arc<RwLock<Option<Arc<dyn TargetAdapter>>>>,
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
    pub(crate) fn embed_client(&self) -> Option<&Arc<dyn EmbeddingClient>> {
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
            owner_recipes_root: self.owner_recipes_root(&req.owner),
        };
        crate::inference::set_wake_entries::set_wake_entries(&ctx, req).await
    }

    #[must_use]
    pub fn owner_recipes_root(&self, owner: &Owner) -> PathBuf {
        let principal_id = match &owner.principal {
            Principal::User(user) => user.into_inner(),
            Principal::Group(group) => group.into_inner(),
        };
        self.recipes_root.join(principal_id.to_string())
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

    /// Currently-installed [`TargetAdapter`]. Returns `None` until either
    /// [`Engine::start`] resolves the goose binary and installs a
    /// `LocalCliGooseAdapter`, or a test wires a mock via
    /// [`Engine::with_target_adapter`].
    #[must_use]
    pub fn target_adapter(&self) -> Option<Arc<dyn TargetAdapter>> {
        self.target_adapter.try_read().ok().and_then(|g| g.clone())
    }

    /// Override the installed [`TargetAdapter`]. Test seam: dispatch
    /// tests inject a mock that returns `Succeeded` without spawning a
    /// real goose subprocess. Production callers go through
    /// [`Engine::start`] which installs `LocalCliGooseAdapter`.
    pub async fn set_target_adapter(&self, adapter: Arc<dyn TargetAdapter>) {
        *self.target_adapter.write().await = Some(adapter);
    }

    /// Resolve the goose binary, run the boot self-check, spawn the
    /// MCP listener (if attached), and start the dispatcher tick loop.
    ///
    /// Returns an [`EngineHandle`] the caller passes to
    /// [`Engine::stop`] to shut both tasks down cleanly. The engine
    /// itself does not keep a copy of the handle — single-owner
    /// shutdown is intentional so two callers can't race a stop.
    ///
    /// # Errors
    ///
    /// - `goose` not on PATH and no `with_goose_bin(...)` set
    /// - `goose --version` exited non-zero or could not be spawned
    /// - the attached [`EngineMcpListener`] failed to bind/serve
    pub async fn start(self: Arc<Self>) -> Result<EngineHandle, ProtocolError> {
        // 1. Resolve + self-check goose. The dispatcher (Task 9) shells
        // out to this binary per wake, so a missing/broken goose is a
        // boot-time failure rather than a per-wake surprise.
        let goose_bin = match &self.goose_bin {
            Some(p) => p.clone(),
            None => which::which("goose").map_err(|e| {
                ProtocolError {
                    code: crate::error::ErrorCode::GooseCliUnavailable,
                    message: format!("goose not on PATH: {e}"),
                    request_id: None,
                }
            })?,
        };
        let _info = crate::wake::boot_check::verify_goose_on_path(&goose_bin)
            .await
            .map_err(|e| ProtocolError {
                code: crate::error::ErrorCode::GooseCliUnavailable,
                message: format!("goose self-check failed: {e}"),
                request_id: None,
            })?;

        // Install the production adapter unless a test seam already
        // populated one via `set_target_adapter`. The dispatcher reads
        // `target_adapter()` lazily on each fire, so tests can replace
        // it before `start` and we'll still see the mock.
        {
            let mut slot = self.target_adapter.write().await;
            if slot.is_none() {
                let adapter: Arc<dyn TargetAdapter> = Arc::new(
                    crate::wake::target_adapter::local_cli_goose::LocalCliGooseAdapter::new(
                        goose_bin.clone(),
                    ),
                );
                *slot = Some(adapter);
            }
        }

        // 2. Spawn MCP listener if a host attached one. Tests and
        // headless callers that don't need MCP can skip
        // `with_mcp_listener` and `mcp_url()` will stay `None`.
        let mcp_join = if let Some(listener) = self.mcp_listener.clone() {
            let running = listener
                .start(self.mcp_listen_addr, self.wake_token_store.clone())
                .await?;
            let url = format!("http://{}/mcp", running.bound_addr);
            *self.mcp_url.write().await = Some(url);
            Some(running.join)
        } else {
            None
        };

        // 3. Spawn dispatcher tick loop. Body is currently a Phase 1a
        // stub returning Ok(0); Task 9 replaces it with the real
        // dispatch logic.
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
            .field("recipes_root", &self.recipes_root)
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
