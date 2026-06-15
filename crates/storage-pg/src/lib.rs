//! Postgres `Storage` impl.
//!
//! See docs/07-storage.md and the `Storage` trait in
//! `proxima_core`.
//!
//! The verb logic lives under [`verbs`]; this module wires the
//! `PgStorage` struct, connection lifecycle, and migration runner,
//! then delegates each `Storage` trait method to its per-verb
//! implementation.

use std::sync::Arc;
use std::time::Duration;

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
use proxima_core::verbs::fact_cleanup::CleanupDueFactsOutcome;
use proxima_core::verbs::goal_write::{
    AchieveGoalAtomicRequest, CreateGoalAtomicRequest, DecomposeGoalAtomicRequest,
    DecomposeGoalOutcome, GoalWriteOutcome, ModifyGoalAtomicRequest, TransitionGoalAtomicRequest,
};
use proxima_core::verbs::persist_mcp_call::{McpCallLogInput, McpCallLogOutcome};
use proxima_core::verbs::query::{
    FactCitationReadback, MemoryLineageRequest, MemoryLineageResponse, MemorySearchRequest,
    MemorySearchResult, QueryRequest, QueryResponse,
};
use proxima_core::{
    AuthorDerivedOutcome, AuthorDerivedRequest, DerivedEdgeSpec, EmbeddingJobClaim,
    MasterTokenPersonality, MemoryDependency, MemoryId, Owner, Principal, SourceBatchId, Storage,
    StorageError, StorageHandle,
};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{PgPool, Postgres, Transaction};

mod authorship;
mod change_event;
mod error;
mod pg_ident;
mod pgvector;
pub mod query {
    pub use crate::verbs::query::MAX_SNAPSHOT_EDGES;
}
pub mod verbs;

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

        Ok(Self { pool })
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
    pub fn into_handle(self) -> StorageHandle {
        Arc::new(self)
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
        core_migrator()
            .run(&self.pool)
            .await
            .map_err(|e| StorageError::Internal(e.to_string()))?;
        Ok(())
    }
}

async fn insert_generic_memory_sidecar(
    tx: &mut Transaction<'_, Postgres>,
    memory_id: MemoryId,
    sidecar_table: &str,
    sidecar_payload: &serde_json::Value,
) -> Result<(), StorageError> {
    let table = pg_ident::PgIdent::table(sidecar_table)?
        .as_str()
        .to_string();
    let sql = format!(
        "INSERT INTO {table}
         SELECT * FROM jsonb_populate_record(
             NULL::{table},
             ($1::jsonb || jsonb_build_object('memory_id', $2::uuid))
         )",
    );
    sqlx::query(&sql)
        .bind(sidecar_payload)
        .bind(memory_id.into_inner())
        .execute(&mut **tx)
        .await
        .map_err(crate::error::map_err)?;
    Ok(())
}

