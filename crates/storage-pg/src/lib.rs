//! Postgres `Storage` impl.
//!
//! See docs/07-storage.md and the `Storage` trait in
//! `proxima_core`.
//!
//! The verb logic lives under [`verbs`]; this module wires the
//! `PgStorage` struct, connection lifecycle, and migration runner,
//! then delegates each `Storage` trait method to its per-verb
//! implementation.
#[cfg(feature = "test-fixtures")]
extern crate self as proxima_storage_pg;

use std::sync::Arc;
use std::time::Duration;

use proxima_core::SidecarPayload;
use proxima_core::access::{
    AccessGrantRow, EntryAccessFacts, GrantResource, GrantSelector, NewAccessGrant,
    RemoveOwnerOutcome, Visibility,
};
use proxima_core::personality::{
    AbstractionRow, ActiveGoalSummary, ChangeEventForWake, InstantiatePersonalityRequest,
    InstantiatePersonalityResponse, ListReadScopeRequest, ListReadScopeResponse, MemorySnapshot,
    PersonalityInstanceId, PersonalityInstanceRow, PersonalityRef, PersonalityWriteOutcome,
    PersonalityWriteRequest, SetReadScopeRequest, SetReadScopeResponse, SetWakeEntriesRequest,
    SetWakeEntriesResponse, SidecarSpec, TombstonePersonalityRequest, TombstonePersonalityResponse,
};
use proxima_core::verbs::close_batch::CloseBatchOutcome;
use proxima_core::verbs::event_history::{EventHistoryRequest, EventHistoryResponse};
use proxima_core::verbs::event_ingest::{
    AuthorizedEventIngest, AuthorizedFactWithCitation, EventDraft, EventIngestOutcome,
};
use proxima_core::verbs::fact_cleanup::{CleanupDueFactsOutcome, TombstoneFactOutcome};
use proxima_core::verbs::goal_write::{
    AchieveGoalAtomicRequest, CreateGoalAtomicRequest, DecomposeGoalAtomicRequest,
    DecomposeGoalOutcome, GoalWriteOutcome, ModifyGoalAtomicRequest, TransitionGoalAtomicRequest,
};
use proxima_core::verbs::mcp_call_history::{McpCallHistoryRequest, McpCallHistoryResponse};
use proxima_core::verbs::persist_mcp_call::{McpCallLogInput, McpCallLogOutcome};
use proxima_core::verbs::query::{
    EdgeExistsRequest, EdgeExistsResponse, EdgeReadRequest, EdgeReadResponse, FactCitationReadback,
    MemoryLineageRequest, MemoryLineageResponse, MemorySearchRequest, MemorySearchResult,
    QueryRequest, QueryResponse,
};
use proxima_core::{
    AuthorDerivedOutcome, AuthorDerivedRequest, DerivedEdgeSpec, EdgeEndpointKindRow, EdgeId,
    EmbeddingJobClaim, FactEntityId, MasterTokenPersonality, MemoryDependency,
    MemoryGraphPayloadRow, MemoryId, MemoryKindRow, NeighborEdgeRow, Owner, Principal, SchemaId,
    SchemaVersion, SourceBatchId, Storage, StorageError, StorageHandle,
};
use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
pub use verbs::fact_embeddings::{
    EmbeddingInlineDrainOutcome, EmbeddingReconcileOptions, EmbeddingReconcileOutcome,
    EmbeddingReconcileScope,
};

use crate::error::internal;

mod authorship;
mod change_event;
mod error;
mod pg_ident;
mod pgvector;
pub mod sidecars;
pub mod query {
    pub use crate::verbs::query::{MAX_SNAPSHOT_EDGES, fact_entity_id_for};
}
#[cfg(feature = "test-fixtures")]
pub mod test_fixtures;
pub mod verbs;
pub use sidecars::{
    PgSidecarKey, PgSidecarRegistry, PgSidecarRegistryFrozen, core_pg_sidecars,
    register_core_pg_sidecars,
};

/// Default DB URL when `DATABASE_URL` is unset. Matches the
/// dev DB created locally via `createdb proxima_dev`.
pub const DEFAULT_DATABASE_URL: &str = "postgres://postgres@localhost/proxima_dev";

/// Embedded core migration set under `crates/storage-pg/migrations/`.
///
/// `ignore_missing = true` is load-bearing when the same database also
/// records flavor migrations in `SQLx`'s default `_sqlx_migrations` table.
#[must_use]
pub fn core_migrator() -> sqlx::migrate::Migrator {
    let mut migrator = sqlx::migrate!("./migrations");
    migrator.set_ignore_missing(true);
    migrator
}

#[derive(Debug, Clone)]
pub struct PgStorage {
    pool: PgPool,
    sidecars: PgSidecarRegistryFrozen,
}

impl PgStorage {
    /// Connect using `url`, build a tuned pool, and verify
    /// connectivity by acquiring one connection.
    ///
    /// # Errors
    ///
    /// Returns `StorageError::Unavailable` on connection or
    /// query failure.
    pub async fn connect(url: &str) -> Result<Self, StorageError> {
        let opts: PgConnectOptions = url.parse().map_err(|e: sqlx::Error| {
            StorageError::Unavailable(format!("invalid DATABASE_URL: {e}"))
        })?;
        let pool = PgPoolOptions::new()
            .max_connections(10)
            .acquire_timeout(Duration::from_secs(5))
            .connect_with(opts)
            .await
            .map_err(|e| StorageError::Unavailable(e.to_string()))?;

        // Validate connectivity with a trivial query.
        sqlx::query!("SELECT 1 AS one")
            .fetch_one(&pool)
            .await
            .map_err(|e| StorageError::Unavailable(e.to_string()))?;

        Ok(Self {
            pool,
            sidecars: core_pg_sidecars(),
        })
    }

