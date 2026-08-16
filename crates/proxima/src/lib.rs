//! Embedded-engine facade: env config -> migrations -> compose -> start.
//!
//! Wraps the blessed `Engine::try_compose` embedding entry point
//! (`proxima_core::engine`) for host binaries. Host wiring template:
//! `apps/proxima-mcp`. Cohabitation contract: core, flavors,
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
#[cfg(feature = "auth-oidc")]
pub mod auth;
mod bundle;
mod config;
mod core_mcp;
pub mod flavor;
pub mod host;
mod migrations;
mod runtime;
mod runtime_config;
mod workers;

pub use host::*;
pub use proxima_core::authz::SystemAuthority;

use std::sync::Arc;

use crate::bundle::FlavorBundle;
use proxima_core::llm::EmbeddingClient;
// `CitedBlobStore` and `GroupId` are not imported here: both are
// re-exported through `host::*`
// above, and a private import of the same name would shadow that
// re-export back out of the public facade (`hidden_glob_reexports`).
use proxima_core::FlavorRegistry;
use proxima_storage_pg::{
    PgSidecarRegistry, PgSidecarRegistryFrozen, PgStorage, register_core_pg_sidecars,
};
use sqlx::PgPool;

/// One Owner per embedded host: a Group principal.
///
/// This is the single place embedded hosts construct `Owner`. Since the
/// `Owner = OwnerRef` collapse removed the org scalar from Core; the
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
/// Returns `Forbidden` when `authz` cannot access the log Owner or lacks an
/// `Ingest`/write-capable owner role for that Owner; or `Internal` on storage
/// failure.
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
/// Returns `Forbidden` when `authz` cannot access `req.owner` or lacks
/// graph-read, or `Internal` on storage failure / `limit == 0`.
pub async fn read_mcp_call_history(
    engine: &Engine,
    authz: &AuthzContext,
    req: &McpCallHistoryRequest,
) -> Result<McpCallHistoryResponse, ProtocolError> {
    engine.read_mcp_call_history(authz, req).await
}

/// Load one owner-scoped opaque source cursor through an embedded engine.
///
/// Cursor state is projector write-state; the engine gates this read through
/// owner `Ingest` authorization before touching storage.
///
/// # Errors
///
/// Returns `Forbidden` when `authz` cannot write `owner` with `Ingest`, or
/// `Internal` on storage failure.
pub async fn load_source_cursor(
    engine: &Engine,
    authz: &AuthzContext,
    owner: &Owner,
    source: &str,
) -> Result<Option<Cursor>, ProtocolError> {
    engine.load_source_cursor(authz, owner, source).await
}

/// Store one owner-scoped opaque source cursor through an embedded engine.
///
/// # Errors
///
/// Returns `Forbidden` when `authz` cannot write `owner` with `Ingest`, or
/// `Internal` on storage failure.
pub async fn store_source_cursor(
    engine: &Engine,
    authz: &AuthzContext,
    owner: &Owner,
    source: &str,
    cursor: &Cursor,
) -> Result<(), ProtocolError> {
    engine
        .store_source_cursor(authz, owner, source, cursor)
        .await
}

type RegisterFn =
    Box<dyn FnOnce(&mut FlavorRegistry) -> Result<(), proxima_core::FlavorRegistryError> + Send>;
type PgSidecarRegisterFn = Box<dyn FnOnce(&mut PgSidecarRegistry) + Send>;

/// Builder for an embedded engine.
pub struct ProximaBuilder {
    config: EmbedConfig,
    owner: Option<Owner>,
    registers: Vec<RegisterFn>,
    pg_sidecar_registers: Vec<PgSidecarRegisterFn>,
    migrators: Vec<NamedMigrator>,
    skip_migrations: bool,
    embed_client: Option<Arc<dyn EmbeddingClient>>,
    deployment_tool_scope: Option<proxima_core::ToolScope>,
    pg_tuning: Option<proxima_storage_pg::PgTuning>,
}

