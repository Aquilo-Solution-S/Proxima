use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

use tokio::sync::RwLock;

use super::{EmbeddingClientReloader, Engine, EngineMcpListener};
use crate::FlavorRegistryError;
use crate::authz::{
    DelegationRuntimeAuthority, DelegationRuntimeBinding, SystemAuthority, SystemAuthorityBinding,
};
use crate::llm::EmbeddingClient;
use crate::storage_ports::{EngineStoragePorts, StoragePorts};
use crate::verbs::schema::FlavorRegistryFrozen;

const DEFAULT_MCP_LISTEN_ADDR: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);

impl Engine {
    #[must_use]
    pub fn new(registry: FlavorRegistryFrozen) -> Self {
        Self {
            registry,
            system_authority_binding: SystemAuthorityBinding::fresh(),
            delegation_runtime_binding: DelegationRuntimeBinding::fresh(),
            storage: EngineStoragePorts::from(StoragePorts::rejecting()),
            deployment_tool_scope: crate::authz::ToolScope::All,
            embed: Arc::new(RwLock::new(None)),
            embedding_runtime_policy: crate::llm::EmbeddingRuntimePolicy::default(),
            embedding_reloader: None,
            publication: crate::publication::PublicationConfig::default(),
            mcp_listen_addr: DEFAULT_MCP_LISTEN_ADDR,
            mcp_listener: None,
            mcp_url: Arc::new(RwLock::new(None)),
        }
    }

    /// Split out the host-held System write witness while the caller still
    /// owns the engine value. Tool contexts receive only shared engine handles,
    /// so they cannot extract this after boot.
    #[must_use]
    pub fn into_system_authority(self) -> (Self, SystemAuthority) {
        let authority = SystemAuthority::new(self.system_authority_binding.clone());
        (self, authority)
    }

