use std::collections::HashMap;

use proxima_core::change_event::{EdgeTargetProjection, EntityRef};
use proxima_core::verbs::query::{
    EdgeExistsRequest, EdgeExistsResponse, EdgeFilter, EdgePayloadSpec, EdgeReadCursor,
    EdgeReadRequest, EdgeReadResponse, EdgeRow, QueryRequest,
};
use proxima_core::verbs::schema::{PayloadKind, SchemaInfo};
use proxima_core::{EdgeId, OwnerRef, SidecarPayload, StorageError};
use sqlx::PgPool;

use crate::error::internal;
use crate::sidecars::{PgSidecarKey, PgSidecarReadCtx, PgSidecarRegistryFrozen};

use super::rows::{EdgeRowDb, edge_row_from_db};
use super::{
    entity_owner_union, read_owner_columns, read_owner_predicate, resolve_heads_by_fact_entity_id,
};

/// Hard upper bound on edges returned by snapshot-edge mode.
/// Decoupled from `QueryRequest::limit`, which sizes the node window.
pub const MAX_SNAPSHOT_EDGES: usize = 50_000;

fn read_edges_sql() -> String {
    format!(
        "
WITH edge_heads AS (
    SELECT e.edge_id, e.relation, e.relation_class,
           e.source_kind, e.target_kind,
           COALESCE(e.source_memory_id, sfe.current_memory_id) AS source_memory_id,
           e.source_goal_id, e.source_fact_entity_id,
           COALESCE(e.target_memory_id, tfe.current_memory_id) AS target_memory_id,
           e.target_goal_id, e.target_fact_entity_id,
           e.created_at,
           COALESCE(e.source_memory_id, e.source_goal_id, sfe.current_memory_id) AS source_entity_id,
           COALESCE(e.target_memory_id, e.target_goal_id, tfe.current_memory_id) AS target_entity_id,
           (etr.edge_id IS NOT NULL
            OR (e.target_memory_id IS NOT NULL AND tm.memory_id IS NULL)
            OR (e.target_goal_id IS NOT NULL AND tg.goal_id IS NULL)
            OR (e.target_fact_entity_id IS NOT NULL AND tfe.fact_entity_id IS NULL)) AS target_unavailable
      FROM proxima_core.edges e
      LEFT JOIN proxima_core.fact_entities sfe
        ON sfe.fact_entity_id = e.source_fact_entity_id
      LEFT JOIN proxima_core.fact_entities tfe
        ON tfe.fact_entity_id = e.target_fact_entity_id
      LEFT JOIN proxima_core.memories tm
        ON tm.memory_id = e.target_memory_id
      LEFT JOIN proxima_core.goals tg
        ON tg.goal_id = e.target_goal_id
      LEFT JOIN proxima_core.compliance_edge_target_redactions etr
        ON etr.edge_id = e.edge_id
     WHERE ($5::boolean = false OR e.edge_id = ANY($6::uuid[]))
       AND ($7::text IS NULL OR e.relation = $7)
       AND ($8::uuid IS NULL OR e.source_memory_id = $8)
       AND ($9::uuid IS NULL OR e.source_goal_id = $9)
       AND ($10::uuid IS NULL OR e.source_fact_entity_id = $10)
       AND ($11::uuid IS NULL OR e.target_memory_id = $11)
       AND ($12::uuid IS NULL OR e.target_goal_id = $12)
       AND ($13::uuid IS NULL OR e.target_fact_entity_id = $13)
       AND ($14::timestamptz IS NULL OR (e.created_at, e.edge_id) < ($14, $15))
),
visible AS (
    SELECT edge_heads.*,
           EXISTS (
               SELECT 1
                 FROM {entity_owner_union} seo
                 JOIN unnest($1::proxima_core.owner_ref_kind[], $2::uuid[]) AS rs(kind, id)
                   ON {source_read_owner_predicate}
                WHERE seo.entity_id = edge_heads.source_entity_id
           ) AS source_readable,
           (NOT edge_heads.target_unavailable AND EXISTS (
               SELECT 1
                 FROM {entity_owner_union} teo
                 JOIN unnest($1::proxima_core.owner_ref_kind[], $2::uuid[]) AS rs(kind, id)
                   ON {target_read_owner_predicate}
                WHERE teo.entity_id = edge_heads.target_entity_id
           )) AS target_visible,
           EXISTS (
               SELECT 1
                 FROM {entity_owner_union} weo
                WHERE weo.entity_id = edge_heads.source_entity_id
                  AND weo.owner_kind = $3
                  AND weo.owner_id IS NOT DISTINCT FROM $4
           ) AS source_world_visible
      FROM edge_heads
)
SELECT edge_id, relation, relation_class,
       source_kind, target_kind, created_at,
       source_memory_id, source_goal_id, source_fact_entity_id,
       target_memory_id, target_goal_id, target_fact_entity_id,
       target_visible, target_unavailable
  FROM visible
 WHERE source_readable
   AND NOT (source_world_visible AND NOT target_visible)
   AND (($11::uuid IS NULL AND $12::uuid IS NULL AND $13::uuid IS NULL) OR target_visible)
 ORDER BY created_at DESC, edge_id DESC
 LIMIT $16
",
        entity_owner_union = entity_owner_union(),
        source_read_owner_predicate = read_owner_predicate("seo", "rs"),
        target_read_owner_predicate = read_owner_predicate("teo", "rs"),
    )
}