impl std::fmt::Debug for ProximaBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProximaBuilder")
            .field("config", &self.config)
            .field("owner", &self.owner)
            .field("flavors", &self.registers.len())
            .field("pg_sidecars", &self.pg_sidecar_registers.len())
            .field("migrators", &self.migrators.len())
            .field("skip_migrations", &self.skip_migrations)
            .field("has_embed_client", &self.embed_client.is_some())
            .field("deployment_tool_scope", &self.deployment_tool_scope)
            .field("pg_tuning", &self.pg_tuning)
            .finish()
    }
}

/// A booted embedded engine plus its companion handles.
pub struct EmbeddedProxima {
    pub engine: Arc<Engine>,
    pub system_authority: SystemAuthority,
    delegation_runtime_authority: proxima_core::DelegationRuntimeAuthority,
    pub handle: EngineHandle,
    pool: PgPool,
    pub registry: Arc<proxima_core::FlavorRegistryFrozen>,
    pub pg_sidecars: Arc<PgSidecarRegistryFrozen>,
    pub blobs: Option<CitedBlobStore>,
    pub owner: Option<Owner>,
}

impl EmbeddedProxima {
    #[must_use]
    pub const fn system_authority(&self) -> &SystemAuthority {
        &self.system_authority
    }

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
        Self::new_optional(config, Some(owner))
    }

    #[must_use]
    pub(crate) fn new_optional(config: EmbedConfig, owner: Option<Owner>) -> Self {
        Self {
            config,
            owner,
            registers: Vec::new(),
            pg_sidecar_registers: Vec::new(),
            migrators: Vec::new(),
            skip_migrations: false,
            embed_client: None,
            deployment_tool_scope: None,
            pg_tuning: None,
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

    /// Boot without applying migrations, keeping only the pre-boot
    /// compatibility preflight.
    ///
    /// For split-role `GitOps` deploys (docs/15): migrate out-of-band under a
    /// DDL role (init container / `tools/dev-migrate`), then run the app under
    /// a DML-only role that cannot issue DDL. A stale pre-v0.0.4 database is
    /// still rejected; the schema is assumed already current.
    #[must_use]
    pub fn skip_migrations(mut self) -> Self {
        self.skip_migrations = true;
        self
    }

    /// Embedding client passthrough (`Engine::with_embed`).
    ///
    /// Proxima registers no inference targets or tiers; hosts inject the
    /// embedding client used by retrieval.
    #[must_use]
    pub fn embed_client(mut self, client: Arc<dyn EmbeddingClient>) -> Self {
        self.embed_client = Some(client);
        self
    }

    /// Deployment tool-surface profile passthrough
    /// (`Engine::with_deployment_tool_scope`). The runtime facade forwards
    /// its required `tool_scope` here so engine chokepoints enforce the
    /// deployment surface even for Host-API callers whose `AuthzContext`
    /// carries `ToolScope::All`.
    #[must_use]
    pub fn deployment_tool_scope(mut self, scope: proxima_core::ToolScope) -> Self {
        self.deployment_tool_scope = Some(scope);
        self
    }

    /// Storage tuning passthrough. Unset, the `PROXIMA_PG_*` environment
    /// decides, and its defaults are this release's shipped behaviour.
    #[must_use]
    pub fn pg_tuning(mut self, tuning: proxima_storage_pg::PgTuning) -> Self {
        self.pg_tuning = Some(tuning);
        self
    }

    /// Connect, migrate, compose, and start the embedded engine.
    ///
    /// # Errors
    ///
    /// Returns `EmbedError::Storage` for connection or migration
    /// failures, `EmbedError::V004ResetRequired` when the target database
    /// does not match `0001_v008.sql` (see `docs/how-to/migrations.md`), and
    /// `EmbedError::Engine` when engine startup fails.
    pub async fn boot(self) -> Result<EmbeddedProxima, EmbedError> {
        let Self {
            config,
            owner,
            registers,
            pg_sidecar_registers,
            migrators,
            skip_migrations,
            embed_client,
            deployment_tool_scope,
            pg_tuning,
        } = self;

        let pg_tuning = match pg_tuning {
            Some(tuning) => tuning,
            None => proxima_storage_pg::PgTuning::from_env().map_err(embed_storage_error)?,
        };
        let pg = PgStorage::connect_with_tuning(&config.database_url, pg_tuning)
            .await
            .map_err(embed_storage_error)?;
        if skip_migrations {
            // GitOps split-role deploy: schema is migrated out-of-band under a
            // DDL role; here we only run the preflight and issue no DDL.
            preflight_without_migrations(&pg, migrators)
                .await
                .map_err(embed_migration_error)?;
        } else {
            run_core_and_flavor_migrations(&pg, migrators)
                .await
                .map_err(embed_migration_error)?;
        }

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
            .map_err(embed_storage_error)?;
        let pg_sidecars = Arc::new(pg_sidecars);
        let pg = pg.with_sidecars(pg_sidecars.as_ref().clone());

        let pool = pg.clone_pool_for_backend();
        let blobs = config
            .s3
            .map(|s3| CitedBlobStore::new(pool.clone(), s3))
            .transpose()
            .map_err(|error| EmbedError::Config(error.to_string()))?;
        let pg = match &blobs {
            Some(store) => pg.with_cold(Arc::new(store.cold_store())),
            None => pg,
        };

        let mut engine =
            Engine::new(registry).with_storage_ports(Arc::new(pg.clone()).storage_ports());
        if let Some(scope) = deployment_tool_scope {
            engine = engine.with_deployment_tool_scope(scope);
        }
        if let Some(store) = &blobs {
            // In-band Art. 17 owner erasure: register the blob backend so
            // owner-scope compliance erase purges the owner's S3 objects
            // (uploaded OCR docs, etc.), not just the Postgres rows. Optional —
            // no S3 configured ⇒ no port ⇒ erase behaves as rows-only.
            engine = engine.with_cited_object_erase(Arc::new(store.clone()));
        }
        if let Some(client) = embed_client {
            // Fail fast on a dimension mismatch. The `embeddings.embedding`
            // column is a fixed-width `vector(EMBEDDING_DIM)`; a client of a
            // different dim (e.g. a 3072-d model) would let every job be
            // claimed and then rejected at insert, silently burning the queue.
            let dim = client.dim();
            if dim != proxima_core::llm::EMBEDDING_DIM {
                return Err(EmbedError::Config(format!(
                    "embedding client reports dim {dim}, but the vector column is fixed at {} \
                     (proxima_core::llm::EMBEDDING_DIM); a mismatched model fails every \
                     embedding job — configure a {}-dimensional embedding model",
                    proxima_core::llm::EMBEDDING_DIM,
                    proxima_core::llm::EMBEDDING_DIM,
                )));
            }
            engine = engine.with_embed(client);
        }

        let (engine, system_authority, delegation_runtime_authority) =
            engine.into_runtime_authorities();
        if let Some(store) = &blobs {
            store
                .bind_system_authority(&system_authority)
                .map_err(|error| EmbedError::Config(error.to_string()))?;
        }
        let engine = Arc::new(engine);
        let handle = engine
            .clone()
            .start()
            .await
            .map_err(|e| EmbedError::Engine(e.to_string()))?;
        let registry = Arc::new(engine.registry().clone());
        Ok(EmbeddedProxima {
            engine,
            system_authority,
            delegation_runtime_authority,
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
    /// The target database does not match this binary's schema
    /// (`0001_v008.sql`) and must be reset before boot.
    #[error("database schema does not match this binary; reset required (see docs/how-to/migrations.md): {details}")]
    V004ResetRequired { details: String },
}

/// Map a storage error onto [`EmbedError`], preserving the typed
/// [`proxima_core::StorageError::V004ResetRequired`] signal instead of
/// collapsing it into the generic [`EmbedError::Storage`] string.
fn embed_storage_error(err: proxima_core::StorageError) -> EmbedError {
    match err {
        proxima_core::StorageError::V004ResetRequired { details } => {
            EmbedError::V004ResetRequired { details }
        }
        other => EmbedError::Storage(other.to_string()),
    }
}

/// Map a migration-facade error onto [`EmbedError`], unwrapping the core
/// preflight check so a stale pre-v0.0.4 database still surfaces as
/// [`EmbedError::V004ResetRequired`] through `boot()` rather than a generic
/// storage string.
fn embed_migration_error(err: MigrationError) -> EmbedError {
    match err {
        MigrationError::CorePreflight(storage_err) => embed_storage_error(storage_err),
        other => EmbedError::Storage(other.to_string()),
    }
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
