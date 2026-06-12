//! Embedded-engine facade: env config -> migrations -> compose -> start.
//!
//! Wraps the blessed `Engine::compose` embedding entry point
//! (`proxima_core::engine`) for host binaries. Host wiring template:
//! `examples/embedded-minimal`. Cohabitation contract: core, flavors,
//! and the host's own sqlx migrations share one database and the
//! default `_sqlx_migrations` table; every migrator in that database
//! must set `ignore_missing(true)`, and host tables must stay out of
//! the `proxima_core` / per-flavor schemas.

mod app;
mod bundle;
mod config;
mod migrations;
mod runtime;
mod runtime_config;

pub use app::{AppContext, AppInfo, Authz, FlavorApp};
pub use bundle::FlavorBundle;
pub use config::EmbedConfig;
pub use migrations::{
    MigrationError, MigrationRunReport, MigrationVersion, NamedMigrator,
    run_core_and_flavor_migrations,
};
pub use proxima_mcp_server::McpAuthContext;
pub use runtime::{
    BuiltProxima, Proxima, RunningProxima, layered_router, layered_router_with_revalidation, run,
};
pub use runtime_config::{McpSettings, ProximaError, RuntimeBuilder, RuntimeConfig, RuntimeParts};

use std::sync::Arc;

use proxima_blob_s3::CitedBlobStore;
use proxima_core::llm::{AnthropicClient, EmbeddingClient};
use proxima_core::{Engine, EngineHandle, FlavorRegistry, GroupId, OrgId, Owner, Principal};
use proxima_storage_pg::PgStorage;
use sqlx::PgPool;

/// One org-wide Owner: a Group principal carrying the org id.
///
/// This is the single place embedded hosts construct `Owner`. When
/// the kernel's `Owner := Principal` refactor lands in code, only this
/// function changes.
#[must_use]
pub fn company_owner(org: uuid::Uuid) -> Owner {
    Owner {
        principal: Principal::Group(GroupId::new(org)),
        org_id: OrgId::new(org),
    }
}

type RegisterFn = Box<dyn FnOnce(&mut FlavorRegistry) + Send>;

/// Builder for an embedded engine.
pub struct ProximaBuilder {
    config: EmbedConfig,
    owner: Owner,
    registers: Vec<RegisterFn>,
    migrators: Vec<NamedMigrator>,
    embed_client: Option<Arc<dyn EmbeddingClient>>,
    anthropic: Option<Arc<dyn AnthropicClient>>,
}

impl std::fmt::Debug for ProximaBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProximaBuilder")
            .field("config", &self.config)
            .field("owner", &self.owner)
            .field("flavors", &self.registers.len())
            .field("migrators", &self.migrators.len())
            .field("has_embed_client", &self.embed_client.is_some())
            .field("has_anthropic", &self.anthropic.is_some())
            .finish()
    }
}

/// A booted embedded engine plus its companion handles.
pub struct EmbeddedProxima {
    pub engine: Arc<Engine>,
    pub handle: EngineHandle,
    pub pool: PgPool,
    pub blobs: Option<CitedBlobStore>,
    pub owner: Owner,
}

impl std::fmt::Debug for EmbeddedProxima {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmbeddedProxima")
            .field("handle", &self.handle)
            .field("pool", &self.pool)
            .field("blobs", &self.blobs)
            .field("owner", &self.owner)
            .finish_non_exhaustive()
    }
}

impl ProximaBuilder {
    #[must_use]
    pub fn new(config: EmbedConfig, owner: Owner) -> Self {
        Self {
            config,
            owner,
            registers: Vec::new(),
            migrators: Vec::new(),
            embed_client: None,
            anthropic: None,
        }
    }

    /// Link one flavor: its `register` fn and optionally its migrator.
    #[must_use]
    pub fn flavor(
        self,
        register: impl FnOnce(&mut FlavorRegistry) + Send + 'static,
        migrator: Option<sqlx::migrate::Migrator>,
    ) -> Self {
        self.flavor_named("inline-flavor", register, migrator)
    }

    /// Link one named flavor: its `register` fn and optionally its
    /// migrator. Prefer the flavor id as `source`, e.g. `proxima-code`.
    #[must_use]
    pub fn flavor_named(
        mut self,
        source: &'static str,
        register: impl FnOnce(&mut FlavorRegistry) + Send + 'static,
        migrator: Option<sqlx::migrate::Migrator>,
    ) -> Self {
        self.registers.push(Box::new(register));
        self.migrators
            .extend(migrator.map(|migrator| NamedMigrator::new(source, migrator)));
        self
    }

    /// Link a statically-composed flavor bundle (single flavor or tuple).
    #[must_use]
    pub fn bundle<B: FlavorBundle + 'static>(mut self) -> Self {
        self.registers.push(Box::new(B::register));
        self.migrators.extend(B::migrators());
        self
    }

    /// Embedding client passthrough (`Engine::with_embed`).
    ///
    /// Inference targets and tiers remain runtime config rows, not env
    /// handled by this facade.
    #[must_use]
    pub fn embed_client(mut self, client: Arc<dyn EmbeddingClient>) -> Self {
        self.embed_client = Some(client);
        self
    }

    /// Anthropic client passthrough (`Engine::with_anthropic`).
    #[must_use]
    pub fn anthropic(mut self, client: Arc<dyn AnthropicClient>) -> Self {
        self.anthropic = Some(client);
        self
    }

    /// Connect, migrate, compose, and start the embedded engine.
    ///
    /// # Errors
    ///
    /// Returns `EmbedError::Storage` for connection or migration
    /// failures and `EmbedError::Engine` when engine startup fails.
    pub async fn boot(self) -> Result<EmbeddedProxima, EmbedError> {
        let pg = PgStorage::connect(&self.config.database_url)
            .await
            .map_err(|e| EmbedError::Storage(e.to_string()))?;
        run_core_and_flavor_migrations(&pg, self.migrators)
            .await
            .map_err(|e| EmbedError::Storage(e.to_string()))?;

        let registers = self.registers;
        let mut engine = Engine::compose(Arc::new(pg.clone()), move |registry| {
            for register in registers {
                register(registry);
            }
        });
        if let Some(client) = self.embed_client {
            engine = engine.with_embed(client);
        }
        if let Some(client) = self.anthropic {
            engine = engine.with_anthropic(client);
        }

        let engine = Arc::new(engine);
        let handle = engine
            .clone()
            .start()
            .await
            .map_err(|e| EmbedError::Engine(e.to_string()))?;
        let pool = pg.pool().clone();
        let blobs = self
            .config
            .s3
            .map(|s3| CitedBlobStore::new(pool.clone(), s3));
        Ok(EmbeddedProxima {
            engine,
            handle,
            pool,
            blobs,
            owner: self.owner,
        })
    }
}

/// Errors from embedded boot.
#[derive(Debug, thiserror::Error)]
pub enum EmbedError {
    #[error("config: {0}")]
    Config(String),
    #[error("storage: {0}")]
    Storage(String),
    #[error("engine: {0}")]
    Engine(String),
}

#[cfg(test)]
mod tests {
    use proxima_core::Principal;

    #[test]
    fn company_owner_is_group_scoped_to_org() {
        let org = uuid::Uuid::now_v7();
        let owner = super::company_owner(org);
        assert!(matches!(owner.principal, Principal::Group(_)));
    }
}