    /// Read `DATABASE_URL` from env, fallback to
    /// `DEFAULT_DATABASE_URL`. Convenience for the bin / dev.
    #[must_use]
    pub fn url_from_env() -> String {
        std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DATABASE_URL.to_string())
    }

    #[must_use]
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    #[must_use]
    pub fn sidecars(&self) -> &PgSidecarRegistryFrozen {
        &self.sidecars
    }

    /// Replace the entire sidecar registry.
    ///
    /// The caller must include the core sidecars. The boot/facade path
    /// enforces sidecar coverage with `freeze_against`; tests may pass
    /// deliberate partial registries.
    #[must_use]
    pub fn with_sidecars(mut self, sidecars: PgSidecarRegistryFrozen) -> Self {
        self.sidecars = sidecars;
        self
    }

    #[must_use]
    pub fn into_handle(self) -> StorageHandle {
        Arc::new(self)
    }

    /// Global enqueue-only embedding reconciliation.
    ///
    /// # Errors
    ///
    /// Returns storage errors from the reconciliation query.
    pub async fn reconcile_embeddings(
        &self,
        options: EmbeddingReconcileOptions<'_>,
    ) -> Result<EmbeddingReconcileOutcome, StorageError> {
        verbs::fact_embeddings::reconcile_embeddings(&self.pool, options).await
    }

    /// Inline drain for queued embedding jobs.
    ///
    /// # Errors
    ///
    /// Returns storage errors from claiming or writing jobs/embeddings.
    pub async fn drain_embedding_jobs_inline(
        &self,
        client: &dyn proxima_core::llm::EmbeddingClient,
        limit: i64,
    ) -> Result<EmbeddingInlineDrainOutcome, StorageError> {
        verbs::fact_embeddings::drain_embedding_jobs_inline(&self.pool, client, limit).await
    }

    /// Apply all pending migrations under
    /// `crates/storage-pg/migrations/`. Idempotent — sqlx tracks
    /// applied migrations in `_sqlx_migrations`. Call once
    /// at process start before any verb dispatch.
    ///
    /// `ignore_missing = true` matches the per-flavor migrator
    /// (`flavors/*/migrations.rs`): core and every flavor share the
    /// default `_sqlx_migrations` table, so on a second run the core
    /// migrator sees flavor-authored versions it doesn't know about.
    /// Without this relaxation the second run fails with
    /// `VersionMissing(<flavor version>)`. The core version-set is
    /// still validated; we only relax the cross-author check.
    ///
    /// # Errors
    ///
    /// Returns `StorageError::Internal` on any sqlx
    /// migration failure (broken file, conflict with the
    /// recorded checksum, etc.).
    pub async fn run_migrations(&self) -> Result<(), StorageError> {
        core_migrator().run(&self.pool).await.map_err(internal)?;
        Ok(())
    }
}

fn edge_draft_from_spec<'a>(edge: &'a DerivedEdgeSpec<'a>) -> verbs::edge_append::EdgeDraft<'a> {
    verbs::edge_append::EdgeDraft {
        edge_id: uuid::Uuid::now_v7(),
        relation: edge.relation,
        source_kind: edge.source_kind,
        source_memory_id: Some(edge.source_memory_id.into_inner()),
        source_goal_id: None,
        source_fact_entity_id: None,
        target_kind: edge.target_kind,
        target_memory_id: Some(edge.target_memory_id.into_inner()),
        target_goal_id: None,
        target_fact_entity_id: None,
        authorship_kind: edge.authorship_kind,
        authorship_owner_memory_id: edge.authorship_owner_memory_id.map(MemoryId::into_inner),
        owner: edge.owner,
    }
}

#[async_trait::async_trait]
impl Storage for PgStorage {
    async fn ingest_event_atomic(
        &self,
        draft: &EventDraft,
        embedding_model_id: Option<&str>,
    ) -> Result<EventIngestOutcome, StorageError> {
        verbs::event_ingest::ingest_event_atomic(&self.pool, draft, embedding_model_id).await
    }

    async fn load_fact_text(
        &self,
        owner: &Owner,
        memory_id: MemoryId,
    ) -> Result<Option<String>, StorageError> {
        verbs::fact_embeddings::load_fact_text(&self.pool, owner, memory_id).await
    }

    async fn load_embedding_text(
        &self,
        owner: &Owner,
        entity_kind: proxima_core::EntityKind,
        memory_id: MemoryId,
    ) -> Result<Option<String>, StorageError> {
        verbs::fact_embeddings::load_embedding_text(&self.pool, owner, entity_kind, memory_id).await
    }

    async fn upsert_fact_embedding(
        &self,
        owner: &Owner,
        memory_id: MemoryId,
        model_id: &str,
        dim: usize,
        vec: &[f32],
    ) -> Result<(), StorageError> {
        let mut tx = self.pool.begin().await.map_err(|err| {
            StorageError::Internal(format!("begin Fact embedding upsert tx: {err}"))
        })?;
        verbs::fact_embeddings::upsert_fact_embedding(
            &mut tx, owner, memory_id, model_id, dim, vec,
        )
        .await?;
        tx.commit().await.map_err(crate::error::map_err)
    }

