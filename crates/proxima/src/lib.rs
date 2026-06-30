//! Embedded-engine facade: env config -> migrations -> compose -> start.
//!
//! Wraps the blessed `Engine::try_compose` embedding entry point
//! (`proxima_core::engine`) for host binaries. Host wiring template:
//! `examples/embedded-minimal`. Cohabitation contract: core, flavors,
//! and the host's own sqlx migrations share one database and the
//! default `_sqlx_migrations` table; every migrator in that database
//! must set `ignore_missing(true)`, and host tables must stay out of
//! the `proxima_core` / per-flavor schemas.
//!
//! # Public surface tiers
//!
//! This facade exposes two intentional, supported tiers:
//!
//! - **Host entry point (most hosts use only this):** [`Proxima`], [`run`],
//!   [`RuntimeBuilder`]/[`RuntimeConfig`], and the `from_env` → migrate →
//!   compose → `run` flow. A host binary that just stands up the MCP server
//!   needs nothing below this line.
//! - **Flavor SDK:** import from `proxima::flavor`, not the root facade. The
//!   SDK exposes payload traits, ids, `proxima_flavor!`, `Tool`/`ToolCtx`,
//!   relation descriptors, sidecar macros/traits, and registry types. It does
//!   not expose raw `PgPool`, raw storage verbs, or proofless append helpers.
//!   Flavor crates should avoid direct `proxima-core` / `proxima-storage-pg`
//!   dependencies except backend-owned adapters explicitly outside the stable
//!   SDK boundary.

mod app;
mod bundle;
mod config;
mod core_mcp;
pub mod flavor;
pub mod host;
mod migrations;
mod runtime;
mod runtime_config;

pub use host::*;

use std::sync::Arc;

use crate::bundle::FlavorBundle;
use proxima_blob_s3::CitedBlobStore;
use proxima_core::llm::{AnthropicClient, EmbeddingClient};
use proxima_core::{FlavorRegistry, GroupId};
use proxima_storage_pg::{
    PgSidecarRegistry, PgSidecarRegistryFrozen, PgStorage, register_core_pg_sidecars,
};
use sqlx::PgPool;

/// One Owner per embedded host: a Group principal.
///
/// This is the single place embedded hosts construct `Owner`. Since the
/// `Owner = OwnerRef` collapse (S0, Track B), the former org scalar is
/// gone — tenancy is a flavor/app concern, not a substrate one.
#[must_use]
pub fn company_owner(id: uuid::Uuid) -> Owner {
    OwnerRef::Group(GroupId::new(id))
}

/// Persist one host-observed MCP tool call through an embedded engine.
///
/// `authz` is the authenticated context of the served MCP call (the
/// host already holds it from dispatch); the engine authorizes the log
/// Owner against it rather than trusting a caller-supplied Owner.
///
/// # Errors
///
/// Returns `Forbidden` when `authz` cannot access the log Owner, lacks the
/// source-ingest role, or lacks a `memory.write` grant on the owner space;
/// or `Internal` on storage failure.
pub async fn log_mcp_call(
    engine: &Engine,
    authz: &AuthzContext,
    input: McpCallLogInput,
) -> Result<McpCallLogOutcome, ProtocolError> {
    engine.persist_mcp_call(authz, input).await
}

/// Read one Owner's MCP-call activity log through an embedded engine.
/// Owner-scoped, `GraphRead`-gated; `req.actor_oid = Some` narrows to one actor.
///
/// # Errors
///
/// Returns `Forbidden` when `authz` cannot access `req.principal` or lacks
/// graph-read, or `Internal` on storage failure / `limit == 0`.
pub async fn read_mcp_call_history(
    engine: &Engine,
    authz: &AuthzContext,
    req: &McpCallHistoryRequest,
) -> Result<McpCallHistoryResponse, ProtocolError> {
    engine.read_mcp_call_history(authz, req).await
}

type RegisterFn =
    Box<dyn FnOnce(&mut FlavorRegistry) -> Result<(), proxima_core::FlavorRegistryError> + Send>;
type PgSidecarRegisterFn = Box<dyn FnOnce(&mut PgSidecarRegistry) + Send>;

/// Builder for an embedded engine.
pub struct ProximaBuilder {
    config: EmbedConfig,
    owner: Owner,
    registers: Vec<RegisterFn>,
    pg_sidecar_registers: Vec<PgSidecarRegisterFn>,
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
            .field("pg_sidecars", &self.pg_sidecar_registers.len())
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
    pool: PgPool,
    pub registry: Arc<proxima_core::FlavorRegistryFrozen>,
    pub pg_sidecars: Arc<PgSidecarRegistryFrozen>,
    pub blobs: Option<CitedBlobStore>,
    pub owner: Owner,
}

impl EmbeddedProxima {
    /// Test-only backend pool access for integration fixtures.
    #[cfg(any(test, feature = "testkit", debug_assertions))]
    #[must_use]
    pub fn pool_for_tests(&self) -> &PgPool {
        &self.pool
    }
}

