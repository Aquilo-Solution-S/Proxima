use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Mutex, RwLock};

use super::{EmbeddingClientReloader, Engine, EngineMcpListener};
use crate::auth::AuthResolver;
use crate::llm::{AnthropicClient, EmbeddingClient};
use crate::storage::{NoopStorage, StorageHandle};
use crate::verbs::query::MemoryStore;
use crate::verbs::schema::FlavorRegistryFrozen;
use crate::wake::target_adapter::TargetAdapter;
use crate::wake::token_store::WakeTokenStore;

const DEFAULT_DISPATCH_INTERVAL: Duration = Duration::from_secs(1);
const DEFAULT_WAKE_TOKEN_TTL: Duration = Duration::from_mins(5);
const DEFAULT_MCP_LISTEN_ADDR: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);

impl Engine {
    #[must_use]
    pub fn new(
        registry: FlavorRegistryFrozen,
        memories: MemoryStore,
        auth: Box<dyn AuthResolver>,
    ) -> Self {
        Self {
            registry,
            memories,
            auth,
            storage: Arc::new(NoopStorage),
            anthropic: None,
            embed: Arc::new(RwLock::new(None)),
            embedding_reloader: None,
            dispatch_interval: DEFAULT_DISPATCH_INTERVAL,
            wake_token_ttl: DEFAULT_WAKE_TOKEN_TTL,
            mcp_listen_addr: DEFAULT_MCP_LISTEN_ADDR,
            mcp_listener: None,
            mcp_url: Arc::new(RwLock::new(None)),
            wake_token_store: Arc::new(WakeTokenStore::new(DEFAULT_WAKE_TOKEN_TTL)),
            target_adapter: Arc::new(RwLock::new(None)),
            dispatch_tick_lock: Arc::new(Mutex::new(())),
        }
    }

    /// One-call composite assembly: build a [`crate::FlavorRegistry`],
    /// hand it to `register` for each linked flavor's `register` fn,
    /// freeze it, and wire the engine over `auth` + `storage`. This is
    /// the blessed embedding entry point for host binaries; chain
    /// `with_*` builders on the result for MCP, providers, and tuning.
    ///
    /// Migrations are NOT run here — the host runs substrate and
    /// per-flavor migrators against its pool before composing.
    #[must_use]
    pub fn compose(
        auth: Box<dyn AuthResolver>,
        storage: StorageHandle,
        register: impl FnOnce(&mut crate::FlavorRegistry),
    ) -> Self {
        let mut registry = crate::FlavorRegistry::new();
        register(&mut registry);
        Self::new(registry.freeze(), MemoryStore::new(), auth).with_storage(storage)
    }

    /// Get a reference to the schema registry.
    #[must_use]
    pub fn registry(&self) -> &FlavorRegistryFrozen {
        &self.registry
    }

    #[must_use]
    pub fn with_storage(mut self, storage: StorageHandle) -> Self {
        self.storage = storage;
        self
    }

    #[must_use]
    pub fn with_embed(mut self, embed: Arc<dyn EmbeddingClient>) -> Self {
        self.embed = Arc::new(RwLock::new(Some(embed)));
        self
    }

    #[must_use]
    pub fn with_embedding_reloader(mut self, reloader: Arc<dyn EmbeddingClientReloader>) -> Self {
        self.embedding_reloader = Some(reloader);
        self
    }

    #[must_use]
    pub fn with_anthropic(mut self, anthropic: Arc<dyn AnthropicClient>) -> Self {
        self.anthropic = Some(anthropic);
        self
    }

    #[must_use]
    pub fn with_dispatch_interval(mut self, dt: Duration) -> Self {
        self.dispatch_interval = dt;
        self
    }

    /// Override the wake-token TTL. Replaces the inner
    /// [`WakeTokenStore`] so the new TTL takes effect immediately;
    /// callers must do this before [`Engine::start`] (or before
    /// handing the store off to anything else) for the change to
    /// reach all readers.
    #[must_use]
    pub fn with_wake_token_ttl(mut self, ttl: Duration) -> Self {
        self.wake_token_ttl = ttl;
        self.wake_token_store = Arc::new(WakeTokenStore::new(ttl));
        self
    }

    #[must_use]
    pub fn with_mcp_listen_addr(mut self, addr: SocketAddr) -> Self {
        self.mcp_listen_addr = addr;
        self
    }

    /// Attach an MCP listener implementation. Without this, the
    /// engine starts without an MCP server (`mcp_url()` stays `None`)
    /// — fine for tests and headless callers that don't need MCP.
    /// `proxima-shell` and the dev-mcp binary wire a concrete listener
    /// backed by `proxima_mcp_server::serve_streamable_http`.
    #[must_use]
    pub fn with_mcp_listener(mut self, listener: Arc<dyn EngineMcpListener>) -> Self {
        self.mcp_listener = Some(listener);
        self
    }

    /// Pre-install a wake harness adapter.
    ///
    /// Replaces the `Arc<RwLock<...>>` slot wholesale rather than
    /// reaching into it — builders run single-threaded so we don't pay
    /// for `try_write` here, and a fresh slot can't drop the adapter on
    /// a contended write.
    #[must_use]
    pub fn with_target_adapter(mut self, adapter: Arc<dyn TargetAdapter>) -> Self {
        self.target_adapter = Arc::new(RwLock::new(Some(adapter)));
        self
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use uuid::Uuid;

    use super::Engine;
    use crate::auth::NoAuth;
    use crate::storage::NoopStorage;
    use crate::{OrgId, Owner, Principal, UserId};

    #[test]
    fn compose_assembles_engine_over_registry_closure() {
        let owner = Owner {
            principal: Principal::User(UserId::new(Uuid::nil())),
            org_id: OrgId::new(Uuid::nil()),
        };
        let auth = NoAuth::new(owner.principal.clone(), owner);
        let engine = Engine::compose(Box::new(auth), Arc::new(NoopStorage), |_registry| {});
        assert!(engine.mcp_url().is_none());
        assert!(engine.embed_client().is_none());
    }
}
