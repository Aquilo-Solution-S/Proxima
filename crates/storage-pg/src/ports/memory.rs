use proxima_core::change_event::{EdgeTargetProjection, EntityRef};
use proxima_core::read_models::{MemorySnapshot, SidecarSpec};
use proxima_core::storage_ports::{
    MemoryAuthoringPort, MemoryInspectPort, MemoryReadPort, OwnerWritePermit,
};
use proxima_core::verbs::query::{
    MemoryLineageRequest, MemoryLineageResponse, MemorySearchPage, MemorySearchRequest,
    QueryRequest, QueryResponse,
};
use proxima_core::{
    AuthorDerivedOutcome, AuthorDerivedRequest, DerivedEdgeSpec, EdgeEndpointKindRow, EdgeId,
    FactSourceBatchRow, MemoryDependency, MemoryGraphPayloadRow, MemoryId, MemoryKindRow,
    NeighborEdgeRow, Owner, OwnerRef, SourceBatchId, StorageError,
};

use super::edge_draft_from_spec;
use crate::error::{internal, with_bounded_retry};
use crate::{PgStorage, verbs};

type NeighborMemoryEdgeTuple = (
    uuid::Uuid,
    String,
    proxima_core::EntityKind,
    Option<uuid::Uuid>,
    proxima_core::EntityKind,
    Option<uuid::Uuid>,
    bool,
    bool,
);

const NEIGHBOR_MEMORY_EDGES_SQL: &str = "
WITH read_set(owner_kind, owner_id) AS (
    SELECT * FROM unnest($1::proxima_core.owner_ref_kind[], $2::uuid[])
)
SELECT e.edge_id, e.relation,
       e.source_kind,
       COALESCE(e.source_memory_id, sfe.current_memory_id) AS source_memory_id,
       e.target_kind,
       COALESCE(e.target_memory_id, tfe.current_memory_id) AS target_memory_id,
       EXISTS (
           SELECT 1
             FROM read_set rs
            WHERE rs.owner_kind = COALESCE(tm.owner_kind, tg.owner_kind)
              AND rs.owner_id IS NOT DISTINCT FROM COALESCE(tm.owner_id, tg.owner_id)
       ) AS target_visible,
       COALESCE(sm.owner_kind, sg.owner_kind) = $3
       AND COALESCE(sm.owner_id, sg.owner_id) IS NOT DISTINCT FROM $4 AS source_world_visible
  FROM proxima_core.edges e
  LEFT JOIN proxima_core.fact_entities sfe
    ON sfe.fact_entity_id = e.source_fact_entity_id
  LEFT JOIN proxima_core.fact_entities tfe
    ON tfe.fact_entity_id = e.target_fact_entity_id
  LEFT JOIN proxima_core.memories sm
    ON sm.memory_id = COALESCE(e.source_memory_id, sfe.current_memory_id)
  LEFT JOIN proxima_core.goals sg
    ON sg.goal_id = e.source_goal_id
  LEFT JOIN proxima_core.memories tm
    ON tm.memory_id = COALESCE(e.target_memory_id, tfe.current_memory_id)
  LEFT JOIN proxima_core.goals tg
    ON tg.goal_id = e.target_goal_id
 WHERE EXISTS (
           SELECT 1
             FROM read_set rs
            WHERE rs.owner_kind = COALESCE(sm.owner_kind, sg.owner_kind)
              AND rs.owner_id IS NOT DISTINCT FROM COALESCE(sm.owner_id, sg.owner_id)
       )
   AND (e.source_memory_id = ANY($5::uuid[])
        OR e.target_memory_id = ANY($5::uuid[])
        OR sfe.current_memory_id = ANY($5::uuid[])
        OR tfe.current_memory_id = ANY($5::uuid[]))
   AND NOT (
        COALESCE(sm.owner_kind, sg.owner_kind) = $3
        AND COALESCE(sm.owner_id, sg.owner_id) IS NOT DISTINCT FROM $4
        AND NOT EXISTS (
            SELECT 1
              FROM read_set rs
             WHERE rs.owner_kind = COALESCE(tm.owner_kind, tg.owner_kind)
               AND rs.owner_id IS NOT DISTINCT FROM COALESCE(tm.owner_id, tg.owner_id)
        )
   )
 ORDER BY e.edge_id DESC
 LIMIT $6