#[derive(Debug, Clone, Copy, Default)]
struct EndpointSql {
    memory: Option<uuid::Uuid>,
    goal: Option<uuid::Uuid>,
    fact_entity: Option<uuid::Uuid>,
}

impl From<Option<EntityRef>> for EndpointSql {
    fn from(value: Option<EntityRef>) -> Self {
        match value {
            Some(EntityRef::Memory(id)) => Self {
                memory: Some(id.into_inner()),
                goal: None,
                fact_entity: None,
            },
            Some(EntityRef::Goal(id)) => Self {
                memory: None,
                goal: Some(id.into_inner()),
                fact_entity: None,
            },
            Some(EntityRef::FactEntity(id)) => Self {
                memory: None,
                goal: None,
                fact_entity: Some(id.into_inner()),
            },
            None => Self::default(),
        }
    }
}

pub(crate) async fn read_edges(
    pool: &PgPool,
    sidecars: &PgSidecarRegistryFrozen,
    read_owners: &[OwnerRef],
    req: &EdgeReadRequest,
    payload_specs: &[EdgePayloadSpec],
) -> Result<EdgeReadResponse, StorageError> {
    if read_owners.is_empty() {
        return Ok(EdgeReadResponse {
            edges: Vec::new(),
            next_cursor: None,
        });
    }
    let (read_owner_kinds, read_owner_ids) = read_owner_columns(read_owners);
    let (world_kind, world_id) =
        crate::access::owner_columns::owner_binds(&proxima_core::access::world());
    let edge_ids = req
        .edge_ids
        .iter()
        .copied()
        .map(EdgeId::into_inner)
        .collect::<Vec<_>>();
    let edge_ids_filter = !edge_ids.is_empty();
    let source = EndpointSql::from(req.filter.source);
    let target = EndpointSql::from(req.filter.target);
    let limit = usize::try_from(req.limit)
        .unwrap_or(MAX_SNAPSHOT_EDGES)
        .min(MAX_SNAPSHOT_EDGES);
    // Over-fetch one row past the page to detect a further page without a
    // second count query; the extra row is dropped before projection.
    let fetch_limit =
        i64::try_from(limit + 1).map_err(|err| StorageError::Internal(err.to_string()))?;
    // SQL-POLICY: fixed-fragment
    let mut rows = sqlx::query_as::<_, EdgeRowDb>(sqlx::AssertSqlSafe(read_edges_sql()))
        .bind(&read_owner_kinds)
        .bind(&read_owner_ids)
        .bind(world_kind)
        .bind(world_id)
        .bind(edge_ids_filter)
        .bind(&edge_ids)
        .bind(req.filter.relation.as_deref())
        .bind(source.memory)
        .bind(source.goal)
        .bind(source.fact_entity)
        .bind(target.memory)
        .bind(target.goal)
        .bind(target.fact_entity)
        .bind(req.cursor.map(|cursor| cursor.created_at))
        .bind(req.cursor.map(|cursor| cursor.edge_id.into_inner()))
        .bind(fetch_limit)
        .fetch_all(pool)
        .await
        .map_err(internal)?;
    let has_more = rows.len() > limit;
    rows.truncate(limit);
    hydrate_fact_entity_heads(pool, &mut rows).await?;
    let mut edges = rows
        .into_iter()
        .map(edge_row_from_db)
        .collect::<Result<Vec<_>, _>>()?;
    let next_cursor = if has_more {
        edges.last().map(|edge| EdgeReadCursor {
            created_at: edge.created_at,
            edge_id: EdgeId::new(edge.id),
        })
    } else {
        None
    };
    if req.include_payloads && !payload_specs.is_empty() {
        hydrate_edge_payloads(pool, sidecars, payload_specs, &mut edges).await?;
    }
    Ok(EdgeReadResponse { edges, next_cursor })
}