    async fn upsert_memory_embedding(
        &self,
        owner: &Owner,
        entity_kind: proxima_core::EntityKind,
        memory_id: MemoryId,
        model_id: &str,
        dim: usize,
        vec: &[f32],
    ) -> Result<(), StorageError> {
        let mut tx = self.pool.begin().await.map_err(|err| {
            StorageError::Internal(format!("begin memory embedding upsert tx: {err}"))
        })?;
        verbs::fact_embeddings::upsert_memory_embedding(
            &mut tx,
            owner,
            entity_kind,
            memory_id,
            model_id,
            dim,
            vec,
        )
        .await?;
        tx.commit().await.map_err(crate::error::map_err)
    }

    async fn list_facts_missing_embedding(
        &self,
        owner: &Owner,
        model_id: &str,
        limit: usize,
    ) -> Result<Vec<MemoryId>, StorageError> {
        verbs::fact_embeddings::list_facts_missing_embedding(&self.pool, owner, model_id, limit)
            .await
    }

    async fn claim_pending_embedding_jobs(
        &self,
        model_id: &str,
        limit: i64,
    ) -> Result<Vec<EmbeddingJobClaim>, StorageError> {
        verbs::fact_embeddings::claim_pending_embedding_jobs(&self.pool, model_id, limit).await
    }

    async fn complete_embedding_job(&self, claim: &EmbeddingJobClaim) -> Result<(), StorageError> {
        verbs::fact_embeddings::complete_embedding_job(&self.pool, claim).await
    }

    async fn fail_embedding_job(
        &self,
        claim: &EmbeddingJobClaim,
        error: &str,
    ) -> Result<(), StorageError> {
        verbs::fact_embeddings::fail_embedding_job(&self.pool, claim, error).await
    }

    async fn enqueue_missing_embedding_jobs(
        &self,
        owner: &Owner,
        model_id: &str,
        limit: i64,
    ) -> Result<u64, StorageError> {
        verbs::fact_embeddings::enqueue_missing_embedding_jobs(&self.pool, owner, model_id, limit)
            .await
    }

    async fn count_pending_embedding_jobs(&self, owner: &Owner) -> Result<u64, StorageError> {
        verbs::fact_embeddings::count_pending_embedding_jobs(&self.pool, owner).await
    }

    async fn persist_mcp_call_atomic(
        &self,
        input: &McpCallLogInput,
    ) -> Result<McpCallLogOutcome, StorageError> {
        verbs::persist_mcp_call::persist_mcp_call_atomic(&self.pool, input).await
    }

    async fn ingest_event_with_typed_sidecar(
        &self,
        authorized: &AuthorizedEventIngest,
        sidecar_payload: &SidecarPayload,
        embedding_model_id: Option<&str>,
    ) -> Result<EventIngestOutcome, StorageError> {
        let mut tx = self.pool.begin().await.map_err(internal)?;
        let fact_sidecars = self.sidecars.clone();
        let payload = sidecar_payload.clone();
        let outcome = verbs::event_ingest::ingest_event_with_sidecar_in_tx(
            &mut tx,
            authorized,
            embedding_model_id,
            move |tx, outcome| {
                Box::pin(async move {
                    fact_sidecars
                        .insert_memory_sidecar(tx, outcome.memory_id, &payload)
                        .await
                })
            },
        )
        .await?;
        tx.commit().await.map_err(crate::error::map_err)?;
        Ok(outcome)
    }

    async fn ingest_fact_with_citation_and_typed_sidecar(
        &self,
        authorized: &AuthorizedFactWithCitation,
        sidecar_payload: &SidecarPayload,
        embedding_model_id: Option<&str>,
    ) -> Result<EventIngestOutcome, StorageError> {
        let mut tx = self.pool.begin().await.map_err(internal)?;
        let sidecars = self.sidecars.clone();
        let fact_sidecars = sidecars.clone();
        let payload = sidecar_payload.clone();
        let outcome = verbs::event_ingest::ingest_fact_with_citation_in_tx(
            &mut tx,
            &sidecars,
            authorized,
            embedding_model_id,
            move |tx, outcome| {
                Box::pin(async move {
                    fact_sidecars
                        .insert_memory_sidecar(tx, outcome.memory_id, &payload)
                        .await
                })
            },
        )
        .await?;
        tx.commit().await.map_err(crate::error::map_err)?;
        Ok(outcome)
    }