";

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
        // and any edges mint fresh ids on the fresh attempt.
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
                supersedes: req.supersedes,
                lexical_language: req.lexical_language,
                embedding: req.embedding.clone(),
                embedding_model_id: req.embedding_model_id,
            };
            // ONE validator for both derived-write paths (this engine port and
            // the flavor-SDK `append_derived_with_edges_in_tx`): the port used
            // to carry its own near-duplicate proof validation here, which
            // silently missed the created_at strict-time gate.
            verbs::derive_append::validate_derived_draft_edges_in_tx(&mut tx, &draft, req.edges)
                .await?;
            let sidecars = self.sidecars.clone();
            let sidecar_payload = req.sidecar_payload.clone();
            let outcome = verbs::derive_append::append_derived_in_tx(
                &mut tx,
                permit,
                &draft,
                move |tx, outcome| {
                    Box::pin(async move {
                        sidecars
                            .insert_memory_sidecar(tx, outcome.memory_id, &sidecar_payload)
                            .await
                    })
                },
            )
            .await?;
            let mut edge_count = 0;
            if outcome.idempotent_replay {
                verbs::derive_append::validate_derived_edge_replay_equivalent(
                    &mut tx, &draft, req.edges,
                )
                .await?;
            } else {
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
        })
        .await
    }

    async fn append_memory_edge(
        &self,
        edge: &DerivedEdgeSpec<'_>,
        permit: &OwnerWritePermit,
        _proof: proxima_core::storage_ports::EdgeWriteProof,
    ) -> Result<EdgeId, StorageError> {
        if edge.owner != permit.owner() {
            return Err(StorageError::ConstraintViolation(
                "edge owner does not match owner write permit".into(),
            ));
        }
        if edge.authorship_kind.is_operator() {
            return Err(StorageError::ConstraintViolation(
                "operator-authored edges require an operator proof-carrier write path".into(),
            ));
        }
        // Retry the whole begin→body→commit on transient deadlock/
        // serialization. A retryable error fully rolls the transaction back;
        // the edge id is minted per attempt, so a re-run inserts exactly one edge.
        with_bounded_retry(move || async move {
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
                        Box::pin(async move {
                            sidecars.insert_edge_sidecar(tx, edge_id, &payload).await
                        })
                    },
                )
                .await?;
            } else {
                verbs::edge_append::append_edge_in_tx(tx.as_mut(), &draft).await?;
            }
            tx.commit().await.map_err(crate::error::map_err)?;
            Ok(edge_id)
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
        let (owner_kind, owner_id) = owner.columns();
        let rows: Vec<(uuid::Uuid, Option<proxima_core::EntityKind>)> = sqlx::query_as(
            "SELECT m.memory_id, m.kind
             FROM proxima_core.memories m
             WHERE m.owner_kind = $1
               AND m.owner_id IS NOT DISTINCT FROM $2
               AND m.memory_id = ANY($3::uuid[])",
        )
        .bind(owner_kind)
        .bind(owner_id)
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

    async fn load_fact_source_batches(
        &self,
        _owner: &Owner,
        memory_ids: &[MemoryId],
    ) -> Result<Vec<FactSourceBatchRow>, StorageError> {
        if memory_ids.is_empty() {
            return Ok(Vec::new());
        }
        let ids = memory_ids
            .iter()
            .copied()
            .map(MemoryId::into_inner)
            .collect::<Vec<_>>();
        let rows: Vec<(uuid::Uuid, uuid::Uuid)> = sqlx::query_as(
            "SELECT m.memory_id, fr.source_batch_id
             FROM proxima_core.memories m
             JOIN proxima_core.fact_receipts fr ON fr.receipt_id = m.receipt_id
             WHERE m.kind IS NULL
               AND m.tombstoned_at IS NULL
               AND m.memory_id = ANY($1::uuid[])",
        )
        .bind(&ids)
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;
        Ok(rows
            .into_iter()
            .map(|(memory_id, source_batch_id)| FactSourceBatchRow {
                memory_id: MemoryId::new(memory_id),
                source_batch_id: SourceBatchId::new(source_batch_id),
            })
            .collect())
    }

    async fn load_memory_edge_ids(
        &self,
        _owner: &Owner,
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
        let rows: Vec<uuid::Uuid> = sqlx::query_scalar(
            "SELECT edge_id
             FROM proxima_core.edges
             WHERE relation = $1
               AND source_memory_id = $2
               AND target_memory_id = ANY($3::uuid[])
             ORDER BY edge_id DESC",
        )
        .bind(relation)
        .bind(source_memory_id.into_inner())
        .bind(&target_ids)
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;
        Ok(rows.into_iter().map(EdgeId::new).collect())
    }
}