/// Attach typed sidecar payloads to edges whose relation declares a payload
/// schema. One batched load per spec whose relation occurs in the page; an
/// edge with no sidecar row keeps `payload: None`.
async fn hydrate_edge_payloads(
    pool: &PgPool,
    sidecars: &PgSidecarRegistryFrozen,
    payload_specs: &[EdgePayloadSpec],
    edges: &mut [EdgeRow],
) -> Result<(), StorageError> {
    for spec in payload_specs {
        let ids = edges
            .iter()
            .filter(|edge| edge.relation == spec.relation)
            .map(|edge| EdgeId::new(edge.id))
            .collect::<Vec<_>>();
        if ids.is_empty() {
            continue;
        }
        let key = PgSidecarKey::new(
            PayloadKind::Edge,
            spec.schema_id.clone(),
            spec.schema_version,
        );
        let loaded = sidecars
            .load_edge_payloads_batch(PgSidecarReadCtx::from(pool), &key, &ids)
            .await?;
        let by_id = loaded
            .into_iter()
            .map(|(edge_id, payload)| (edge_id.into_inner(), payload))
            .collect::<HashMap<uuid::Uuid, SidecarPayload>>();
        for edge in edges
            .iter_mut()
            .filter(|edge| edge.relation == spec.relation)
        {
            edge.payload = by_id.get(&edge.id).cloned();
        }
    }
    Ok(())
}

pub(crate) async fn edge_exists(
    pool: &PgPool,
    sidecars: &PgSidecarRegistryFrozen,
    read_owners: &[OwnerRef],
    req: &EdgeExistsRequest,
) -> Result<EdgeExistsResponse, StorageError> {
    let read = EdgeReadRequest {
        owner: req.owner,
        edge_ids: req.edge_ids.clone(),
        filter: req.filter.clone(),
        limit: 1,
        cursor: None,
        include_payloads: false,
    };
    let response = read_edges(pool, sidecars, read_owners, &read, &[]).await?;
    Ok(EdgeExistsResponse {
        exists: !response.edges.is_empty(),
    })
}

pub(super) async fn query_edges(
    pool: &PgPool,
    sidecars: &PgSidecarRegistryFrozen,
    req: &QueryRequest,
    read_owners: &[OwnerRef],
    visible_memory_ids: &[uuid::Uuid],
    visible_goal_ids: &[uuid::Uuid],
    schemas: &[SchemaInfo],
) -> Result<Vec<EdgeRow>, StorageError> {
    let edge_ids = req.edge_ids.clone();
    let id_hydration =
        !req.memory_ids.is_empty() || !req.goal_ids.is_empty() || !req.edge_ids.is_empty();

    if !edge_ids.is_empty() {
        return query_edges_by_id(pool, sidecars, req, &edge_ids, read_owners, schemas).await;
    }
    // Focused identity and entity-kind queries should not return graph
    // closure as a side effect. Atlas uses entity_kind = None to opt in.
    if id_hydration || req.entity_kind.is_some() {
        return Ok(Vec::new());
    }
    if visible_memory_ids.is_empty() && visible_goal_ids.is_empty() {
        return Ok(Vec::new());
    }
    query_edges_between_visible_nodes(pool, visible_memory_ids, visible_goal_ids).await
}