    async fn author_derived(
        &self,
        req: &AuthorDerivedRequest<'_>,
    ) -> Result<AuthorDerivedOutcome, StorageError> {
        let mut tx = self.pool.begin().await.map_err(internal)?;
        let draft = verbs::derive_append::DerivedDraft {
            memory_id: req.memory_id.into_inner(),
            owner: req.owner.clone(),
            kind: req.kind,
            author_personality_instance_id: req.author_personality_instance_id,
            schema_id: req.schema_id.clone(),
            schema_version: req.schema_version,
            text: req.text.clone(),
            operator_kind: req.operator_kind,
            model_id: req.model_id,
            prompt_version: req.prompt_version,
            supersedes: req.supersedes,
            embedding: req.embedding.clone(),
            embedding_model_id: req.embedding_model_id,
        };
        let sidecars = self.sidecars.clone();
        let sidecar_payload = req.sidecar_payload.clone();
        let outcome =
            verbs::derive_append::append_derived_in_tx(&mut tx, &draft, move |tx, outcome| {
                Box::pin(async move {
                    sidecars
                        .insert_memory_sidecar(tx, outcome.memory_id, &sidecar_payload)
                        .await
                })
            })
            .await?;
        let mut edge_count = 0;
        if !outcome.idempotent_replay {
            for edge in req.edges {
                let draft = edge_draft_from_spec(edge);
                if let Some(sidecar_payload) = edge.sidecar_payload {
                    let sidecars = self.sidecars.clone();
                    let payload = sidecar_payload.clone();
                    verbs::edge_append::append_edge_with_sidecar_in_tx(
                        tx.as_mut(),
                        &draft,
                        move |tx, edge_id| {
                            Box::pin(async move {
                                sidecars.insert_edge_sidecar(tx, edge_id, &payload).await
                            })
                        },
                    )
                    .await?;
                } else {
                    verbs::edge_append::append_edge_in_tx(tx.as_mut(), &draft).await?;
                }
                edge_count += 1;
            }
        }
        tx.commit().await.map_err(crate::error::map_err)?;
        Ok(AuthorDerivedOutcome {
            memory_id: outcome.memory_id,
            idempotent_replay: outcome.idempotent_replay,
            edge_count,
        })
    }

    async fn append_memory_edge(&self, edge: &DerivedEdgeSpec<'_>) -> Result<EdgeId, StorageError> {
        let mut tx = self.pool.begin().await.map_err(internal)?;
        let draft = edge_draft_from_spec(edge);
        let edge_id = EdgeId::new(draft.edge_id);
        if let Some(sidecar_payload) = edge.sidecar_payload {
            let sidecars = self.sidecars.clone();
            let payload = sidecar_payload.clone();
            verbs::edge_append::append_edge_with_sidecar_in_tx(
                tx.as_mut(),
                &draft,
                move |tx, edge_id| {
                    Box::pin(
                        async move { sidecars.insert_edge_sidecar(tx, edge_id, &payload).await },
                    )
                },
            )
            .await?;
        } else {
            verbs::edge_append::append_edge_in_tx(tx.as_mut(), &draft).await?;
        }
        tx.commit().await.map_err(crate::error::map_err)?;
        Ok(edge_id)
    }

    async fn load_memory_kinds(
        &self,
        owner: &Owner,
        memory_ids: &[MemoryId],
    ) -> Result<Vec<MemoryKindRow>, StorageError> {
        if memory_ids.is_empty() {
            return Ok(Vec::new());
        }
        let ids = memory_ids
            .iter()
            .copied()
            .map(MemoryId::into_inner)
            .collect::<Vec<_>>();
        let (owner_kind, owner_principal_id) = owner.columns();
        let rows: Vec<(uuid::Uuid, Option<proxima_core::EntityKind>)> = sqlx::query_as(
            "SELECT memory_id, kind
             FROM proxima_core.memories
             WHERE owner_principal_kind = $1
               AND owner_principal_id = $2
               AND memory_id = ANY($3::uuid[])",
        )
        .bind(owner_kind)
        .bind(owner_principal_id)
        .bind(&ids)
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;
        Ok(rows
            .into_iter()
            .map(|(memory_id, kind)| MemoryKindRow {
                memory_id: MemoryId::new(memory_id),
                kind,
            })
            .collect())
    }

    async fn load_memory_graph_payloads(
        &self,
        owner: &Owner,
        memory_ids: &[MemoryId],
        include_body: bool,
    ) -> Result<Vec<MemoryGraphPayloadRow>, StorageError> {
        if memory_ids.is_empty() {
            return Ok(Vec::new());
        }
        let ids = memory_ids
            .iter()
            .copied()
            .map(MemoryId::into_inner)
            .collect::<Vec<_>>();
        let (owner_kind, owner_principal_id) = owner.columns();
        let rows: Vec<(uuid::Uuid, Option<Vec<String>>, Option<String>)> = sqlx::query_as(
            "SELECT m.memory_id,
                    COALESCE(n.tags, d.tags) AS tags,
                    CASE WHEN $4
                         THEN COALESCE(n.body, d.body, m.text)
                         ELSE NULL
                    END AS body
             FROM proxima_core.memories m
             LEFT JOIN proxima_core.agent_note_v1 n USING (memory_id)
             LEFT JOIN proxima_core.agent_derivation_v1 d USING (memory_id)
             WHERE m.owner_principal_kind = $1
               AND m.owner_principal_id = $2
               AND m.memory_id = ANY($3::uuid[])",
        )
        .bind(owner_kind)
        .bind(owner_principal_id)
        .bind(&ids)
        .bind(include_body)
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;
        Ok(rows
            .into_iter()
            .map(|(memory_id, tags, body)| MemoryGraphPayloadRow {
                memory_id: MemoryId::new(memory_id),
                tags,
                body,
            })
            .collect())
    }