#[async_trait::async_trait]
impl MemoryReadPort for PgStorage {
    async fn load_fact_text(
        &self,
        owner: &Owner,
        memory_id: MemoryId,
    ) -> Result<Option<String>, StorageError> {
        verbs::fact_embeddings::load_fact_text(&self.pool, owner, memory_id).await
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
        let (owner_kind, owner_id) = owner.columns();
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
             WHERE m.owner_kind = $1
               AND m.owner_id IS NOT DISTINCT FROM $2
               AND m.memory_id = ANY($3::uuid[])",
        )
        .bind(owner_kind)
        .bind(owner_id)
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
        read_owners: &[OwnerRef],
        memory_ids: &[MemoryId],
        limit: usize,
    ) -> Result<Vec<NeighborEdgeRow>, StorageError> {
        if memory_ids.is_empty() {
            return Ok(Vec::new());
        }
        if read_owners.is_empty() {
            return Ok(Vec::new());
        }
        let ids = memory_ids
            .iter()
            .copied()
            .map(MemoryId::into_inner)
            .collect::<Vec<_>>();
        let limit = i64::try_from(limit).map_err(|err| StorageError::Internal(err.to_string()))?;
        let (read_owner_kinds, read_owner_ids) = verbs::query::read_owner_columns(read_owners);
        let (world_kind, world_id) =
            crate::access::owner_columns::owner_binds(&proxima_core::access::world());
        let rows: Vec<NeighborMemoryEdgeTuple> = {
            sqlx::query_as(NEIGHBOR_MEMORY_EDGES_SQL)
                .bind(&read_owner_kinds)
                .bind(&read_owner_ids)
                .bind(world_kind)
                .bind(world_id)
                .bind(&ids)
                .bind(limit)
                .fetch_all(&self.pool)
                .await
                .map_err(internal)?
        };
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
                    target_visible,
                    _source_world_visible,
                )| {
                    let target_memory_kind = target_visible.then_some(target_kind);
                    let target = if target_visible {
                        target_memory_id.map_or(EdgeTargetProjection::Unavailable, |id| {
                            EdgeTargetProjection::Visible {
                                target: EntityRef::Memory(MemoryId::new(id)),
                            }
                        })
                    } else {
                        EdgeTargetProjection::Redacted
                    };
                    NeighborEdgeRow {
                        edge_id: EdgeId::new(edge_id),
                        relation,
                        source_kind,
                        source_memory_id: source_memory_id.map(MemoryId::new),
                        target_memory_kind,
                        target,
                    }
                },
            )
            .collect())
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
                target_kind: Some(target_kind),
            })
            .collect())
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
        verbs::query::search_memories(&self.pool, req, projections).await
    }

    async fn walk_memory_lineage(
        &self,
        read_owners: &[OwnerRef],
        req: &MemoryLineageRequest,
    ) -> Result<MemoryLineageResponse, StorageError> {
        verbs::query::walk_memory_lineage(&self.pool, read_owners, req).await
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

    async fn list_memory_dependencies(
        &self,
        owner: &Owner,
        source_memory_id: MemoryId,
    ) -> Result<Vec<MemoryDependency>, StorageError> {
        verbs::consolidate::list_memory_dependencies(&self.pool, owner, source_memory_id).await
    }
}