async fn query_edges_by_id(
    pool: &PgPool,
    sidecars: &PgSidecarRegistryFrozen,
    req: &QueryRequest,
    edge_ids: &[uuid::Uuid],
    read_owners: &[OwnerRef],
    schemas: &[SchemaInfo],
) -> Result<Vec<EdgeRow>, StorageError> {
    let (read_owner_kinds, read_owner_ids) = read_owner_columns(read_owners);
    let edge_ids = edge_ids.iter().copied().map(EdgeId::new).collect();
    // Access dimension: source-owned visibility with target redaction.
    let response = read_edges(
        pool,
        sidecars,
        read_owners,
        &EdgeReadRequest {
            owner: req.owner,
            edge_ids,
            filter: EdgeFilter::default(),
            limit: req.limit,
            cursor: None,
            include_payloads: false,
        },
        &[],
    )
    .await?;
    // Presence dimension: window readable endpoints against the request's
    // present-only / stateful-head visibility (the same machinery the node
    // query uses), so an edge to a tombstoned head is excluded in present-only
    // mode. Access-unreadable targets are already stubbed and stay as-is.
    let mut candidate_memory_ids = Vec::new();
    let mut candidate_goal_ids = Vec::new();
    let mut push_endpoint = |endpoint: &EntityRef| match endpoint {
        EntityRef::Memory(id) => candidate_memory_ids.push(id.into_inner()),
        EntityRef::Goal(id) => candidate_goal_ids.push(id.into_inner()),
        EntityRef::FactEntity(_) => {}
    };
    for edge in &response.edges {
        push_endpoint(&edge.source);
        if let EdgeTargetProjection::Visible { target } = edge.target {
            push_endpoint(&target);
        }
    }
    let (visible_memory_ids, visible_goal_ids) = super::memories::visible_ids_for(
        pool,
        req,
        &read_owner_kinds,
        &read_owner_ids,
        &candidate_memory_ids,
        &candidate_goal_ids,
        schemas,
    )
    .await?;
    let endpoint_present = |endpoint: &EntityRef| match endpoint {
        EntityRef::Memory(id) => visible_memory_ids.contains(&id.into_inner()),
        EntityRef::Goal(id) => visible_goal_ids.contains(&id.into_inner()),
        // Fact-entity endpoints resolve to a current head during hydration; the
        // edge's own presence is governed by its memory/goal endpoints.
        EntityRef::FactEntity(_) => true,
    };
    let edges = response
        .edges
        .into_iter()
        .filter(|edge| {
            endpoint_present(&edge.source)
                && match edge.target {
                    EdgeTargetProjection::Visible { target } => endpoint_present(&target),
                    EdgeTargetProjection::Redacted | EdgeTargetProjection::Unavailable => true,
                }
        })
        .collect();
    Ok(edges)
}

async fn query_edges_between_visible_nodes(
    pool: &PgPool,
    visible_memory_ids: &[uuid::Uuid],
    visible_goal_ids: &[uuid::Uuid],
) -> Result<Vec<EdgeRow>, StorageError> {
    let rows = sqlx::query_as::<_, EdgeRowDb>(
        "SELECT e.edge_id, e.relation, e.relation_class,
                e.source_kind, e.target_kind, e.created_at,
                COALESCE(e.source_memory_id, sfe.current_memory_id) AS source_memory_id,
                e.source_goal_id, e.source_fact_entity_id,
                COALESCE(e.target_memory_id, tfe.current_memory_id) AS target_memory_id,
                e.target_goal_id, e.target_fact_entity_id,
                true AS target_visible,
                false AS target_unavailable
         FROM proxima_core.edges e
         LEFT JOIN proxima_core.fact_entities sfe
           ON sfe.fact_entity_id = e.source_fact_entity_id
         LEFT JOIN proxima_core.fact_entities tfe
           ON tfe.fact_entity_id = e.target_fact_entity_id
         WHERE (e.source_memory_id = ANY($1::uuid[])
                OR e.source_goal_id = ANY($2::uuid[])
                OR sfe.current_memory_id = ANY($1::uuid[]))
           AND (e.target_memory_id = ANY($1::uuid[])
                OR e.target_goal_id = ANY($2::uuid[])
                OR tfe.current_memory_id = ANY($1::uuid[]))
         ORDER BY e.created_at DESC
         LIMIT $3",
    )
    .bind(visible_memory_ids)
    .bind(visible_goal_ids)
    .bind(i64::try_from(MAX_SNAPSHOT_EDGES).expect("MAX_SNAPSHOT_EDGES fits in i64"))
    .fetch_all(pool)
    .await
    .map_err(internal)?;
    rows.into_iter().map(edge_row_from_db).collect()
}

async fn hydrate_fact_entity_heads(
    pool: &PgPool,
    rows: &mut [EdgeRowDb],
) -> Result<(), StorageError> {
    let mut ids = rows
        .iter()
        .flat_map(|row| {
            [
                row.source_fact_entity_id,
                if row.target_unavailable {
                    None
                } else {
                    row.target_fact_entity_id
                },
            ]
        })
        .flatten()
        .collect::<Vec<_>>();
    if ids.is_empty() {
        return Ok(());
    }
    ids.sort_unstable();
    ids.dedup();

    let resolved = resolve_heads_by_fact_entity_id(pool, &ids).await?;

    for row in rows {
        if let Some(id) = row.source_fact_entity_id {
            let head = resolved
                .get(&id)
                .copied()
                .ok_or_else(|| StorageError::Internal(format!("fact entity {id} has no head")))?;
            row.source_memory_id = Some(head);
        }
        if !row.target_unavailable
            && let Some(id) = row.target_fact_entity_id
        {
            let head = resolved
                .get(&id)
                .copied()
                .ok_or_else(|| StorageError::Internal(format!("fact entity {id} has no head")))?;
            row.target_memory_id = Some(head);
        }
    }
    Ok(())
}