    async fn load_neighbor_memory_edges(
        &self,
        owner: &Owner,
        memory_ids: &[MemoryId],
        limit: usize,
    ) -> Result<Vec<NeighborEdgeRow>, StorageError> {
        if memory_ids.is_empty() {
            return Ok(Vec::new());
        }
        let ids = memory_ids
            .iter()
            .copied()
            .map(MemoryId::into_inner)
            .collect::<Vec<_>>();
        let limit = i64::try_from(limit).map_err(|err| StorageError::Internal(err.to_string()))?;
        let (owner_kind, owner_principal_id) = owner.columns();
        let rows: Vec<(
            uuid::Uuid,
            String,
            proxima_core::EntityKind,
            Option<uuid::Uuid>,
            proxima_core::EntityKind,
            Option<uuid::Uuid>,
        )> = sqlx::query_as(
            "SELECT e.edge_id, e.relation,
                    e.source_kind,
                    COALESCE(e.source_memory_id, sfe.current_memory_id) AS source_memory_id,
                    e.target_kind,
                    COALESCE(e.target_memory_id, tfe.current_memory_id) AS target_memory_id
             FROM proxima_core.edges e
             LEFT JOIN proxima_core.fact_entities sfe
               ON sfe.fact_entity_id = e.source_fact_entity_id
              AND sfe.owner_principal_kind = e.owner_principal_kind
              AND sfe.owner_principal_id = e.owner_principal_id
             LEFT JOIN proxima_core.fact_entities tfe
               ON tfe.fact_entity_id = e.target_fact_entity_id
              AND tfe.owner_principal_kind = e.owner_principal_kind
              AND tfe.owner_principal_id = e.owner_principal_id
             WHERE e.owner_principal_kind = $1
               AND e.owner_principal_id = $2
               AND (e.source_memory_id = ANY($3::uuid[])
                    OR e.target_memory_id = ANY($3::uuid[])
                    OR sfe.current_memory_id = ANY($3::uuid[])
                    OR tfe.current_memory_id = ANY($3::uuid[]))
             ORDER BY e.edge_id DESC
             LIMIT $4",
        )
        .bind(owner_kind)
        .bind(owner_principal_id)
        .bind(&ids)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;
        Ok(rows
            .into_iter()
            .map(
                |(
                    edge_id,
                    relation,
                    source_kind,
                    source_memory_id,
                    target_kind,
                    target_memory_id,
                )| {
                    NeighborEdgeRow {
                        edge_id: EdgeId::new(edge_id),
                        relation,
                        source_kind,
                        source_memory_id: source_memory_id.map(MemoryId::new),
                        target_kind,
                        target_memory_id: target_memory_id.map(MemoryId::new),
                    }
                },
            )
            .collect())
    }

    async fn load_memory_edge_ids(
        &self,
        owner: &Owner,
        relation: &str,
        source_memory_id: MemoryId,
        target_memory_ids: &[MemoryId],
    ) -> Result<Vec<EdgeId>, StorageError> {
        if target_memory_ids.is_empty() {
            return Ok(Vec::new());
        }
        let target_ids = target_memory_ids
            .iter()
            .copied()
            .map(MemoryId::into_inner)
            .collect::<Vec<_>>();
        let (owner_kind, owner_principal_id) = owner.columns();
        let rows: Vec<uuid::Uuid> = sqlx::query_scalar(
            "SELECT edge_id
             FROM proxima_core.edges
             WHERE owner_principal_kind = $1
               AND owner_principal_id = $2
               AND relation = $3
               AND source_memory_id = $4
               AND target_memory_id = ANY($5::uuid[])
             ORDER BY edge_id DESC",
        )
        .bind(owner_kind)
        .bind(owner_principal_id)
        .bind(relation)
        .bind(source_memory_id.into_inner())
        .bind(&target_ids)
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;
        Ok(rows.into_iter().map(EdgeId::new).collect())
    }

    async fn load_edge_endpoint_kinds(
        &self,
        edge_ids: &[EdgeId],
    ) -> Result<Vec<EdgeEndpointKindRow>, StorageError> {
        if edge_ids.is_empty() {
            return Ok(Vec::new());
        }
        let ids = edge_ids
            .iter()
            .copied()
            .map(EdgeId::into_inner)
            .collect::<Vec<_>>();
        let rows: Vec<(
            uuid::Uuid,
            proxima_core::EntityKind,
            proxima_core::EntityKind,
        )> = sqlx::query_as(
            "SELECT edge_id, source_kind, target_kind
                 FROM proxima_core.edges
                 WHERE edge_id = ANY($1::uuid[])",
        )
        .bind(&ids)
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;
        Ok(rows
            .into_iter()
            .map(|(edge_id, source_kind, target_kind)| EdgeEndpointKindRow {
                edge_id: EdgeId::new(edge_id),
                source_kind,
                target_kind,
            })
            .collect())
    }

    async fn active_personality_root(
        &self,
        owner: &Owner,
        instance_id: PersonalityInstanceId,
    ) -> Result<Option<MemoryId>, StorageError> {
        let (owner_kind, owner_id) = owner.columns();
        let row: Option<(uuid::Uuid,)> = sqlx::query_as(
            "SELECT current_root_perspective_memory_id
             FROM proxima_core.personality
             WHERE owner_principal_kind = $1
               AND owner_principal_id = $2
               AND personality_instance_id = $3
               AND status <> 'tombstoned'::proxima_core.personality_status",
        )
        .bind(owner_kind)
        .bind(owner_id)
        .bind(instance_id.into_inner())
        .fetch_optional(&self.pool)
        .await
        .map_err(internal)?;
        Ok(row.map(|(memory_id,)| MemoryId::new(memory_id)))
    }

