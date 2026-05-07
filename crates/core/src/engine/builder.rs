use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::RwLock;

use super::{Engine, EngineMcpListener};
use crate::auth::AuthResolver;
use crate::llm::{AnthropicClient, EmbeddingClient};
use crate::storage::{NoopStorage, StorageHandle};
use crate::verbs::query::MemoryStore;
use crate::verbs::schema::FlavorRegistryFrozen;
use crate::wake::target_adapter::TargetAdapter;
use crate::wake::token_store::WakeTokenStore;

const DEFAULT_DISPATCH_INTERVAL: Duration = Duration::from_secs(1);
const DEFAULT_WAKE_TOKEN_TTL: Duration = Duration::from_secs(300);
const DEFAULT_MCP_LISTEN_ADDR: SocketAddr =
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);

impl Engine {
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
            recipes_root: default_recipes_root(),
            anthropic: None,
            embed: None,
            dispatch_interval: DEFAULT_DISPATCH_INTERVAL,
            wake_token_ttl: DEFAULT_WAKE_TOKEN_TTL,
            mcp_listen_addr: DEFAULT_MCP_LISTEN_ADDR,
            goose_bin: None,
            mcp_listener: None,
            mcp_url: Arc::new(RwLock::new(None)),
            wake_token_store: Arc::new(WakeTokenStore::new(DEFAULT_WAKE_TOKEN_TTL)),
            target_adapter: Arc::new(RwLock::new(None)),
        }
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
    pub fn with_recipes_root(mut self, recipes_root: PathBuf) -> Self {
        self.recipes_root = recipes_root;
        self
    }

    #[must_use]
    pub fn with_embed(mut self, embed: Arc<dyn EmbeddingClient>) -> Self {
        self.embed = Some(embed);
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

    /// Pin the goose binary used by the dispatcher's local-CLI target
    /// adapter. Defaults to `which::which("goose")` resolved at
    /// [`Engine::start`].
    #[must_use]
    pub fn with_goose_bin(mut self, bin: PathBuf) -> Self {
        self.goose_bin = Some(bin);
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

    /// Pre-install a [`TargetAdapter`]. Test seam: dispatch tests wire
    /// a mock so they don't need a real goose binary. Production paths
    /// rely on [`Engine::start`] installing `LocalCliGooseAdapter` from
    /// the resolved goose binary.
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

fn default_recipes_root() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join(".proxima/recipes")
}
