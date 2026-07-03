use proxima_core::change_event::{EdgeTargetProjection, EntityRef};
use proxima_core::verbs::query::{
    EdgeExistsRequest, EdgeExistsResponse, EdgeFilter, EdgeReadRequest, EdgeReadResponse, EdgeRow,
    QueryRequest,
};
use proxima_core::verbs::schema::SchemaInfo;
use proxima_core::{EdgeId, OwnerRef, OwnerRefKind, StorageError};
use sqlx::PgPool;

use crate::error::internal;

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
       source_memory_id, source_goal_id, source_fact_entity_id,
       target_memory_id, target_goal_id, target_fact_entity_id,
       target_visible, target_unavailable
  FROM visible
 WHERE source_readable
   AND NOT (source_world_visible AND NOT target_visible)
   AND (($11::uuid IS NULL AND $12::uuid IS NULL AND $13::uuid IS NULL) OR target_visible)
 ORDER BY created_at DESC
 LIMIT $14
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
    read_owners: &[OwnerRef],
    req: &EdgeReadRequest,
) -> Result<EdgeReadResponse, StorageError> {
    if read_owners.is_empty() {
        return Ok(EdgeReadResponse { edges: Vec::new() });
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
    let limit = i64::from(
        req.limit
            .min(u32::try_from(MAX_SNAPSHOT_EDGES).expect("MAX_SNAPSHOT_EDGES fits in u32")),
    );
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
        .bind(limit)
        .fetch_all(pool)
        .await
        .map_err(internal)?;
    hydrate_fact_entity_heads(pool, &mut rows).await?;
    let edges = rows
        .into_iter()
        .map(edge_row_from_db)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(EdgeReadResponse { edges })
}

pub(crate) async fn edge_exists(
    pool: &PgPool,
    read_owners: &[OwnerRef],
    req: &EdgeExistsRequest,
) -> Result<EdgeExistsResponse, StorageError> {
    let read = EdgeReadRequest {
        owner: req.owner,
        edge_ids: req.edge_ids.clone(),
        filter: req.filter.clone(),
        limit: 1,
    };
    let response = read_edges(pool, read_owners, &read).await?;
    Ok(EdgeExistsResponse {
        exists: !response.edges.is_empty(),
    })
}

pub(super) async fn query_edges(
    pool: &PgPool,
    req: &QueryRequest,
    read_owner_kinds: &[OwnerRefKind],
    read_owner_ids: &[Option<uuid::Uuid>],
    visible_memory_ids: &[uuid::Uuid],
    visible_goal_ids: &[uuid::Uuid],
    schemas: &[SchemaInfo],
) -> Result<Vec<EdgeRow>, StorageError> {
    let edge_ids = req.edge_ids.clone();
    let id_hydration =
        !req.memory_ids.is_empty() || !req.goal_ids.is_empty() || !req.edge_ids.is_empty();

    if !edge_ids.is_empty() {
        return query_edges_by_id(
            pool,
            req,
            &edge_ids,
            read_owner_kinds,
            read_owner_ids,
            schemas,
        )
        .await;
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
    req: &QueryRequest,
    edge_ids: &[uuid::Uuid],
    read_owner_kinds: &[OwnerRefKind],
    read_owner_ids: &[Option<uuid::Uuid>],
    schemas: &[SchemaInfo],
) -> Result<Vec<EdgeRow>, StorageError> {
    let read_owners = read_owner_kinds
        .iter()
        .zip(read_owner_ids.iter())
        .map(|(kind, id)| {
            kind.with_uuid(*id)
                .ok_or_else(|| StorageError::Internal("invalid read owner_ref shape".to_string()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let edge_ids = edge_ids.iter().copied().map(EdgeId::new).collect();
    // Access dimension: source-owned visibility with target redaction.
    let response = read_edges(
        pool,
        &read_owners,
        &EdgeReadRequest {
            owner: req.owner,
            edge_ids,
            filter: EdgeFilter::default(),
            limit: req.limit,
        },
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
        read_owner_kinds,
        read_owner_ids,
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
