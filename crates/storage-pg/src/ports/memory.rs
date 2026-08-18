use std::sync::Arc;

use proxima_core::read_models::{MemorySnapshot, SidecarSpec};
use proxima_core::storage_ports::{
    MemoryAuthoringPort, MemoryInspectPort, MemoryReadPort, OwnerWritePermit,
};
use proxima_core::verbs::query::{
    MemoryLineageRequest, MemoryLineageResponse, MemorySearchPage, MemorySearchRequest,
    QueryRequest, QueryResponse,
};
use proxima_core::{
    AuthorDerivedOutcome, AuthorDerivedRequest, FactSourceBatchRow, MemoryGraphIdentity,
    MemoryGraphPayloadRow, MemoryId, MemoryKindRow, Owner, OwnerRef, StorageError,
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
                operator_id: req.operator_id,
                input_contract_id: req.input_contract_id,
                source_batch_id: req.source_batch_id,
                model_id: req.model_id,
                prompt_version: req.prompt_version,
                authoring_perspective_id: req.authoring_perspective_id,
                supersedes: req.supersedes,
                lexical_language: req.lexical_language,
                embedding: req.embedding.clone(),
            };
            // ONE validator for both derived-write paths (this engine port and
            // the flavor-SDK `append_derived_with_edges_in_tx`): the port used
            // to carry its own near-duplicate proof validation here, which
            // silently missed the created_at strict-time gate.
            verbs::derive_append::validate_derived_origins_in_tx(&mut tx, &draft, req.origins)
                .await?;
            verbs::derive_append::validate_derived_reference_kinds_in_tx(&mut tx, req.references)
                .await?;
            let sidecars = self.sidecars.clone();
            let sidecar_payload = req.sidecar_payload.clone();
            let tables = sidecars.tables_for_payloads(std::slice::from_ref(&sidecar_payload))?;
            crate::access::owner_columns::reject_world_write_owner(permit.owner())?;
            let owner_id =
                crate::access::owner_columns::ensure_owner_row(tx.as_mut(), permit.owner()).await?;
            let content_id = verbs::content::ensure_content_from_payloads(
                &mut tx,
                owner_id,
                req.schema_id.as_str(),
                std::slice::from_ref(&sidecar_payload),
            )
            .await?;
            let outcome = verbs::derive_append::append_derived_in_tx(
                &mut tx,
                permit,
                &draft,
                req.origins,
                req.references,
                &tables,
                content_id,
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
                    owner_id,
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
        crate::access::owner_columns::reject_world_write_owner(owner)?;
        let owner_id = owner.stored_owner_id();
        let t = memory_id.into_inner();
        let handle: uuid::Uuid = sqlx::query_scalar(
            "SELECT handle FROM proxima_core.memory WHERE t = $1 AND owner_id = $2",
        )
        .bind(t)
        .bind(owner_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(internal)?
        .ok_or(StorageError::NotFound)?;
        let key = verbs::forget::cold_object_key(&verbs::forget::owner_hash_hex(owner), handle, t);
        let pool = self.pool.clone();
        let cold = Arc::clone(&self.cold);
        let sidecars = self.sidecars.clone();
        with_bounded_retry(move || {
            let key = key.clone();
            let pool = pool.clone();
            let cold = Arc::clone(&cold);
            let sidecars = sidecars.clone();
            async move {
                verbs::forget::forget_memory_oneshot(
                    &pool,
                    &sidecars,
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
        verbs::fact_embeddings::load_fact_text(
            &self.pool,
            owner,
            memory_id,
            &self.search_projections,
        )
        .await
    }

    async fn load_memory_graph_payloads(
        &self,
        identities: &[MemoryGraphIdentity],
        include_body: bool,
    ) -> Result<Vec<MemoryGraphPayloadRow>, StorageError> {
        verbs::consolidate::load_memory_graph_payloads(
            &self.pool,
            &self.sidecars,
            identities,
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
        schemas: &[proxima_core::verbs::schema::SchemaInfo],
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
        verbs::query::owned_head_handle(&self.pool, owner, schema_id, sidecar_table, columns).await
    }
}

#[async_trait::async_trait]
impl MemoryInspectPort for PgStorage {
    async fn load_memory_by_id(
        &self,
        memory_id: proxima_core::MemoryId,
        sidecars: &[SidecarSpec],
    ) -> Result<Option<MemorySnapshot>, StorageError> {
        verbs::consolidate::load_memory_by_id(&self.pool, &self.sidecars, memory_id, sidecars).await
    }

    async fn load_memories_by_ids(
        &self,
        read_owners: &[OwnerRef],
        memory_ids: &[MemoryId],
        sidecars: &[SidecarSpec],
    ) -> Result<Vec<MemorySnapshot>, StorageError> {
        verbs::consolidate::load_memories_by_ids(
            &self.pool,
            &self.sidecars,
            read_owners,
            memory_ids,
            sidecars,
        )
        .await
    }
}
