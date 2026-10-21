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
    MemoryId, MemoryKindRow, OwnerId, StorageError,
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
            let mut tx = crate::owner_scope::begin_owner_transaction(
                &self.pool,
                crate::owner_scope::require_owner_scope(permit.owner_scope())?,
            )
            .await?;
            let draft = verbs::derive_append::DerivedDraft::from_request(req, self.store_number);
            // Share origin validation with the write-session authoring path.
            verbs::derive_append::validate_derived_origins_in_tx(
                &mut tx,
                &draft,
                req.origins,
                req.references,
            )
            .await?;
            verbs::derive_append::validate_derived_reference_kinds_in_tx(
                &mut tx,
                &draft,
                req.origins,
                req.references,
            )
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
                verbs::sketch::upsert_sketch(
                    &mut tx,
                    permit.owner().into_inner(),
                    outcome.memory_id.into_inner(),
                    req.kind.into(),
                    &verbs::sketch::sketch_line(
                        req.kind.into(),
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
            let edge_count = edge_count.saturating_add(
                verbs::derive_append::record_external_edges(
                    &mut tx,
                    &outcome,
                    req.external_origins,
                    req.external_references,
                )
                .await?,
            );
            crate::write_wake::commit(tx).await?;
            Ok(AuthorDerivedOutcome {
                memory_id: outcome.memory_id,
                idempotent_replay: outcome.idempotent_replay,
                forgotten: outcome.forgotten,
                edge_count,
                // A replay wrote nothing, so it deferred nothing: the row it
                // found already carries whatever vector (or job) the write
                // that minted it left behind.
                embedding_deferred: !req.queued_spaces.is_empty() && !outcome.idempotent_replay,
            })
        })
        .await
    }

    async fn load_memory_kinds(
        &self,
        owner_scope: Option<&proxima_core::OwnerScope>,
        owner: &OwnerId,
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
        let owner_id = owner.into_inner();
        let mut tx = crate::owner_scope::begin_owner_transaction(
            &self.pool,
            crate::owner_scope::require_owner_scope(owner_scope)?,
        )
        .await?;
        let rows: Vec<(uuid::Uuid, crate::pg_enums::PgMemoryKind)> = sqlx::query_as(
            "SELECT m.t, m.kind
             FROM proxima_core.memory m
             WHERE m.owner_id = $1
               AND m.t = ANY($2::uuid[])",
        )
        .bind(owner_id)
        .bind(&ids)
        .fetch_all(&mut *tx)
        .await
        .map_err(internal)?;
        crate::write_wake::commit(tx).await?;
        Ok(rows
            .into_iter()
            .map(|(memory_id, kind)| MemoryKindRow {
                memory_id: MemoryId::new(memory_id),
                kind: kind.into(),
            })
            .collect())
    }

    async fn forget_memory(
        &self,
        permit: &OwnerWritePermit,
        memory_id: MemoryId,
    ) -> Result<(), StorageError> {
        let t = memory_id.into_inner();
        // The same-transaction probe in forget_memory_oneshot_in_transaction
        // checks the permit's owner and kinds before marking the row.
        let storage = self.clone();
        let sidecars = self.sidecars.clone();
        let surfaces = self.surfaces.clone();
        with_bounded_retry(move || {
            let storage = storage.clone();
            let sidecars = sidecars.clone();
            let surfaces = surfaces.clone();
            async move {
                let tx = storage.owner_maintenance_transaction(permit).await?;
                verbs::forget::forget_memory_oneshot_in_transaction(
                    tx,
                    &sidecars,
                    &surfaces,
                    t,
                    &verbs::forget::Reach::of(permit),
                )
                .await
            }
        })
        .await
    }
    async fn restore_memory(
        &self,
        permit: &OwnerWritePermit,
        memory_id: MemoryId,
        embedding_spaces: &[proxima_core::EmbeddingSpace],
    ) -> Result<(), StorageError> {
        let t = memory_id.into_inner();
        let storage = self.clone();
        let sidecars = self.sidecars.clone();
        let non_embeddable_schemas = self.non_embeddable_schemas.clone();
        with_bounded_retry(move || {
            let storage = storage.clone();
            let sidecars = sidecars.clone();
            let non_embeddable_schemas = non_embeddable_schemas.clone();
            async move {
                let tx = storage.owner_maintenance_transaction(permit).await?;
                verbs::forget::restore_memory_oneshot_in_transaction(
                    tx,
                    &sidecars,
                    t,
                    &verbs::forget::Reach::of(permit),
                    verbs::fact_embeddings::RouteSpaces {
                        spaces: embedding_spaces,
                        non_embeddable_schemas: &non_embeddable_schemas,
                    },
                )
                .await
            }
        })
        .await
    }
}