    async fn create_goal_atomic(
        &self,
        req: &CreateGoalAtomicRequest<'_>,
    ) -> Result<GoalWriteOutcome, StorageError> {
        verbs::goal_write::create_goal_atomic(&self.pool, &self.sidecars, req).await
    }

    async fn transition_goal_atomic(
        &self,
        req: &TransitionGoalAtomicRequest<'_>,
    ) -> Result<GoalWriteOutcome, StorageError> {
        verbs::goal_write::transition_goal_atomic(&self.pool, &self.sidecars, req).await
    }

    async fn achieve_goal_atomic(
        &self,
        req: &AchieveGoalAtomicRequest<'_>,
    ) -> Result<GoalWriteOutcome, StorageError> {
        verbs::goal_write::achieve_goal_atomic(&self.pool, &self.sidecars, req).await
    }

    async fn modify_goal_atomic(
        &self,
        req: &ModifyGoalAtomicRequest<'_>,
    ) -> Result<GoalWriteOutcome, StorageError> {
        verbs::goal_write::modify_goal_atomic(&self.pool, &self.sidecars, req).await
    }

    async fn decompose_goal_atomic(
        &self,
        req: &DecomposeGoalAtomicRequest<'_>,
    ) -> Result<DecomposeGoalOutcome, StorageError> {
        verbs::goal_write::decompose_goal_atomic(&self.pool, &self.sidecars, req).await
    }

    async fn event_history(
        &self,
        req: &EventHistoryRequest,
    ) -> Result<EventHistoryResponse, StorageError> {
        verbs::event_history::event_history(&self.pool, req).await
    }

    async fn read_mcp_call_history(
        &self,
        req: &McpCallHistoryRequest,
    ) -> Result<McpCallHistoryResponse, StorageError> {
        verbs::mcp_call_history::read_mcp_call_history(&self.pool, req).await
    }

    async fn query_memories(
        &self,
        req: &QueryRequest,
        schemas: &[proxima_core::verbs::schema::SchemaInfo],
    ) -> Result<QueryResponse, StorageError> {
        verbs::query::query_memories(&self.pool, &self.sidecars, req, schemas).await
    }

    async fn read_edges(&self, req: &EdgeReadRequest) -> Result<EdgeReadResponse, StorageError> {
        verbs::query::read_edges(&self.pool, req).await
    }

    async fn edge_exists(
        &self,
        req: &EdgeExistsRequest,
    ) -> Result<EdgeExistsResponse, StorageError> {
        verbs::query::edge_exists(&self.pool, req).await
    }

    async fn search_memories(
        &self,
        req: &MemorySearchRequest,
        projections: &[proxima_core::verbs::schema::MemorySearchProjection],
    ) -> Result<Vec<MemorySearchResult>, StorageError> {
        verbs::query::search_memories(&self.pool, req, projections).await
    }

    async fn fact_entity_id_for(
        &self,
        owner: &Owner,
        schema_id: &SchemaId,
        schema_version: SchemaVersion,
        natural_key: &[String],
    ) -> Result<Option<FactEntityId>, StorageError> {
        verbs::query::fact_entity_id_for_pool(
            &self.pool,
            owner,
            schema_id,
            schema_version,
            natural_key,
        )
        .await
    }

    async fn facts_citing_object(
        &self,
        owner: &Owner,
        cited_object_id: uuid::Uuid,
        sidecars: &[SidecarSpec],
    ) -> Result<Vec<MemorySnapshot>, StorageError> {
        verbs::query::facts_citing_object(
            &self.pool,
            &self.sidecars,
            owner,
            cited_object_id,
            sidecars,
        )
        .await
    }

    async fn citation_of_fact(
        &self,
        owner: &Owner,
        fact_memory_id: MemoryId,
    ) -> Result<Option<FactCitationReadback>, StorageError> {
        verbs::query::citation_of_fact(&self.pool, owner, fact_memory_id).await
    }

    async fn citation_of_entity_head(
        &self,
        owner: &Owner,
        fact_entity_id: FactEntityId,
    ) -> Result<Option<FactCitationReadback>, StorageError> {
        verbs::query::citation_of_entity_head(&self.pool, owner, fact_entity_id).await
    }

    async fn walk_memory_lineage(
        &self,
        req: &MemoryLineageRequest,
    ) -> Result<MemoryLineageResponse, StorageError> {
        verbs::query::walk_memory_lineage(&self.pool, req).await
    }

    async fn list_active_goals(
        &self,
        principal: &Principal,
        self_perspective_memory_id: MemoryId,
        limit: usize,
    ) -> Result<Vec<ActiveGoalSummary>, StorageError> {
        verbs::active_goals::list_active_goals(
            &self.pool,
            principal,
            self_perspective_memory_id,
            limit,
        )
        .await
    }

    async fn close_batch(
        &self,
        principal: &Principal,
        source_batch_id: SourceBatchId,
    ) -> Result<CloseBatchOutcome, StorageError> {
        verbs::close_batch::close_batch(&self.pool, principal, source_batch_id).await
    }

    async fn list_personality_instances(
        &self,
        owner: &Owner,
        include_tombstoned: bool,
    ) -> Result<Vec<PersonalityInstanceRow>, StorageError> {
        verbs::consolidate::list_personality_instances(&self.pool, owner, include_tombstoned).await
    }

