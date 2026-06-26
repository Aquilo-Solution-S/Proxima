//! Embedded-engine facade: env config -> migrations -> compose -> start.
//!
//! Wraps the blessed `Engine::compose` embedding entry point
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
//! - **Extension API (flavor authors / advanced hosts):** the re-exports of
//!   `proxima-core` payload traits + ids and of `proxima-storage-pg` typed
//!   verbs and sidecar generators (e.g. [`pg_sidecar`], [`PgMemorySidecar`],
//!   [`ingest_fact`], [`append_edge`], [`append_derived_in_tx`],
//!   [`proxima_flavor`], [`CitationSpec`]). These are deliberately surfaced
//!   through the facade so a flavor or host can build typed sidecars, citations,
//!   and ingest paths while **reducing or avoiding direct dependencies on
//!   `proxima-core` / `proxima-storage-pg`**. They are part of the framework's
//!   supported surface, not internal leakage; treat them as lower-level and
//!   longer-to-stabilize than the host entry point. (`examples/embedded-minimal`
//!   imports a few core types directly for illustration; hosts may instead route
//!   those through these re-exports.)

mod app;
mod bundle;
mod config;
mod core_mcp;
mod migrations;
mod runtime;
mod runtime_config;

pub use app::{AppContext, AppInfo, Authz, FlavorApp};
pub use bundle::FlavorBundle;
pub use config::EmbedConfig;
pub use core_mcp::{CoreMcpError, CoreMcpErrorKind, CoreMcpTools, CoreToolInfo};
pub use migrations::{
    MigrationError, MigrationRunReport, NamedMigrator, run_core_and_flavor_migrations,
};
pub use proxima_core::error::ProtocolError;
pub use proxima_core::llm;
pub use proxima_core::storage::NoopStorage;
pub use proxima_core::verbs::event_ingest::{
    AuthorizedCitationAttachment, CitationSpec, EventDraft, EventIngestOutcome,
    InlineCitationMappingDraft, InlineCitedObjectDraft,
};
pub use proxima_core::verbs::goal_write::{
    GoalAuthorship, GoalCreateRequest, GoalEvidenceRef, GoalPayloadWrite, GoalState,
    GoalWriteBuildError, GoalWriteOutcome, IdempotencyKey, MAX_GOAL_TEXT_CHARS,
    MAX_GOAL_TITLE_CHARS, OperatorKind, SystemOrigin,
};
pub use proxima_core::verbs::mcp_call_history::{
    MAX_MCP_CALL_HISTORY_LIMIT, McpCallHistoryRequest, McpCallHistoryResponse, McpCallRecord,
};
pub use proxima_core::verbs::query::{
    EdgeExistsRequest, EdgeExistsResponse, EdgeFilter, EdgeReadRequest, EdgeReadResponse, EdgeRow,
    FactCitationReadback, MemoryLineageDirection, MemoryLineageEdge, MemoryLineageNode,
    MemoryLineageRequest, MemoryLineageResponse, MemoryRow, PersonalityRootFilter, QueryRequest,
    QueryResponse, SupersessionStatus, TombstoneFilter,
};
pub use proxima_core::verbs::schema::PayloadKind;
pub use proxima_core::{
    AbstractionPayload, AuthPath, AuthzContext, CapabilitySet, CitationMappingPayload,
    CitedObjectPayload, Engine, EngineHandle, FactPayload, FlavorRegistry, GoalPayload, GroupId,
    Identity, McpCallLogInput, McpCallLogOutcome, MemoryId, ModelId, OperatorId, Owner,
    PersonalityInstanceId, PerspectivePayload, Principal, PromptVersion, Role, RoleSet, SchemaId,
    SchemaVersion, SearchProjection, SearchProjectionColumnKind, SearchProjectionField,
    SidecarPayload, SourceBatchId, SourceId, StorageError, ToolId, ToolScope, UserId,
    canonical_json_bytes, provider_safe_tool_name, proxima_flavor,
};
pub use proxima_core::{
    AuthorDerivedEdgeInput, AuthorDerivedOutcome, AuthorDerivedRequestInput, AuthorshipKindMask,
    EdgeAuthorshipKind, EdgeId, EndpointBinding, EntityKind, EntityKindMask, EntityRef,
    FactEntityId, FlavorRegistryFrozen, GoalId, McpTool, McpToolCtx, McpToolError,
    McpToolErrorKind, MemoryOperatorKind, PayloadKeyBuilder, RegisteredRelation, RelationClass,
    RelationDescriptor, SchemaRef,
};
#[cfg(feature = "openai-compat-embed")]
pub use proxima_llm_openai_compat::{
    MISTRAL_EMBED_BASE_URL, MISTRAL_EMBED_MODEL, OpenAiCompatConfig, OpenAiCompatEmbeddingClient,
};
pub use proxima_mcp_server::selfdoc::{build_instructions, how_to_markdown};
pub use proxima_mcp_server::{McpAuthContext, ResourceServerMetadata};
#[cfg(feature = "testkit")]
pub use proxima_pg_testkit as testkit;
/// Declarative PG sidecar generator for flavor-owned typed sidecar tables.
pub use proxima_storage_pg::pg_sidecar;
pub use proxima_storage_pg::query::fact_entity_id_for;
pub use proxima_storage_pg::sidecars::{
    PgCitationMappingSidecar, PgCitedObjectSidecar, PgEdgeSidecar, PgGoalSidecar, PgMemoryPayload,
    PgMemoryPayloadFuture, PgMemorySidecar, PgSidecarFuture,
};
pub use proxima_storage_pg::verbs::derive_append::{
    DerivedDraft, DerivedOutcome, append_derived_in_tx,
};
pub use proxima_storage_pg::verbs::edge_append::{
    EdgeDraft, Endpoint, append_edge, append_edge_in_tx,
};
pub use proxima_storage_pg::verbs::event_ingest::{
    AttachCitationOutcome, attach_citation_in_tx, ingest_fact, ingest_fact_in_tx,
    ingest_fact_with_citation_atomic, ingest_fact_with_citation_in_tx,
};
pub use proxima_storage_pg::verbs::fact_embeddings::{
    list_facts_missing_embedding, load_fact_text, load_fact_text_in_tx, upsert_fact_embedding,
};
pub use proxima_storage_pg::{
    PgSidecarKey, PgSidecarRegistry, PgSidecarRegistryFrozen, register_core_pg_sidecars,
};
pub use runtime::{
    BuiltProxima, Proxima, RunningProxima, layered_router, layered_router_with_revalidation, run,
};
pub use runtime_config::{McpSettings, ProximaError, RuntimeBuilder, RuntimeConfig, RuntimeParts};