fn edge_draft_from_spec<'a>(edge: &'a DerivedEdgeSpec<'a>) -> verbs::edge_append::EdgeDraft<'a> {
    verbs::edge_append::EdgeDraft {
        edge_id: uuid::Uuid::now_v7(),
        relation: edge.relation,
        source_kind: edge.source_kind,
        source_memory_id: Some(edge.source_memory_id.into_inner()),
        source_goal_id: None,
        target_kind: edge.target_kind,
        target_memory_id: Some(edge.target_memory_id.into_inner()),
        target_goal_id: None,
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

    async fn persist_mcp_call_atomic(
        &self,
        input: &McpCallLogInput,
    ) -> Result<McpCallLogOutcome, StorageError> {
        verbs::persist_mcp_call::persist_mcp_call_atomic(&self.pool, input).await
    }

    async fn ingest_event_with_sidecar(
        &self,
        authorized: &AuthorizedEventIngest,
        sidecar_table: &str,
        sidecar_payload: &serde_json::Value,
        embedding_model_id: Option<&str>,
    ) -> Result<EventIngestOutcome, StorageError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| StorageError::Internal(e.to_string()))?;
        let table = sidecar_table.to_string();
        let payload = sidecar_payload.clone();
        let outcome = verbs::event_ingest::ingest_event_with_sidecar_in_tx(
            &mut tx,
            authorized,
            embedding_model_id,
            move |tx, outcome| {
                Box::pin(async move {
                    insert_generic_memory_sidecar(tx, outcome.memory_id, &table, &payload).await
                })
            },
        )
        .await?;
        tx.commit().await.map_err(crate::error::map_err)?;
        Ok(outcome)
    }

    async fn ingest_fact_with_citation_and_sidecar(
        &self,
        authorized: &AuthorizedFactWithCitation,
        sidecar_table: &str,
        sidecar_payload: &serde_json::Value,
        embedding_model_id: Option<&str>,
    ) -> Result<EventIngestOutcome, StorageError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| StorageError::Internal(e.to_string()))?;
        let table = sidecar_table.to_string();
        let payload = sidecar_payload.clone();
        let outcome = verbs::event_ingest::ingest_fact_with_citation_in_tx(
            &mut tx,
            authorized,
            embedding_model_id,
            move |tx, outcome| {
                Box::pin(async move {
                    insert_generic_memory_sidecar(tx, outcome.memory_id, &table, &payload).await
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
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| StorageError::Internal(e.to_string()))?;
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
            sidecar_table: Some(req.sidecar_table),
            sidecar_payload: Some(req.sidecar_payload.clone()),
            embedding: req.embedding.clone(),
            embedding_model_id: req.embedding_model_id,
        };
        let outcome = verbs::derive_append::append_derived_in_tx(&mut tx, &draft).await?;
        let mut edge_count = 0;
        if !outcome.idempotent_replay {
            for edge in req.edges {
                let draft = edge_draft_from_spec(edge);
                verbs::edge_append::append_edge_in_tx(&mut tx, &draft, None).await?;
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

    async fn append_memory_edge(&self, edge: &DerivedEdgeSpec<'_>) -> Result<(), StorageError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| StorageError::Internal(e.to_string()))?;
        let draft = edge_draft_from_spec(edge);
        verbs::edge_append::append_edge_in_tx(&mut tx, &draft, edge.edge_payload).await?;
        tx.commit().await.map_err(crate::error::map_err)
    }

    async fn create_goal_atomic(
        &self,
        req: &CreateGoalAtomicRequest<'_>,
    ) -> Result<GoalWriteOutcome, StorageError> {
        verbs::goal_write::create_goal_atomic(&self.pool, req).await
    }

    async fn transition_goal_atomic(
        &self,
        req: &TransitionGoalAtomicRequest<'_>,
    ) -> Result<GoalWriteOutcome, StorageError> {
        verbs::goal_write::transition_goal_atomic(&self.pool, req).await
    }

    async fn achieve_goal_atomic(
        &self,
        req: &AchieveGoalAtomicRequest<'_>,
    ) -> Result<GoalWriteOutcome, StorageError> {
        verbs::goal_write::achieve_goal_atomic(&self.pool, req).await
    }

    async fn modify_goal_atomic(
        &self,
        req: &ModifyGoalAtomicRequest<'_>,
    ) -> Result<GoalWriteOutcome, StorageError> {
        verbs::goal_write::modify_goal_atomic(&self.pool, req).await
    }

    async fn decompose_goal_atomic(
        &self,
        req: &DecomposeGoalAtomicRequest<'_>,
    ) -> Result<DecomposeGoalOutcome, StorageError> {
        verbs::goal_write::decompose_goal_atomic(&self.pool, req).await
    }

    async fn event_history(
        &self,
        req: &EventHistoryRequest,
    ) -> Result<EventHistoryResponse, StorageError> {
        verbs::event_history::event_history(&self.pool, req).await
    }

    async fn query_memories(
        &self,
        req: &QueryRequest,
        schemas: &[proxima_core::verbs::schema::SchemaInfo],
    ) -> Result<QueryResponse, StorageError> {
        verbs::query::query_memories(&self.pool, req, schemas).await
    }

    async fn search_memories(
        &self,
        req: &MemorySearchRequest,
        projections: &[proxima_core::verbs::schema::MemorySearchProjection],
    ) -> Result<Vec<MemorySearchResult>, StorageError> {
        verbs::query::search_memories(&self.pool, req, projections).await
    }

    async fn facts_citing_object(
        &self,
        owner: &Owner,
        cited_object_id: uuid::Uuid,
        sidecars: &[SidecarSpec],
    ) -> Result<Vec<MemorySnapshot>, StorageError> {
        verbs::query::facts_citing_object(&self.pool, owner, cited_object_id, sidecars).await
    }

    async fn citation_of_fact(
        &self,
        owner: &Owner,
        fact_memory_id: MemoryId,
    ) -> Result<Option<FactCitationReadback>, StorageError> {
        verbs::query::citation_of_fact(&self.pool, owner, fact_memory_id).await
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
        verbs::consolidate::load_memory_batch_facts(&self.pool, owner, memory_id, sidecars).await
    }

    async fn load_abstraction_heads(
        &self,
        owner: &Owner,
        sidecars: &[SidecarSpec],
        limit: usize,
    ) -> Result<Vec<AbstractionRow>, StorageError> {
        verbs::consolidate::load_abstraction_heads(&self.pool, owner, sidecars, limit).await
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
        verbs::consolidate::append_personality_memories(&self.pool, req).await
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

    async fn has_satisfied_code_test_request(
        &self,
        owner: &Owner,
        test_request_memory_id: MemoryId,
    ) -> Result<bool, StorageError> {
        verbs::consolidate::has_satisfied_code_test_request(
            &self.pool,
            owner,
            test_request_memory_id,
        )
        .await
    }
}
