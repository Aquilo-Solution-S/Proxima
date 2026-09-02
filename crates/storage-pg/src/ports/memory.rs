use std::sync::Arc;

use proxima_core::read_models::{MemorySchemaSpec, MemorySnapshot};
use proxima_core::storage_ports::{
    MemoryAuthoringPort, MemoryInspectPort, MemoryReadPort, OwnerWritePermit,
};
use proxima_core::verbs::query::{
    MemoryLineageRequest, MemoryLineageResponse, MemorySearchPage, MemorySearchRequest,
    QueryRequest, QueryResponse,
};
use proxima_core::{
    AuthorDerivedOutcome, AuthorDerivedRequest, FactSourceBatchRow, MemoryGraphIdentity,
    MemoryGraphPayloadRow, MemoryHydrationBatchOutcome, MemoryId, MemoryKindRow, Owner, OwnerRef,
    StorageError, cold_object_key,
};

use crate::error::{internal, with_bounded_retry};
use crate::{PgStorage, verbs};

#[async_trait::async_trait]
impl MemoryAuthoringPort for PgStorage {
    async fn author_derived(
        &self,
        req: &AuthorDerivedRequest<'_>,
        permit: &OwnerWritePermit,
        _proof: proxima_core::storage_ports::OperatorWriteProof,
    ) -> Result<AuthorDerivedOutcome, StorageError> {
        // Retry the whole begin→body→commit on transient deadlock/
        // serialization. A retryable error fully rolls the transaction back, so
        // re-running is clean — the derived row replays on its idempotency key
        // and the index rows re-assert the same primary keys.
        with_bounded_retry(move || async move {
            let mut tx = self.pool.begin().await.map_err(internal)?;
            let draft = verbs::derive_append::DerivedDraft {
                memory_id: req.memory_id.into_inner(),
                owner: req.owner,
                kind: req.kind,
                schema_id: req.schema_id.clone(),
                schema_version: req.schema_version,
                text: req.text.clone(),
                operator_kind: req.operator_kind,
                supersedes: req.supersedes,
                lexical_language: req.lexical_language,
                embedding: req.embedding.clone(),
            };
            // ONE validator for both derived-write paths (this engine port and
            // the flavor-SDK `append_derived_with_edges_in_tx`): a second copy
            // here drifts, and the gate it drops is the created_at strict-time
            // check on origins.
            verbs::derive_append::validate_derived_origins_in_tx(&mut tx, &draft, req.origins)
                .await?;
            verbs::derive_append::validate_derived_reference_kinds_in_tx(&mut tx, req.references)
                .await?;
            let sidecars = self.sidecars.writing_derived(&draft);
            let sidecar_payload = req.sidecar_payload.clone();
            let content_payload = sidecar_payload.clone();
            let tables = self
                .sidecars
                .tables_for_payloads(std::slice::from_ref(&sidecar_payload))?;
            let outcome = verbs::derive_append::append_derived_with_content_payloads_in_tx(
                &mut tx,
                permit,
                &draft,
                verbs::derive_append::DerivedAdmissionInput {
                    origins: req.origins,
                    references: req.references,
                    sidecar_tables: &tables,
                    content: verbs::derive_append::ContentResolution {
                        content_id: None,
                        payloads: Some(std::slice::from_ref(&content_payload)),
                    },
                },
                move |tx, outcome| {
                    Box::pin(async move {
                        sidecars
                            .insert_memory_sidecar(tx, outcome.memory_id, &sidecar_payload)
                            .await
                    })
                },
            )
            .await?;
            if !outcome.idempotent_replay {
                let kind = match req.kind {
                    proxima_core::EntityKind::Fact => "fact",
                    proxima_core::EntityKind::Abstraction => "abstraction",
                    proxima_core::EntityKind::Perspective => "perspective",
                    proxima_core::EntityKind::Goal => "goal",
                };
                verbs::sketch::upsert_sketch(
                    &mut tx,
                    permit.owner().stored_owner_id(),
                    outcome.memory_id.into_inner(),
                    kind,
                    &verbs::sketch::sketch_line(
                        kind,
                        Some(req.text.as_str()),
                        std::slice::from_ref(&req.sidecar_payload),
                    ),
                )
                .await?;
            }
            let edge_count = verbs::derive_append::assert_derived_index_rows(
                &mut tx,
                &draft,
                &outcome,
                req.origins,
                req.references,
            )
            .await?;
            tx.commit().await.map_err(crate::error::map_err)?;
            Ok(AuthorDerivedOutcome {
                memory_id: outcome.memory_id,
                idempotent_replay: outcome.idempotent_replay,
                edge_count,
                // A replay wrote nothing, so it deferred nothing: the row it
                // found already carries whatever vector (or job) the write
                // that minted it left behind.
                embedding_deferred: req.embedding.is_deferred() && !outcome.idempotent_replay,
            })
        })
        .await
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
        let owner_id = owner.stored_owner_id();
        let rows: Vec<(uuid::Uuid, String)> = sqlx::query_as(
            "SELECT m.t, m.kind::text
             FROM proxima_core.memory m
             WHERE m.owner_id = $1
               AND m.t = ANY($2::uuid[])",
        )
        .bind(owner_id)
        .bind(&ids)
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;
        rows.into_iter()
            .map(|(memory_id, kind)| {
                let kind = match kind.as_str() {
                    "fact" | "Fact" => proxima_core::EntityKind::Fact,
                    "abstraction" | "Abstraction" => proxima_core::EntityKind::Abstraction,
                    "perspective" | "Perspective" => proxima_core::EntityKind::Perspective,
                    other => {
                        return Err(StorageError::Internal(format!(
                            "invalid memory kind {other}"
                        )));
                    }
                };
                Ok(MemoryKindRow {
                    memory_id: MemoryId::new(memory_id),
                    kind,
                })
            })
            .collect()
    }