    /// Split out both boot-only runtime witnesses while the caller still owns
    /// the Engine. The delegation witness has no accessor and is withheld
    /// from tool/worker contexts after composing the one bound service set.
    #[doc(hidden)]
    #[must_use]
    pub fn into_runtime_authorities(self) -> (Self, SystemAuthority, DelegationRuntimeAuthority) {
        let system = SystemAuthority::new(self.system_authority_binding.clone());
        let delegation = DelegationRuntimeAuthority::new(self.delegation_runtime_binding.clone());
        (self, system, delegation)
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

    /// Deployment tool-surface profile enforced at engine chokepoints that
    /// consume tool scope (currently wake-candidate admission). Transport
    /// hosts additionally intersect this into per-caller `AuthzContext`
    /// scope; setting it here keeps Host-API callers inside the same
    /// deployment surface even when their `AuthzContext` carries
    /// `ToolScope::All`. Defaults to `ToolScope::All`.
    #[must_use]
    pub fn with_deployment_tool_scope(mut self, scope: crate::authz::ToolScope) -> Self {
        self.deployment_tool_scope = scope;
        self
    }

    /// The composed deployment tool-surface profile.
    #[must_use]
    pub fn deployment_tool_scope(&self) -> &crate::authz::ToolScope {
        &self.deployment_tool_scope
    }

    #[must_use]
    pub fn with_embed(mut self, embed: Arc<dyn EmbeddingClient>) -> Self {
        self.embed = Arc::new(RwLock::new(Some(embed)));
        self
    }

    /// Set provider batching and durable-claim lifecycle policy for the
    /// installed embedding client. Direct core composition defaults to the
    /// generic finite policy; hosts should pass their resolved deployment
    /// policy explicitly.
    #[must_use]
    pub const fn with_embedding_runtime_policy(
        mut self,
        policy: crate::llm::EmbeddingRuntimePolicy,
    ) -> Self {
        self.embedding_runtime_policy = policy;
        self
    }

    /// Bind the deployment's publication source and capture bounds
    /// (docs/18 §Configuration).
    ///
    /// Fallible on purpose. A registry that freezes a listenable schema and
    /// a configuration that binds no source is a deployment which would
    /// refuse every write of that schema at runtime; the host learns that
    /// at boot, naming the schemas that made a source required, instead of
    /// on the first admission.
    ///
    /// # Errors
    ///
    /// Returns [`crate::publication::PublicationError::SourceUnbound`] when
    /// the frozen registry carries a listenable schema and `config.source`
    /// is `None`.
    pub fn try_with_publication_config(
        mut self,
        config: crate::publication::PublicationConfig,
    ) -> Result<Self, crate::publication::PublicationError> {
        config.validate_against(self.registry.listenable_schema_ids())?;
        self.publication = config;
        Ok(self)
    }

    /// The bound publication configuration.
    #[must_use]
    pub const fn publication_config(&self) -> &crate::publication::PublicationConfig {
        &self.publication
    }

    #[must_use]
    pub fn with_embedding_reloader(mut self, reloader: Arc<dyn EmbeddingClientReloader>) -> Self {
        self.embedding_reloader = Some(reloader);
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
    use std::sync::Arc;
    use std::time::Duration;

    use async_trait::async_trait;

    use super::Engine;
    use crate::{EmbeddingClient, EmbeddingRuntimePolicy, FlavorRegistry, LlmError, StoragePorts};

    #[test]
    fn compose_assembles_engine_over_registry_closure() {
        let engine = Engine::compose_or_panic_for_tests(StoragePorts::rejecting(), |_registry| {});
        assert!(engine.mcp_url().is_none());
        assert!(engine.embed_client().is_none());
    }

    #[derive(Debug)]
    struct HangingCustomEmbedding;

    #[async_trait]
    impl EmbeddingClient for HangingCustomEmbedding {
        async fn embed(&self, _text: &str) -> Result<Vec<f32>, LlmError> {
            std::future::pending().await
        }

        async fn embed_many(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>, LlmError> {
            std::future::pending().await
        }

        fn model_id(&self) -> &'static str {
            "hanging-custom"
        }

        fn dim(&self) -> usize {
            4
        }
    }

    #[tokio::test(start_paused = true)]
    async fn engine_enforces_request_timeout_for_custom_client_batches() {
        let policy = EmbeddingRuntimePolicy::new(
            Duration::from_secs(1),
            2,
            Duration::from_secs(1),
            Duration::from_secs(3),
        )
        .expect("valid policy");
        let engine = Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests())
            .with_embedding_runtime_policy(policy)
            .with_embed(Arc::new(HangingCustomEmbedding));

        let result = engine
            .embed_client()
            .expect("client")
            .embed_many(&["one".to_owned(), "two".to_owned()])
            .await;
        assert!(
            matches!(result, Err(LlmError::Embed(ref message)) if message.contains("timed out")),
            "engine timeout must remain retryable: {result:?}"
        );
    }

    /// A deployment that freezes a listenable schema and binds no source
    /// would refuse every write of that schema. The host learns it at boot,
    /// and the message names the schema that made a source required.
    #[test]
    fn a_listenable_registry_refuses_to_boot_without_a_source() {
        let engine = Engine::new(crate::test_fixtures::probe_registry());
        let err = engine
            .try_with_publication_config(crate::publication::PublicationConfig::default())
            .expect_err("an unbound source must fail the build");
        assert!(
            matches!(
                err,
                crate::publication::PublicationError::SourceUnbound { ref schema_id }
                    if schema_id == "probe/listenable-v1"
            ),
            "unexpected error: {err:?}"
        );
    }

    /// The same registry boots once a source is bound, and the bound value
    /// is what the engine carries.
    #[test]
    fn a_bound_source_boots_and_is_readable() {
        let source = crate::publication::PublicationSource::new("urn:proxima:test-install")
            .expect("a URN is an absolute source");
        let engine = Engine::new(crate::test_fixtures::probe_registry())
            .try_with_publication_config(crate::publication::PublicationConfig::new(source.clone()))
            .expect("a bound source boots");
        assert_eq!(engine.publication_config().source.as_ref(), Some(&source));
    }

    /// A registry with no listenable schema needs no source at all: the
    /// boot rule must not tax deployments the feature does not touch.
    #[test]
    fn a_registry_with_no_listenable_schema_needs_no_source() {
        Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests())
            .try_with_publication_config(crate::publication::PublicationConfig::default())
            .expect("no listenable schema, no obligation");
    }
}