use std::sync::Arc;

use proxima_blob_s3::CitedBlobStore;
use proxima_core::llm::{AnthropicClient, EmbeddingClient};
use proxima_storage_pg::PgStorage;
use sqlx::PgPool;

/// One Owner per embedded host: a Group principal.
///
/// This is the single place embedded hosts construct `Owner`. Since the
/// `Owner = Principal` collapse (S0, Track B), the former org scalar is
/// gone — tenancy is a flavor/app concern, not a substrate one.
#[must_use]
pub fn company_owner(id: uuid::Uuid) -> Owner {
    Principal::Group(GroupId::new(id))
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

type RegisterFn = Box<dyn FnOnce(&mut FlavorRegistry) + Send>;
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
    pub pool: PgPool,
    pub registry: Arc<proxima_core::FlavorRegistryFrozen>,
    pub pg_sidecars: Arc<PgSidecarRegistryFrozen>,
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
            register(&mut registry);
        }
        let registry = registry.freeze();

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

        let mut engine = Engine::new(registry).with_storage(Arc::new(pg.clone()));
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
        let pool = pg.pool().clone();
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
    #[error("storage: {0}")]
    Storage(String),
    #[error("engine: {0}")]
    Engine(String),
}

#[cfg(test)]
mod tests {
    use proxima_core::Principal;

    #[test]
    fn company_owner_is_group_scoped() {
        let id = uuid::Uuid::now_v7();
        let owner = super::company_owner(id);
        assert!(matches!(owner, Principal::Group(_)));
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