#[async_trait::async_trait]
impl MemoryReadPort for PgStorage {
    fn store_number(&self) -> u16 {
        self.store_number
    }

    async fn load_fact_text(
        &self,
        owner_scope: Option<&proxima_core::OwnerScope>,
        owner: &OwnerId,
        memory_id: MemoryId,
    ) -> Result<Option<String>, StorageError> {
        let mut tx = crate::owner_scope::begin_owner_transaction(
            &self.pool,
            crate::owner_scope::require_owner_scope(owner_scope)?,
        )
        .await?;
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
        let mut tx = crate::owner_scope::begin_owner_transaction(
            &self.pool,
            crate::owner_scope::require_owner_scope(owner_scope)?,
        )
        .await?;
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
        read_owners: &[OwnerId],
        memory_ids: &[MemoryId],
    ) -> Result<Vec<proxima_core::read_models::MemorySketch>, StorageError> {
        let mut tx = crate::owner_scope::begin_owner_transaction(
            &self.pool,
            crate::owner_scope::require_owner_scope(owner_scope)?,
        )
        .await?;
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
        read_owners: &[OwnerId],
        memory_ids: &[MemoryId],
    ) -> Result<Vec<proxima_core::PinNode>, StorageError> {
        let mut tx = crate::owner_scope::begin_owner_transaction(
            &self.pool,
            crate::owner_scope::require_owner_scope(owner_scope)?,
        )
        .await?;
        let result = async {
            verbs::query::load_pin_nodes_on_connection(&mut tx, read_owners, memory_ids).await
        }
        .await;
        crate::owner_scope::finish_transaction(tx, result).await
    }

    async fn load_visible_goal_ids(
        &self,
        owner_scope: Option<&proxima_core::OwnerScope>,
        read_owners: &[OwnerId],
        goal_ids: &[proxima_core::GoalId],
    ) -> Result<Vec<proxima_core::GoalId>, StorageError> {
        let mut tx = crate::owner_scope::begin_owner_transaction(
            &self.pool,
            crate::owner_scope::require_owner_scope(owner_scope)?,
        )
        .await?;
        let result = async {
            verbs::query::load_visible_goal_ids_on_connection(&mut tx, read_owners, goal_ids).await
        }
        .await;
        crate::owner_scope::finish_transaction(tx, result).await
    }

    async fn load_inbound_pin_nodes(
        &self,
        owner_scope: Option<&proxima_core::OwnerScope>,
        read_owners: &[OwnerId],
        query: proxima_core::InboundPinQuery<'_>,
    ) -> Result<Vec<proxima_core::PinNode>, StorageError> {
        let mut tx = crate::owner_scope::begin_owner_transaction(
            &self.pool,
            crate::owner_scope::require_owner_scope(owner_scope)?,
        )
        .await?;
        let result = async {
            verbs::query::load_inbound_pin_nodes_on_connection(&mut tx, read_owners, query).await
        }
        .await;
        crate::owner_scope::finish_transaction(tx, result).await
    }