    async fn tombstone_personality(
        &self,
        req: &TombstonePersonalityRequest,
    ) -> Result<TombstonePersonalityResponse, StorageError> {
        verbs::consolidate::tombstone_personality(&self.pool, req).await
    }

    async fn instantiate_personality(
        &self,
        req: &InstantiatePersonalityRequest,
    ) -> Result<InstantiatePersonalityResponse, StorageError> {
        verbs::consolidate::instantiate_personality(&self.pool, req).await
    }

    async fn ensure_master_token_personality(
        &self,
        owner: &Owner,
        master_token_id: uuid::Uuid,
    ) -> Result<MasterTokenPersonality, StorageError> {
        verbs::master_token_personality::ensure_master_token_personality(
            &self.pool,
            owner,
            master_token_id,
        )
        .await
    }

    async fn ensure_subject_personality(
        &self,
        owner: &Owner,
        subject: &Principal,
    ) -> Result<MasterTokenPersonality, StorageError> {
        verbs::subject_personality::ensure_subject_personality(&self.pool, owner, subject).await
    }

    async fn set_wake_entries(
        &self,
        req: &SetWakeEntriesRequest,
    ) -> Result<SetWakeEntriesResponse, StorageError> {
        verbs::consolidate::set_wake_entries(&self.pool, req).await
    }

    async fn set_wake_entries_within(
        &self,
        owner: &Owner,
        personality_instance_id: PersonalityInstanceId,
        mutate: proxima_core::WakeEntriesMutator,
    ) -> Result<SetWakeEntriesResponse, StorageError> {
        verbs::consolidate::set_wake_entries_within(
            &self.pool,
            owner,
            personality_instance_id,
            mutate,
        )
        .await
    }

    async fn list_read_scope(
        &self,
        req: &ListReadScopeRequest,
    ) -> Result<ListReadScopeResponse, StorageError> {
        verbs::consolidate::list_read_scope(&self.pool, req).await
    }

    async fn set_read_scope(
        &self,
        req: &SetReadScopeRequest,
    ) -> Result<SetReadScopeResponse, StorageError> {
        verbs::consolidate::set_read_scope(&self.pool, req).await
    }

    async fn upsert_fact_retention(&self, owner: &Owner, seconds: i64) -> Result<(), StorageError> {
        verbs::fact_retention::upsert_fact_retention(&self.pool, owner, seconds).await
    }

    async fn get_fact_retention(&self, owner: &Owner) -> Result<Option<i64>, StorageError> {
        verbs::fact_retention::get_fact_retention(&self.pool, owner).await
    }

    async fn clear_fact_retention(&self, owner: &Owner) -> Result<bool, StorageError> {
        verbs::fact_retention::clear_fact_retention(&self.pool, owner).await
    }

    async fn cleanup_due_facts(
        &self,
        owner: &Owner,
        fact_sidecar_tables: &[String],
        edge_sidecar_tables: &[String],
        citation_mapping_sidecar_tables: &[String],
        cited_object_sidecar_tables: &[String],
    ) -> Result<CleanupDueFactsOutcome, StorageError> {
        verbs::fact_cleanup::cleanup_due_facts(
            &self.pool,
            owner,
            fact_sidecar_tables,
            edge_sidecar_tables,
            citation_mapping_sidecar_tables,
            cited_object_sidecar_tables,
        )
        .await
    }

    async fn tombstone_fact(
        &self,
        owner: &Owner,
        fact_id: uuid::Uuid,
        fact_sidecar_tables: &[String],
        edge_sidecar_tables: &[String],
        citation_mapping_sidecar_tables: &[String],
        cited_object_sidecar_tables: &[String],
    ) -> Result<TombstoneFactOutcome, StorageError> {
        verbs::fact_cleanup::tombstone_fact(
            &self.pool,
            owner,
            fact_id,
            fact_sidecar_tables,
            edge_sidecar_tables,
            citation_mapping_sidecar_tables,
            cited_object_sidecar_tables,
        )
        .await
    }

    async fn list_change_events_after(
        &self,
        owner: &Owner,
        after: uuid::Uuid,
        limit: usize,
    ) -> Result<Vec<ChangeEventForWake>, StorageError> {
        verbs::consolidate::list_change_events_after(&self.pool, owner, after, limit).await
    }

    async fn list_change_events_for_replay(
        &self,
        owner: &Owner,
        after: uuid::Uuid,
        until: Option<uuid::Uuid>,
        limit: usize,
    ) -> Result<Vec<ChangeEventForWake>, StorageError> {
        verbs::consolidate::list_change_events_for_replay(&self.pool, owner, after, until, limit)
            .await
    }

    async fn load_memory_batch_facts(
        &self,
        owner: &Owner,
        memory_id: proxima_core::MemoryId,
        sidecars: &[SidecarSpec],
    ) -> Result<Vec<proxima_core::FactRow>, StorageError> {
        verbs::consolidate::load_memory_batch_facts(
            &self.pool,
            &self.sidecars,
            owner,
            memory_id,
            sidecars,
        )
        .await
    }

    async fn load_abstraction_heads(
        &self,
        owner: &Owner,
        sidecars: &[SidecarSpec],
        limit: usize,
    ) -> Result<Vec<AbstractionRow>, StorageError> {
        verbs::consolidate::load_abstraction_heads(
            &self.pool,
            &self.sidecars,
            owner,
            sidecars,
            limit,
        )
        .await
    }

