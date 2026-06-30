use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

use tokio::sync::RwLock;

use super::{EmbeddingClientReloader, Engine, EngineMcpListener};
use crate::FlavorRegistryError;
use crate::llm::{AnthropicClient, EmbeddingClient};
use crate::storage_ports::{EngineStoragePorts, StoragePorts};
use crate::verbs::schema::FlavorRegistryFrozen;

const DEFAULT_MCP_LISTEN_ADDR: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);

impl Engine {
    #[must_use]
    pub fn new(registry: FlavorRegistryFrozen) -> Self {
        Self {
            registry,
            storage: EngineStoragePorts::from(StoragePorts::rejecting()),
            anthropic: None,
            embed: Arc::new(RwLock::new(None)),
            embedding_reloader: None,
            mcp_listen_addr: DEFAULT_MCP_LISTEN_ADDR,
            mcp_listener: None,
            mcp_url: Arc::new(RwLock::new(None)),
        }
    }

    /// Test-only infallible composite assembly.
    ///
    /// Production hosts call [`Self::try_compose`] and propagate the typed
    /// registry error.
    ///
    /// # Panics
    ///
    /// Panics if registry registration or freeze fails.
    #[cfg(any(test, feature = "test-fixtures"))]
    #[doc(hidden)]
    #[must_use]
    pub fn compose_or_panic_for_tests(
        storage: StoragePorts,
        register: impl FnOnce(&mut crate::FlavorRegistry),
    ) -> Self {
        Self::try_compose(storage, |registry| {
            register(registry);
            Ok(())
        })
        .expect("flavor registry must be valid")
    }

    /// One-call composite assembly: build a [`crate::FlavorRegistry`],
    /// hand it to `register` for each linked flavor's `register` fn,
    /// freeze it, and wire the engine over `storage`. Authentication lives
    /// at the transport edge; chain `with_*` builders on the result for MCP,
    /// providers, and tuning.
    ///
    /// Migrations are NOT run here — the host runs substrate and
    /// per-flavor migrators against its pool before composing.
    ///
    /// # Errors
    ///
    /// Returns a registry error from flavor registration or freeze.
    pub fn try_compose(
        storage: StoragePorts,
        register: impl FnOnce(&mut crate::FlavorRegistry) -> Result<(), FlavorRegistryError>,
    ) -> Result<Self, FlavorRegistryError> {
        let mut registry = crate::FlavorRegistry::new();
        register(&mut registry)?;
        Ok(Self::new(registry.try_freeze()?).with_storage_ports(storage))
    }

    /// Get a reference to the schema registry.
    #[must_use]
    pub fn registry(&self) -> &FlavorRegistryFrozen {
        &self.registry
    }

    #[must_use]
    pub fn with_storage_ports(mut self, storage: StoragePorts) -> Self {
        self.storage = EngineStoragePorts::from(storage);
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
    pub fn with_mcp_listen_addr(mut self, addr: SocketAddr) -> Self {
        self.mcp_listen_addr = addr;
        self
    }

    /// Attach an MCP listener implementation. Without this, the
    /// engine starts without an MCP server (`mcp_url()` stays `None`)
    /// — fine for tests and headless callers that don't need MCP.
    /// Host binaries wire a concrete listener
    /// backed by `proxima_mcp_server::serve_streamable_http`.
    #[must_use]
    pub fn with_mcp_listener(mut self, listener: Arc<dyn EngineMcpListener>) -> Self {
        self.mcp_listener = Some(listener);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::Engine;
    use crate::StoragePorts;

    #[test]
    fn compose_assembles_engine_over_registry_closure() {
        let engine = Engine::compose_or_panic_for_tests(StoragePorts::rejecting(), |_registry| {});
        assert!(engine.mcp_url().is_none());
        assert!(engine.embed_client().is_none());
    }
}