    async fn query_memories(
        &self,
        owner_scope: Option<&proxima_core::OwnerScope>,
        read_owners: &[OwnerId],
        req: &QueryRequest,
        schemas: &[MemorySchemaSpec],
    ) -> Result<QueryResponse, StorageError> {
        let mut tx = crate::owner_scope::begin_owner_transaction(
            &self.pool,
            crate::owner_scope::require_owner_scope(owner_scope)?,
        )
        .await?;
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

    async fn count_memories(
        &self,
        owner_scope: Option<&proxima_core::OwnerScope>,
        read_owners: &[OwnerId],
    ) -> Result<proxima_core::read_models::MemoryCounts, StorageError> {
        let mut tx = crate::owner_scope::begin_owner_transaction(
            &self.pool,
            crate::owner_scope::require_owner_scope(owner_scope)?,
        )
        .await?;
        let result =
            async { verbs::query::count_memories_on_connection(&mut tx, read_owners).await }.await;
        crate::owner_scope::finish_transaction(tx, result).await
    }

    async fn search_memories(
        &self,
        owner_scope: Option<&proxima_core::OwnerScope>,
        req: &MemorySearchRequest,
        projections: &[proxima_core::verbs::schema::MemorySearchProjection],
    ) -> Result<MemorySearchPage, StorageError> {
        let mut tx = crate::owner_scope::begin_owner_transaction(
            &self.pool,
            crate::owner_scope::require_owner_scope(owner_scope)?,
        )
        .await?;
        let result = async {
            // A request can narrow authenticated authority. The database helper
            // is also the candidate functions' source of the owner-set union.
            let authenticated_owners: Vec<uuid::Uuid> =
                sqlx::query_scalar("SELECT proxima_core.search_owner_ids()")
                    .fetch_one(tx.as_mut())
                    .await
                    .map_err(crate::error::map_err)?;
            let mut req = req.clone();
            req.read_owners
                .retain(|owner| authenticated_owners.contains(&owner.into_inner()));
            verbs::query::search_memories_on_connection(
                &mut tx,
                &req,
                projections,
                &self.tuning,
                &self.vectors,
            )
            .await
        }
        .await;
        crate::owner_scope::finish_transaction(tx, result).await
    }

    async fn walk_memory_lineage(
        &self,
        owner_scope: Option<&proxima_core::OwnerScope>,
        read_owners: &[OwnerId],
        req: &MemoryLineageRequest,
    ) -> Result<MemoryLineageResponse, StorageError> {
        let mut tx = crate::owner_scope::begin_owner_transaction(
            &self.pool,
            crate::owner_scope::require_owner_scope(owner_scope)?,
        )
        .await?;
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

    async fn external_edges(
        &self,
        owner_scope: Option<&proxima_core::OwnerScope>,
        read_owners: &[OwnerId],
        memory_ids: &[proxima_core::MemoryId],
    ) -> Result<Vec<(proxima_core::MemoryId, uuid::Uuid, proxima_core::EdgeKind)>, StorageError>
    {
        if read_owners.is_empty() || memory_ids.is_empty() {
            return Ok(Vec::new());
        }
        let owner_ids: Vec<uuid::Uuid> = read_owners
            .iter()
            .copied()
            .map(OwnerId::into_inner)
            .collect();
        let ids: Vec<uuid::Uuid> = memory_ids
            .iter()
            .copied()
            .map(proxima_core::MemoryId::into_inner)
            .collect();
        let mut tx = crate::owner_scope::begin_owner_transaction(
            &self.pool,
            crate::owner_scope::require_owner_scope(owner_scope)?,
        )
        .await?;
        let result =
            async { verbs::query::external_edges_on_connection(&mut tx, &ids, &owner_ids).await }
                .await;
        crate::owner_scope::finish_transaction(tx, result).await
    }

    async fn lineage_nodes(
        &self,
        owner_scope: Option<&proxima_core::OwnerScope>,
        read_owners: &[OwnerId],
        memory_ids: &[proxima_core::MemoryId],
    ) -> Result<Vec<proxima_core::verbs::query::MemoryLineageNode>, StorageError> {
        if read_owners.is_empty() || memory_ids.is_empty() {
            return Ok(Vec::new());
        }
        let owner_ids: Vec<uuid::Uuid> = read_owners
            .iter()
            .copied()
            .map(OwnerId::into_inner)
            .collect();
        let ids: Vec<uuid::Uuid> = memory_ids
            .iter()
            .copied()
            .map(proxima_core::MemoryId::into_inner)
            .collect();
        let mut tx = crate::owner_scope::begin_owner_transaction(
            &self.pool,
            crate::owner_scope::require_owner_scope(owner_scope)?,
        )
        .await?;
        let result = async {
            verbs::query::lineage_nodes_on_connection(
                &mut tx,
                &ids,
                &owner_ids,
                &self.search_projections,
            )
            .await
        }
        .await;
        crate::owner_scope::finish_transaction(tx, result).await
    }

    async fn forgotten_nodes(
        &self,
        owner_scope: Option<&proxima_core::OwnerScope>,
        read_owners: &[OwnerId],
        memory_ids: &[proxima_core::MemoryId],
    ) -> Result<Vec<proxima_core::verbs::query::MemoryLineageNode>, StorageError> {
        if read_owners.is_empty() || memory_ids.is_empty() {
            return Ok(Vec::new());
        }
        let owner_ids: Vec<uuid::Uuid> = read_owners
            .iter()
            .copied()
            .map(OwnerId::into_inner)
            .collect();
        let ids: Vec<uuid::Uuid> = memory_ids
            .iter()
            .copied()
            .map(proxima_core::MemoryId::into_inner)
            .collect();
        let mut tx = crate::owner_scope::begin_owner_transaction(
            &self.pool,
            crate::owner_scope::require_owner_scope(owner_scope)?,
        )
        .await?;
        let result =
            async { verbs::query::forgotten_nodes_on_connection(&mut tx, &ids, &owner_ids).await }
                .await;
        crate::owner_scope::finish_transaction(tx, result).await
    }

    async fn sidecar_heads_by_key(
        &self,
        owner_scope: Option<&proxima_core::OwnerScope>,
        read_owners: &[OwnerId],
        query: &proxima_core::verbs::query::ResolvedSidecarHeads<'_>,
    ) -> Result<proxima_core::verbs::query::SidecarHeadsPage, StorageError> {
        let mut tx = crate::owner_scope::begin_owner_transaction(
            &self.pool,
            crate::owner_scope::require_owner_scope(owner_scope)?,
        )
        .await?;
        let result =
            verbs::query::sidecar_heads_by_key_on_connection(&mut tx, read_owners, query).await;
        crate::owner_scope::finish_transaction(tx, result).await
    }

    async fn owned_series_handle(
        &self,
        owner_scope: Option<&proxima_core::OwnerScope>,
        owner: OwnerId,
        schema_id: &proxima_core::SchemaId,
        sidecar_table: &str,
        columns: &[(&str, proxima_core::verbs::query::SidecarAtom)],
    ) -> Result<Option<uuid::Uuid>, StorageError> {
        let mut tx = crate::owner_scope::begin_owner_transaction(
            &self.pool,
            crate::owner_scope::require_owner_scope(owner_scope)?,
        )
        .await?;
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
        let mut tx = crate::owner_scope::begin_owner_transaction(
            &self.pool,
            crate::owner_scope::require_owner_scope(owner_scope)?,
        )
        .await?;
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
        read_owners: &[OwnerId],
        memory_ids: &[MemoryId],
        schemas: &[MemorySchemaSpec],
    ) -> Result<Vec<MemorySnapshot>, StorageError> {
        let mut tx = crate::owner_scope::begin_owner_transaction(
            &self.pool,
            crate::owner_scope::require_owner_scope(owner_scope)?,
        )
        .await?;
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

    async fn load_series_head(
        &self,
        owner_scope: Option<&proxima_core::OwnerScope>,
        read_owners: &[OwnerId],
        memory_id: MemoryId,
    ) -> Result<Option<MemoryId>, StorageError> {
        let mut tx = crate::owner_scope::begin_owner_transaction(
            &self.pool,
            crate::owner_scope::require_owner_scope(owner_scope)?,
        )
        .await?;
        let result =
            verbs::consolidate::load_series_head_on_connection(&mut tx, read_owners, memory_id)
                .await;
        crate::owner_scope::finish_transaction(tx, result).await
    }
}
