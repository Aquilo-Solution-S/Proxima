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
    AuthorDerivedOutcome, AuthorDerivedRequest, MemoryGraphIdentity, MemoryGraphPayloadRow,
    MemoryHydrationBatchOutcome, MemoryId, MemoryKindRow, Owner, OwnerRef, StorageError,
    cold_object_key,
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
            let mut tx = crate::owner_scope::begin_compatible_owner_transaction(
                &self.pool,
                permit.owner_scope(),
            )
            .await?;
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
            // Share origin validation with the write-session authoring path.
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
            // `author_derived` is a host-facing write like any other: the
            // scope comes off the payload it carries, so a derived row over a
            // scoped payload is fenced exactly as its ingest would be.
            let scopes = self
                .scopes
                .targets_for_payloads(std::slice::from_ref(&sidecar_payload))?;
            let outcome = verbs::derive_append::append_derived_with_content_payloads_in_tx(
                &mut tx,
                permit,
                &draft,
                verbs::derive_append::DerivedAdmissionInput {
                    origins: req.origins,
                    references: req.references,
                    sidecar_tables: &tables,
                    scopes: &scopes,
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
        owner_scope: Option<&proxima_core::OwnerScope>,
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
        let mut tx =
            crate::owner_scope::begin_compatible_owner_transaction(&self.pool, owner_scope).await?;
        let rows: Vec<(uuid::Uuid, String)> = sqlx::query_as(
            "SELECT m.t, m.kind::text
             FROM proxima_core.memory m
             WHERE m.owner_id = $1
               AND m.t = ANY($2::uuid[])",
        )
        .bind(owner_id)
        .bind(&ids)
        .fetch_all(&mut *tx)
        .await
        .map_err(internal)?;
        tx.commit().await.map_err(crate::error::map_err)?;
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
        // The same-transaction probe in forget_memory_oneshot_in_transaction
        // checks this owner before reading or publishing any cold payload.
        let key = cold_object_key(t);
        let storage = self.clone();
        let cold = Arc::clone(&self.cold);
        let sidecars = self.sidecars.clone();
        let surfaces = self.surfaces.clone();
        with_bounded_retry(move || {
            let key = key.clone();
            let storage = storage.clone();
            let cold = Arc::clone(&cold);
            let sidecars = sidecars.clone();
            let surfaces = surfaces.clone();
            async move {
                let tx = storage.owner_maintenance_transaction(permit).await?;
                verbs::forget::forget_memory_oneshot_in_transaction(
                    tx,
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
        let storage = self.clone();
        let sidecars = self.sidecars.clone();
        let surfaces = self.surfaces.clone();
        let cold = Arc::clone(&self.cold);
        let ids = memory_ids.to_vec();
        let non_embeddable_schemas = self.non_embeddable_schemas.clone();
        with_bounded_retry(move || {
            let storage = storage.clone();
            let sidecars = sidecars.clone();
            let surfaces = surfaces.clone();
            let cold = Arc::clone(&cold);
            let ids = ids.clone();
            let non_embeddable_schemas = non_embeddable_schemas.clone();
            async move {
                let tx = storage.owner_maintenance_transaction(permit).await?;
                verbs::forget::hydrate_memories_oneshot(
                    tx,
                    &sidecars,
                    &surfaces,
                    cold.as_ref(),
                    permit,
                    &ids,
                    &non_embeddable_schemas,
                )
                .await
            }
        })
        .await
    }
}

#[async_trait::async_trait]
impl MemoryReadPort for PgStorage {
    async fn load_fact_text(
        &self,
        owner_scope: Option<&proxima_core::OwnerScope>,
        owner: &Owner,
        memory_id: MemoryId,
    ) -> Result<Option<String>, StorageError> {
        let mut tx =
            crate::owner_scope::begin_compatible_owner_transaction(&self.pool, owner_scope).await?;
        let result = async {
            verbs::fact_embeddings::load_fact_text_in_tx(
                &mut tx,
                owner,
                memory_id,
                &self.embed_units,
            )
            .await
        }
        .await;
        crate::owner_scope::finish_transaction(tx, result).await
    }

    async fn load_memory_graph_payloads(
        &self,
        owner_scope: Option<&proxima_core::OwnerScope>,
        identities: &[MemoryGraphIdentity],
        schemas: &[MemorySchemaSpec],
        include_body: bool,
    ) -> Result<Vec<MemoryGraphPayloadRow>, StorageError> {
        let mut tx =
            crate::owner_scope::begin_compatible_owner_transaction(&self.pool, owner_scope).await?;
        let result = async {
            verbs::consolidate::load_memory_graph_payloads_on_connection(
                &mut tx,
                &self.sidecars,
                identities,
                schemas,
                include_body,
            )
            .await
        }
        .await;
        crate::owner_scope::finish_transaction(tx, result).await
    }

    async fn load_sketches(
        &self,
        owner_scope: Option<&proxima_core::OwnerScope>,
        read_owners: &[OwnerRef],
        memory_ids: &[MemoryId],
    ) -> Result<Vec<proxima_core::read_models::MemorySketch>, StorageError> {
        let mut tx =
            crate::owner_scope::begin_compatible_owner_transaction(&self.pool, owner_scope).await?;
        let result = async {
            let rows = verbs::sketch::load_sketches_on_connection(&mut tx, read_owners, memory_ids)
                .await?;
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
        .await;
        crate::owner_scope::finish_transaction(tx, result).await
    }

    async fn load_pin_nodes(
        &self,
        owner_scope: Option<&proxima_core::OwnerScope>,
        read_owners: &[OwnerRef],
        memory_ids: &[MemoryId],
    ) -> Result<Vec<proxima_core::PinNode>, StorageError> {
        let mut tx =
            crate::owner_scope::begin_compatible_owner_transaction(&self.pool, owner_scope).await?;
        let result = async {
            verbs::query::load_pin_nodes_on_connection(&mut tx, read_owners, memory_ids).await
        }
        .await;
        crate::owner_scope::finish_transaction(tx, result).await
    }

    async fn load_visible_goal_ids(
        &self,
        owner_scope: Option<&proxima_core::OwnerScope>,
        read_owners: &[OwnerRef],
        goal_ids: &[proxima_core::GoalId],
    ) -> Result<Vec<proxima_core::GoalId>, StorageError> {
        let mut tx =
            crate::owner_scope::begin_compatible_owner_transaction(&self.pool, owner_scope).await?;
        let result = async {
            verbs::query::load_visible_goal_ids_on_connection(&mut tx, read_owners, goal_ids).await
        }
        .await;
        crate::owner_scope::finish_transaction(tx, result).await
    }

    async fn load_inbound_pin_nodes(
        &self,
        owner_scope: Option<&proxima_core::OwnerScope>,
        read_owners: &[OwnerRef],
        query: proxima_core::InboundPinQuery<'_>,
    ) -> Result<Vec<proxima_core::PinNode>, StorageError> {
        let mut tx =
            crate::owner_scope::begin_compatible_owner_transaction(&self.pool, owner_scope).await?;
        let result = async {
            verbs::query::load_inbound_pin_nodes_on_connection(&mut tx, read_owners, query).await
        }
        .await;
        crate::owner_scope::finish_transaction(tx, result).await
    }

    async fn query_memories(
        &self,
        owner_scope: Option<&proxima_core::OwnerScope>,
        read_owners: &[OwnerRef],
        req: &QueryRequest,
        schemas: &[MemorySchemaSpec],
    ) -> Result<QueryResponse, StorageError> {
        let mut tx =
            crate::owner_scope::begin_compatible_owner_transaction(&self.pool, owner_scope).await?;
        let result = async {
            verbs::query::query_memories_on_connection(
                &mut tx,
                &self.sidecars,
                read_owners,
                req,
                schemas,
            )
            .await
        }
        .await;
        crate::owner_scope::finish_transaction(tx, result).await
    }

    async fn search_memories(
        &self,
        owner_scope: Option<&proxima_core::OwnerScope>,
        req: &MemorySearchRequest,
        projections: &[proxima_core::verbs::schema::MemorySearchProjection],
    ) -> Result<MemorySearchPage, StorageError> {
        let mut tx =
            crate::owner_scope::begin_compatible_owner_transaction(&self.pool, owner_scope).await?;
        let result = async {
            verbs::query::search_memories_on_connection(&mut tx, req, projections, &self.tuning)
                .await
        }
        .await;
        crate::owner_scope::finish_transaction(tx, result).await
    }

    async fn walk_memory_lineage(
        &self,
        owner_scope: Option<&proxima_core::OwnerScope>,
        read_owners: &[OwnerRef],
        req: &MemoryLineageRequest,
    ) -> Result<MemoryLineageResponse, StorageError> {
        let mut tx =
            crate::owner_scope::begin_compatible_owner_transaction(&self.pool, owner_scope).await?;
        let result = async {
            verbs::query::walk_memory_lineage_on_connection(
                &mut tx,
                read_owners,
                req,
                &self.search_projections,
            )
            .await
        }
        .await;
        crate::owner_scope::finish_transaction(tx, result).await
    }

    async fn owned_series_handle(
        &self,
        owner_scope: Option<&proxima_core::OwnerScope>,
        owner: Owner,
        schema_id: &proxima_core::SchemaId,
        sidecar_table: &str,
        columns: &[(&str, proxima_core::verbs::query::SidecarAtom)],
    ) -> Result<Option<uuid::Uuid>, StorageError> {
        let mut tx =
            crate::owner_scope::begin_compatible_owner_transaction(&self.pool, owner_scope).await?;
        let result = async {
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
                &mut *tx,
                owner,
                schema_id,
                sidecar_table,
                key_column,
                columns,
            )
            .await
        }
        .await;
        crate::owner_scope::finish_transaction(tx, result).await
    }
}

#[async_trait::async_trait]
impl MemoryInspectPort for PgStorage {
    async fn load_memory_by_id(
        &self,
        owner_scope: Option<&proxima_core::OwnerScope>,
        memory_id: proxima_core::MemoryId,
        schemas: &[MemorySchemaSpec],
    ) -> Result<Option<MemorySnapshot>, StorageError> {
        let mut tx =
            crate::owner_scope::begin_compatible_owner_transaction(&self.pool, owner_scope).await?;
        let result = async {
            verbs::consolidate::load_memory_by_id_on_connection(
                &mut tx,
                &self.sidecars,
                memory_id,
                schemas,
            )
            .await
        }
        .await;
        crate::owner_scope::finish_transaction(tx, result).await
    }

    async fn load_memories_by_ids(
        &self,
        owner_scope: Option<&proxima_core::OwnerScope>,
        read_owners: &[OwnerRef],
        memory_ids: &[MemoryId],
        schemas: &[MemorySchemaSpec],
    ) -> Result<Vec<MemorySnapshot>, StorageError> {
        let mut tx =
            crate::owner_scope::begin_compatible_owner_transaction(&self.pool, owner_scope).await?;
        let result = async {
            verbs::consolidate::load_memories_by_ids_on_connection(
                &mut tx,
                &self.sidecars,
                read_owners,
                memory_ids,
                schemas,
            )
            .await
        }
        .await;
        crate::owner_scope::finish_transaction(tx, result).await
    }
}