    async fn forget_memory(
        &self,
        permit: &OwnerWritePermit,
        memory_id: MemoryId,
    ) -> Result<(), StorageError> {
        let owner = permit.owner();
        let owner_id = owner.stored_owner_id();
        let t = memory_id.into_inner();
        // Ownership precondition, not a key ingredient: the cold key derives
        // from `t` alone, but a `t` the caller does not own must be NotFound
        // rather than a forget on someone else's row.
        sqlx::query_scalar::<_, uuid::Uuid>(
            "SELECT handle FROM proxima_core.memory WHERE t = $1 AND owner_id = $2",
        )
        .bind(t)
        .bind(owner_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(internal)?
        .ok_or(StorageError::NotFound)?;
        let key = cold_object_key(t);
        let pool = self.pool.clone();
        let cold = Arc::clone(&self.cold);
        let sidecars = self.sidecars.clone();
        let surfaces = self.surfaces.clone();
        with_bounded_retry(move || {
            let key = key.clone();
            let pool = pool.clone();
            let cold = Arc::clone(&cold);
            let sidecars = sidecars.clone();
            let surfaces = surfaces.clone();
            async move {
                verbs::forget::forget_memory_oneshot(
                    &pool,
                    &sidecars,
                    &surfaces,
                    cold.as_ref(),
                    &key,
                    t,
                    owner_id,
                )
                .await
            }
        })
        .await
    }

    async fn hydrate_memories(
        &self,
        permit: &OwnerWritePermit,
        memory_ids: &[MemoryId],
    ) -> Result<MemoryHydrationBatchOutcome, StorageError> {
        let owner_id = permit.owner().stored_owner_id();
        let pool = self.pool.clone();
        let sidecars = self.sidecars.clone();
        let surfaces = self.surfaces.clone();
        let cold = Arc::clone(&self.cold);
        let ids = memory_ids.to_vec();
        let non_embeddable_schemas = self.non_embeddable_schemas.clone();
        with_bounded_retry(move || {
            let pool = pool.clone();
            let sidecars = sidecars.clone();
            let surfaces = surfaces.clone();
            let cold = Arc::clone(&cold);
            let ids = ids.clone();
            let non_embeddable_schemas = non_embeddable_schemas.clone();
            async move {
                verbs::forget::hydrate_memories_oneshot(
                    &pool,
                    &sidecars,
                    &surfaces,
                    cold.as_ref(),
                    owner_id,
                    &ids,
                    &non_embeddable_schemas,
                )
                .await
            }
        })
        .await
    }

    async fn load_fact_source_batches(
        &self,
        _owner: &Owner,
        memory_ids: &[MemoryId],
    ) -> Result<Vec<FactSourceBatchRow>, StorageError> {
        let _ = memory_ids;
        Ok(Vec::new())
    }
}

#[async_trait::async_trait]
impl MemoryReadPort for PgStorage {
    async fn load_fact_text(
        &self,
        owner: &Owner,
        memory_id: MemoryId,
    ) -> Result<Option<String>, StorageError> {
        verbs::fact_embeddings::load_fact_text(&self.pool, owner, memory_id, &self.embed_units)
            .await
    }