impl std::fmt::Debug for EmbeddedProxima {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmbeddedProxima")
            .field("handle", &self.handle)
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
            pg_sidecar_registers: Vec::new(),
            migrators: Vec::new(),
            embed_client: None,
            anthropic: None,
        }
    }

    /// Link one flavor: its `register` fn and optionally its migrator.
    #[must_use]
    pub fn flavor(
        self,
        register: impl FnOnce(&mut FlavorRegistry) -> Result<(), proxima_core::FlavorRegistryError>
        + Send
        + 'static,
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
        register: impl FnOnce(&mut FlavorRegistry) -> Result<(), proxima_core::FlavorRegistryError>
        + Send
        + 'static,
        migrator: Option<sqlx::migrate::Migrator>,
    ) -> Self {
        self.registers.push(Box::new(register));
        self.migrators
            .extend(migrator.map(|migrator| NamedMigrator::new(source, migrator)));
        self
    }

    /// Add a backend-specific PG sidecar registration callback.
    #[must_use]
    pub fn pg_sidecars(
        mut self,
        register: impl FnOnce(&mut PgSidecarRegistry) + Send + 'static,
    ) -> Self {
        self.pg_sidecar_registers.push(Box::new(register));
        self
    }

    /// Link a statically-composed flavor bundle (single flavor or tuple).
    #[must_use]
    pub fn bundle<B: FlavorBundle + 'static>(mut self) -> Self {
        self.registers.push(Box::new(B::register));
        self.pg_sidecar_registers
            .push(Box::new(B::register_pg_sidecars));
        self.migrators.extend(B::migrators());
        self
    }

    /// Embedding client passthrough (`Engine::with_embed`).
    ///
    /// Proxima registers no inference targets or tiers; hosts inject the
    /// embedding/model-seat client used by retrieval.
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
        let Self {
            config,
            owner,
            registers,
            pg_sidecar_registers,
            migrators,
            embed_client,
            anthropic,
        } = self;

        let pg = PgStorage::connect(&config.database_url)
            .await
            .map_err(|e| EmbedError::Storage(e.to_string()))?;
        run_core_and_flavor_migrations(&pg, migrators)
            .await
            .map_err(|e| EmbedError::Storage(e.to_string()))?;

        let mut registry = FlavorRegistry::new();
        for register in registers {
            register(&mut registry).map_err(EmbedError::Registry)?;
        }
        let registry = registry.try_freeze().map_err(EmbedError::Registry)?;

        let mut pg_sidecars = PgSidecarRegistry::new();
        register_core_pg_sidecars(&mut pg_sidecars);
        for register in pg_sidecar_registers {
            register(&mut pg_sidecars);
        }
        let pg_sidecars = pg_sidecars
            .freeze_against(registry.schemas())
            .map_err(|e| EmbedError::Storage(e.to_string()))?;
        let pg_sidecars = Arc::new(pg_sidecars);
        let pg = pg.with_sidecars(pg_sidecars.as_ref().clone());

        let mut engine =
            Engine::new(registry).with_storage_ports(Arc::new(pg.clone()).storage_ports());
        if let Some(client) = embed_client {
            engine = engine.with_embed(client);
        }
        if let Some(client) = anthropic {
            engine = engine.with_anthropic(client);
        }

        let engine = Arc::new(engine);
        let handle = engine
            .clone()
            .start()
            .await
            .map_err(|e| EmbedError::Engine(e.to_string()))?;
        let pool = pg.clone_pool_for_backend();
        let registry = Arc::new(engine.registry().clone());
        let blobs = config.s3.map(|s3| CitedBlobStore::new(pool.clone(), s3));
        Ok(EmbeddedProxima {
            engine,
            handle,
            pool,
            registry,
            pg_sidecars,
            blobs,
            owner,
        })
    }
}

/// Errors from embedded boot.
#[derive(Debug, thiserror::Error)]
pub enum EmbedError {
    #[error("config: {0}")]
    Config(String),
    #[error("registry: {0}")]
    Registry(#[from] proxima_core::FlavorRegistryError),
    #[error("storage: {0}")]
    Storage(String),
    #[error("engine: {0}")]
    Engine(String),
}

#[cfg(test)]
mod tests {
    use proxima_core::OwnerRef;

    #[test]
    fn company_owner_is_group_scoped() {
        let id = uuid::Uuid::now_v7();
        let owner = super::company_owner(id);
        assert!(matches!(owner, OwnerRef::Group(_)));
    }

    #[test]
    fn proxima_builder_debug_redacts_database_url() {
        let owner = super::company_owner(uuid::Uuid::now_v7());
        let builder = super::ProximaBuilder::new(
            super::EmbedConfig {
                database_url: "postgres://user:secret@localhost/proxima".to_string(),
                s3: None,
            },
            owner,
        );
        let debug = format!("{builder:?}");

        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("secret"));
        assert!(!debug.contains("postgres://user"));
    }
}
