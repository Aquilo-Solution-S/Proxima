use proxima_core::read_models::{MemorySnapshot, SidecarSpec};
use proxima_core::storage_ports::{
    MemoryAuthoringPort, MemoryInspectPort, MemoryReadPort, OwnerWritePermit,
};
use proxima_core::verbs::query::{
    MemoryLineageRequest, MemoryLineageResponse, MemorySearchPage, MemorySearchRequest,
    QueryRequest, QueryResponse,
};
use proxima_core::{
    AuthorDerivedOutcome, AuthorDerivedRequest, Edge, EdgeKind, EdgeTargetProjection,
    FactSourceBatchRow, MemoryGraphPayloadRow, MemoryId, MemoryKindRow, Owner, OwnerRef,
    SourceBatchId, StorageError,
};

use crate::error::{internal, with_bounded_retry};
use crate::verbs::edge_index::{PgEndpointKind, endpoint_from_columns};
use crate::{PgStorage, verbs};

type NeighborMemoryEdgeTuple = (
    PgEndpointKind,
    uuid::Uuid,
    PgEndpointKind,
    uuid::Uuid,
    EdgeKind,
    time::OffsetDateTime,
    bool,
);

/// The neighbor window (sql-sweep S5). `touching` filters the edges scan on
/// the RAW endpoint columns before the head resolution, so it rides
/// `idx_edges_source`/`idx_edges_target` instead of resolving every edge.
///
/// The prefilter is an exact superset of the resolved predicate it guards:
/// a resolved endpoint id equals a requested id only when the raw column
/// equals it (no `fact_entities` match) or when the raw column is a
/// fact-entity id whose `current_memory_id` is requested — precisely the
/// ids `head_probe` collects (via `idx_fact_entities_current_memory`,
/// migration 0017). The original resolved-column filter is kept verbatim as
/// the residual, so the row set cannot change. Prior art: `PostgreSQL` docs
/// on `ScalarArrayOp` index quals — `= ANY(array)` over a base column is an
/// index condition, `= ANY` over a `COALESCE` of joined columns is not.
const NEIGHBOR_MEMORY_EDGES_SQL: &str = "
WITH read_set(owner_kind, owner_id) AS (
    SELECT * FROM unnest($1::proxima_core.owner_ref_kind[], $2::uuid[])
),
head_probe AS (
    SELECT COALESCE(array_agg(fact_entity_id), '{}') AS ids
      FROM proxima_core.fact_entities
     WHERE current_memory_id = ANY($5::uuid[])
),
touching AS (
    SELECT e.source_id, e.target_id, e.kind, e.created_at,
           CASE WHEN e.source_kind = 'FactEntityHead'::proxima_core.edge_endpoint_kind
                THEN 'Fact'::proxima_core.edge_endpoint_kind ELSE e.source_kind END
                AS source_kind,
           CASE WHEN e.target_kind = 'FactEntityHead'::proxima_core.edge_endpoint_kind
                THEN 'Fact'::proxima_core.edge_endpoint_kind ELSE e.target_kind END
                AS target_kind,
           COALESCE(sfe.current_memory_id, e.source_id) AS source_entity_id,
           COALESCE(tfe.current_memory_id, e.target_id) AS target_entity_id
      FROM proxima_core.edges e
      LEFT JOIN proxima_core.fact_entities sfe
        ON e.source_kind = 'FactEntityHead'::proxima_core.edge_endpoint_kind
       AND sfe.fact_entity_id = e.source_id
      LEFT JOIN proxima_core.fact_entities tfe
        ON e.target_kind = 'FactEntityHead'::proxima_core.edge_endpoint_kind
       AND tfe.fact_entity_id = e.target_id
     WHERE e.source_id = ANY($5::uuid[])
        OR e.target_id = ANY($5::uuid[])
        OR e.source_id = ANY((SELECT ids FROM head_probe)::uuid[])
        OR e.target_id = ANY((SELECT ids FROM head_probe)::uuid[])
)
SELECT t.source_kind, t.source_entity_id, t.target_kind, t.target_entity_id,
       t.kind, t.created_at,
       EXISTS (
           SELECT 1
             FROM read_set rs
            WHERE rs.owner_kind = COALESCE(tm.owner_kind, tg.owner_kind)
              AND rs.owner_id IS NOT DISTINCT FROM COALESCE(tm.owner_id, tg.owner_id)
       ) AS target_visible
  FROM touching t
  LEFT JOIN proxima_core.memories sm ON sm.memory_id = t.source_entity_id
  LEFT JOIN proxima_core.goals sg ON sg.goal_id = t.source_entity_id
  LEFT JOIN proxima_core.memories tm ON tm.memory_id = t.target_entity_id
  LEFT JOIN proxima_core.goals tg ON tg.goal_id = t.target_entity_id
 WHERE EXISTS (
           SELECT 1
             FROM read_set rs
            WHERE rs.owner_kind = COALESCE(sm.owner_kind, sg.owner_kind)
              AND rs.owner_id IS NOT DISTINCT FROM COALESCE(sm.owner_id, sg.owner_id)
       )
   AND (t.source_entity_id = ANY($5::uuid[]) OR t.target_entity_id = ANY($5::uuid[]))
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
 ORDER BY t.created_at DESC, t.source_kind DESC, t.source_id DESC,
          t.target_kind DESC, t.target_id DESC, t.kind DESC
 LIMIT $6
";

/// The neighbor-window statement the given tuning selects, for plan and
/// equivalence assertions in tests. Same cfg gate as the search and claim
/// `*_sql_for_tests` exports.
#[cfg(any(test, feature = "test-fixtures", debug_assertions))]
#[doc(hidden)]
#[must_use]
pub fn neighbor_memory_edges_sql_for_tests() -> &'static str {
    NEIGHBOR_MEMORY_EDGES_SQL
}

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
    ) -> Result<Vec<Edge>, StorageError> {
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
                    source_kind,
                    source_id,
                    target_kind,
                    target_id,
                    kind,
                    created_at,
                    target_visible,
                )| Edge {
                    source: endpoint_from_columns(source_kind, source_id),
                    // A readable edge whose target the reader may not see is
                    // returned with the endpoint withheld, not suppressed:
                    // redaction must not rewrite the shape of the graph.
                    target: if target_visible {
                        EdgeTargetProjection::visible(endpoint_from_columns(target_kind, target_id))
                    } else {
                        EdgeTargetProjection::Redacted
                    },
                    kind,
                    created_at,
                },
            )
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
        verbs::query::search_memories(&self.pool, req, projections, &self.tuning).await
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
}