    async fn load_perspective_heads(
        &self,
        owner: &Owner,
        instance: PersonalityInstanceId,
        root_perspective_memory_id: proxima_core::MemoryId,
        sidecars: &[SidecarSpec],
        limit: usize,
    ) -> Result<Vec<MemorySnapshot>, StorageError> {
        verbs::consolidate::load_perspective_heads(
            &self.pool,
            &self.sidecars,
            owner,
            instance,
            root_perspective_memory_id,
            sidecars,
            limit,
        )
        .await
    }

    async fn lookup_prior_personality_head(
        &self,
        owner: &Owner,
        instance: &PersonalityRef,
        schema_id: &proxima_core::SchemaId,
    ) -> Result<Option<proxima_core::MemoryId>, StorageError> {
        verbs::consolidate::lookup_prior_personality_head(&self.pool, owner, instance, schema_id)
            .await
    }

    async fn append_personality_memories(
        &self,
        req: &PersonalityWriteRequest<'_>,
    ) -> Result<PersonalityWriteOutcome, StorageError> {
        verbs::consolidate::append_personality_memories(&self.pool, &self.sidecars, req).await
    }

    async fn load_memory_by_id(
        &self,
        owner: &Owner,
        memory_id: proxima_core::MemoryId,
        reader_personality_instance_id: Option<PersonalityInstanceId>,
        sidecars: &[SidecarSpec],
    ) -> Result<Option<MemorySnapshot>, StorageError> {
        verbs::consolidate::load_memory_by_id(
            &self.pool,
            &self.sidecars,
            owner,
            memory_id,
            reader_personality_instance_id,
            sidecars,
        )
        .await
    }

    async fn list_memory_dependencies(
        &self,
        owner: &Owner,
        source_memory_id: MemoryId,
    ) -> Result<Vec<MemoryDependency>, StorageError> {
        verbs::consolidate::list_memory_dependencies(&self.pool, owner, source_memory_id).await
    }

    // --- Entry-level access grants (migration 0005, see crate::access) --------

    async fn resolve_space_relations(
        &self,
        space_owner: &Owner,
        principal: &Principal,
    ) -> Result<Vec<AccessGrantRow>, StorageError> {
        verbs::access_grants::resolve_space_relations(&self.pool, space_owner, principal).await
    }

    async fn resolve_entry_relations(
        &self,
        memory_id: MemoryId,
        principal: &Principal,
    ) -> Result<Vec<AccessGrantRow>, StorageError> {
        verbs::access_grants::resolve_entry_relations(&self.pool, memory_id, principal).await
    }

    async fn resolve_entry_owner(
        &self,
        memory_id: MemoryId,
    ) -> Result<Option<EntryAccessFacts>, StorageError> {
        verbs::access_grants::resolve_entry_owner(&self.pool, memory_id).await
    }

    async fn insert_space_binding(&self, grant: &NewAccessGrant) -> Result<(), StorageError> {
        verbs::access_grants::insert_space_binding(&self.pool, grant).await
    }

    async fn revoke_access_grants(&self, selector: &GrantSelector) -> Result<u64, StorageError> {
        verbs::access_grants::revoke_access_grants(&self.pool, selector).await
    }

    async fn share_entry_atomic(
        &self,
        grant: &NewAccessGrant,
        set_shared_if_private: bool,
    ) -> Result<(), StorageError> {
        verbs::access_grants::share_entry_atomic(&self.pool, grant, set_shared_if_private).await
    }

    async fn unshare_entry_atomic(&self, selector: &GrantSelector) -> Result<u64, StorageError> {
        verbs::access_grants::unshare_entry_atomic(&self.pool, selector).await
    }

    async fn list_access_grants(
        &self,
        space_owner: &Owner,
        resource: GrantResource,
    ) -> Result<Vec<AccessGrantRow>, StorageError> {
        verbs::access_grants::list_access_grants(&self.pool, space_owner, resource).await
    }

    async fn set_memory_visibility(
        &self,
        owner: &Owner,
        memory_id: MemoryId,
        visibility: Visibility,
    ) -> Result<(), StorageError> {
        verbs::access_grants::set_memory_visibility(&self.pool, owner, memory_id, visibility).await
    }

    async fn list_public_memories(&self, limit: i64) -> Result<Vec<MemorySnapshot>, StorageError> {
        verbs::access_grants::list_public_memories(&self.pool, limit).await
    }

    async fn count_active_entry_grants(
        &self,
        owner: &Owner,
        memory_id: MemoryId,
    ) -> Result<u64, StorageError> {
        verbs::access_grants::count_active_entry_grants(&self.pool, owner, memory_id).await
    }

    async fn init_space_owner(
        &self,
        space: &Owner,
        owner_principal: &Principal,
        granted_by: PersonalityInstanceId,
    ) -> Result<(), StorageError> {
        verbs::access_grants::init_space_owner(&self.pool, space, owner_principal, granted_by).await
    }

    async fn add_space_owner(
        &self,
        space: &Owner,
        new_owner: &Principal,
        granted_by: PersonalityInstanceId,
    ) -> Result<(), StorageError> {
        verbs::access_grants::add_space_owner(&self.pool, space, new_owner, granted_by).await
    }

    async fn remove_space_owner(
        &self,
        space: &Owner,
        owner_principal: &Principal,
    ) -> Result<RemoveOwnerOutcome, StorageError> {
        verbs::access_grants::remove_space_owner(&self.pool, space, owner_principal).await
    }
}