    async fn load_memory_graph_payloads(
        &self,
        identities: &[MemoryGraphIdentity],
        schemas: &[MemorySchemaSpec],
        include_body: bool,
    ) -> Result<Vec<MemoryGraphPayloadRow>, StorageError> {
        verbs::consolidate::load_memory_graph_payloads(
            &self.pool,
            &self.sidecars,
            identities,
            schemas,
            include_body,
        )
        .await
    }

    async fn load_sketches(
        &self,
        read_owners: &[OwnerRef],
        memory_ids: &[MemoryId],
    ) -> Result<Vec<proxima_core::read_models::MemorySketch>, StorageError> {
        let rows = verbs::sketch::load_sketches(&self.pool, read_owners, memory_ids).await?;
        Ok(rows
            .into_iter()
            .map(|row| proxima_core::read_models::MemorySketch {
                id: row.id,
                owner: row.owner,
                kind: row.kind,
                text: row.text,
            })
            .collect())
    }

    async fn load_pin_nodes(
        &self,
        read_owners: &[OwnerRef],
        memory_ids: &[MemoryId],
    ) -> Result<Vec<proxima_core::PinNode>, StorageError> {
        verbs::query::load_pin_nodes(&self.pool, read_owners, memory_ids).await
    }

    async fn load_visible_goal_ids(
        &self,
        read_owners: &[OwnerRef],
        goal_ids: &[proxima_core::GoalId],
    ) -> Result<Vec<proxima_core::GoalId>, StorageError> {
        verbs::query::load_visible_goal_ids(&self.pool, read_owners, goal_ids).await
    }

    async fn load_inbound_pin_nodes(
        &self,
        read_owners: &[OwnerRef],
        query: proxima_core::InboundPinQuery<'_>,
    ) -> Result<Vec<proxima_core::PinNode>, StorageError> {
        verbs::query::load_inbound_pin_nodes(&self.pool, read_owners, query).await
    }

    async fn query_memories(
        &self,
        req: &QueryRequest,
        schemas: &[MemorySchemaSpec],
    ) -> Result<QueryResponse, StorageError> {
        verbs::query::query_memories(&self.pool, &self.sidecars, req, schemas).await
    }

    async fn search_memories(
        &self,
        req: &MemorySearchRequest,
        projections: &[proxima_core::verbs::schema::MemorySearchProjection],
    ) -> Result<MemorySearchPage, StorageError> {
        verbs::query::search_memories(&self.pool, req, projections, &self.tuning).await
    }

    async fn walk_memory_lineage(
        &self,
        read_owners: &[OwnerRef],
        req: &MemoryLineageRequest,
    ) -> Result<MemoryLineageResponse, StorageError> {
        verbs::query::walk_memory_lineage(&self.pool, read_owners, req, &self.search_projections)
            .await
    }

    async fn owned_series_handle(
        &self,
        owner: Owner,
        schema_id: &proxima_core::SchemaId,
        sidecar_table: &str,
        columns: &[(&str, proxima_core::verbs::query::SidecarAtom)],
    ) -> Result<Option<uuid::Uuid>, StorageError> {
        let key_column = self
            .sidecars
            .memory_key_column(sidecar_table)
            .ok_or_else(|| {
                StorageError::ConstraintViolation(format!(
                    "owned series-handle lookup names {sidecar_table}, which is not a registered \
                 memory sidecar table; register the payload with `pg_sidecar!` so its memory-key \
                 column is declared"
                ))
            })?;
        verbs::query::owned_head_handle(
            &self.pool,
            owner,
            schema_id,
            sidecar_table,
            key_column,
            columns,
        )
        .await
    }
}

#[async_trait::async_trait]
impl MemoryInspectPort for PgStorage {
    async fn load_memory_by_id(
        &self,
        memory_id: proxima_core::MemoryId,
        schemas: &[MemorySchemaSpec],
    ) -> Result<Option<MemorySnapshot>, StorageError> {
        verbs::consolidate::load_memory_by_id(&self.pool, &self.sidecars, memory_id, schemas).await
    }

    async fn load_memories_by_ids(
        &self,
        read_owners: &[OwnerRef],
        memory_ids: &[MemoryId],
        schemas: &[MemorySchemaSpec],
    ) -> Result<Vec<MemorySnapshot>, StorageError> {
        verbs::consolidate::load_memories_by_ids(
            &self.pool,
            &self.sidecars,
            read_owners,
            memory_ids,
            schemas,
        )
        .await
    }
}
