use std::collections::{BTreeMap, BTreeSet};

use proxima_core::change_event::{EdgeTargetProjection, EntityRef};
use proxima_core::verbs::query::{
    EntityKind, MemoryLineageDirection, MemoryLineageEdge, MemoryLineageNode, MemoryLineageRequest,
    MemoryLineageResponse,
};
use proxima_core::{
    MemoryId, OwnerRef, OwnerRefKind, RelationClass, SchemaId, StorageError, WakeChainDepth,
};
use sqlx::PgPool;

use crate::error::internal;

use super::read_owner_columns;

#[derive(Debug, sqlx::FromRow)]
struct EdgeWalkRow {
    distance: i32,
    edge_id: uuid::Uuid,
    relation: String,
    relation_class: RelationClass,
    source_kind: EntityKind,
    source_memory_id: uuid::Uuid,
    target_memory_id: uuid::Uuid,
    next_memory_id: uuid::Uuid,
    next_readable: bool,
}

#[derive(Debug, sqlx::FromRow)]
struct NodeRow {
    memory_id: uuid::Uuid,
    kind: Option<EntityKind>,
    schema_id: String,
    snippet: Option<String>,
    wake_chain_depth: i16,
}

pub(crate) async fn walk_memory_lineage(
    pool: &PgPool,
    read_owners: &[OwnerRef],
    req: &MemoryLineageRequest,
) -> Result<MemoryLineageResponse, StorageError> {
    if read_owners.is_empty() {
        return Ok(MemoryLineageResponse {
            nodes: Vec::new(),
            edges: Vec::new(),
            truncated: false,
        });
    }
    let limit = req.limit.min(200);
    let depth = req.depth.min(8);
    let (read_owner_kinds, read_owner_ids) = read_owner_columns(read_owners);

    let reader_id = req
        .reader_personality_instance_id
        .map(proxima_core::PersonalityInstanceId::into_inner);

    if !start_memory_visible(
        pool,
        &read_owner_kinds,
        &read_owner_ids,
        req.start_memory_id,
        reader_id,
    )
    .await?
    {
        return Ok(MemoryLineageResponse {
            nodes: Vec::new(),
            edges: Vec::new(),
            truncated: false,
        });
    }

    let edge_rows = walk_edges(
        pool,
        req,
        &read_owner_kinds,
        &read_owner_ids,
        depth,
        limit.saturating_add(1),
        reader_id,
    )
    .await?;
    let truncated = edge_rows.len() > usize::try_from(limit).unwrap_or(200);
    let edge_rows: Vec<_> = edge_rows
        .into_iter()
        .take(usize::try_from(limit).unwrap_or(200))
        .collect();

    let mut distances = BTreeMap::from([(req.start_memory_id.into_inner(), 0_u8)]);
    for row in &edge_rows {
        let distance = u8::try_from(row.distance).unwrap_or(u8::MAX);
        if row.next_readable {
            distances
                .entry(row.next_memory_id)
                .and_modify(|prior| *prior = (*prior).min(distance))
                .or_insert(distance);
        }
    }
    let memory_ids: Vec<_> = distances.keys().copied().collect();
    let node_rows = load_nodes(
        pool,
        &read_owner_kinds,
        &read_owner_ids,
        &memory_ids,
        reader_id,
    )
    .await?;
    let visible_ids: BTreeSet<_> = node_rows.iter().map(|row| row.memory_id).collect();
    let nodes = node_rows
        .into_iter()
        .map(|row| MemoryLineageNode {
            memory_id: MemoryId::new(row.memory_id),
            kind: row.kind.unwrap_or(EntityKind::Fact),
            schema_id: SchemaId::new(row.schema_id),
            snippet: row.snippet.unwrap_or_default(),
            wake_chain_depth: WakeChainDepth::new(u16::try_from(row.wake_chain_depth).unwrap_or(0)),
            distance: *distances.get(&row.memory_id).unwrap_or(&0),
        })
        .collect();

    let edges = edge_rows
        .into_iter()
        .filter(|row| visible_ids.contains(&row.source_memory_id))
        .map(|row| MemoryLineageEdge {
            edge_id: row.edge_id,
            relation: row.relation,
            relation_class: row.relation_class.as_str().to_string(),
            source_kind: row.source_kind,
            source_memory_id: MemoryId::new(row.source_memory_id),
            target: if row.next_readable {
                EdgeTargetProjection::Visible {
                    target: EntityRef::Memory(MemoryId::new(row.target_memory_id)),
                }
            } else {
                EdgeTargetProjection::Redacted
            },
            distance: u8::try_from(row.distance).unwrap_or(u8::MAX),
        })
        .collect();

    Ok(MemoryLineageResponse {
        nodes,
        edges,
        truncated,
    })
}

async fn start_memory_visible(
    pool: &PgPool,
    read_owner_kinds: &[OwnerRefKind],
    read_owner_ids: &[Option<uuid::Uuid>],
    memory_id: MemoryId,
    reader_id: Option<uuid::Uuid>,
) -> Result<bool, StorageError> {
    let mut sql = String::from(
        "SELECT m.memory_id
             FROM proxima_core.memories m
             WHERE EXISTS (
                       SELECT 1
                         FROM (SELECT memory_id AS entity_id, owner_kind, owner_id FROM proxima_core.memories UNION ALL SELECT goal_id AS entity_id, owner_kind, owner_id FROM proxima_core.goals) eo
                         JOIN unnest($1::proxima_core.owner_ref_kind[], $2::uuid[]) AS rs(kind, id)
                           ON eo.owner_kind = rs.kind
                          AND eo.owner_id IS NOT DISTINCT FROM rs.id
                        WHERE eo.entity_id = m.memory_id
                   )
               AND m.memory_id = $3
               AND m.tombstoned_at IS NULL",
    );
    push_reader_visibility_filter(&mut sql, "m", reader_id.map(|_| 4));
    let mut query = sqlx::query_as::<_, (uuid::Uuid,)>(&sql)
        .bind(read_owner_kinds)
        .bind(read_owner_ids)
        .bind(memory_id.into_inner());
    if let Some(reader_id) = reader_id {
        query = query.bind(reader_id);
    }
    let present = query.fetch_optional(pool).await.map_err(internal)?;
    Ok(present.is_some())
}

async fn walk_edges(
    pool: &PgPool,
    req: &MemoryLineageRequest,
    read_owner_kinds: &[OwnerRefKind],
    read_owner_ids: &[Option<uuid::Uuid>],
    depth: u8,
    limit: u32,
    reader_id: Option<uuid::Uuid>,
) -> Result<Vec<EdgeWalkRow>, StorageError> {
    let template = match req.direction {
        MemoryLineageDirection::Ancestors => ANCESTORS_SQL,
        MemoryLineageDirection::Descendants => DESCENDANTS_SQL,
    };
    let reader_filter = reader_id
        .map(|_| reader_visibility_filter("m", 8))
        .unwrap_or_default();
    let sql = template.replace("{reader_filter}", &reader_filter);
    let (world_kind, world_id) =
        crate::access::owner_columns::owner_binds(&proxima_core::access::world());
    let mut query = sqlx::query_as::<_, EdgeWalkRow>(&sql)
        .bind(read_owner_kinds)
        .bind(read_owner_ids)
        .bind(world_kind)
        .bind(world_id)
        .bind(req.start_memory_id.into_inner())
        .bind(i32::from(depth))
        .bind(i64::from(limit));
    if let Some(reader_id) = reader_id {
        query = query.bind(reader_id);
    }
    query.fetch_all(pool).await.map_err(internal)
}

async fn load_nodes(
    pool: &PgPool,
    read_owner_kinds: &[OwnerRefKind],
    read_owner_ids: &[Option<uuid::Uuid>],
    memory_ids: &[uuid::Uuid],
    reader_id: Option<uuid::Uuid>,
) -> Result<Vec<NodeRow>, StorageError> {
    let mut sql = String::from(
        "SELECT m.memory_id,
                  m.kind,
                  m.schema_id,
                  left(COALESCE(m.text, ''), 480) AS snippet,
                  m.wake_chain_depth
             FROM proxima_core.memories m
             WHERE EXISTS (
                       SELECT 1
                         FROM (SELECT memory_id AS entity_id, owner_kind, owner_id FROM proxima_core.memories UNION ALL SELECT goal_id AS entity_id, owner_kind, owner_id FROM proxima_core.goals) eo
                         JOIN unnest($1::proxima_core.owner_ref_kind[], $2::uuid[]) AS rs(kind, id)
                           ON eo.owner_kind = rs.kind
                          AND eo.owner_id IS NOT DISTINCT FROM rs.id
                        WHERE eo.entity_id = m.memory_id
                   )
               AND m.memory_id = ANY($3::uuid[])
               AND m.tombstoned_at IS NULL",
    );
    push_reader_visibility_filter(&mut sql, "m", reader_id.map(|_| 4));
    let mut query = sqlx::query_as::<_, NodeRow>(&sql)
        .bind(read_owner_kinds)
        .bind(read_owner_ids)
        .bind(memory_ids);
    if let Some(reader_id) = reader_id {
        query = query.bind(reader_id);
    }
    let rows = query.fetch_all(pool).await.map_err(internal)?;

    Ok(rows)
}

fn push_reader_visibility_filter(sql: &mut String, alias: &str, reader_param: Option<usize>) {
    if let Some(param) = reader_param {
        sql.push_str(&reader_visibility_filter(alias, param));
    }
}

fn reader_visibility_filter(alias: &str, param: usize) -> String {
    format!(
        " AND (
            {alias}.kind IS NULL
            OR {alias}.personality_instance_id = ${param}
        )"
    )
}

const ANCESTORS_SQL: &str = "
WITH RECURSIVE read_set(kind, id) AS (
    SELECT * FROM unnest($1::proxima_core.owner_ref_kind[], $2::uuid[])
),
readable_memories AS (
    SELECT m.memory_id
      FROM proxima_core.memories m
     WHERE EXISTS (
               SELECT 1
                 FROM (SELECT memory_id AS entity_id, owner_kind, owner_id FROM proxima_core.memories UNION ALL SELECT goal_id AS entity_id, owner_kind, owner_id FROM proxima_core.goals) eo
                 JOIN read_set rs
                   ON eo.owner_kind = rs.kind
                  AND eo.owner_id IS NOT DISTINCT FROM rs.id
                WHERE eo.entity_id = m.memory_id
           )
       AND m.tombstoned_at IS NULL
       {reader_filter}
),
edge_endpoints AS (
    SELECT e.edge_id, e.relation, e.relation_class,
           e.source_kind, e.target_kind,
           COALESCE(e.source_memory_id, sfe.current_memory_id) AS source_memory_id,
           COALESCE(e.target_memory_id, tfe.current_memory_id) AS target_memory_id,
           COALESCE(e.source_memory_id, e.source_goal_id, sfe.current_memory_id) AS source_entity_id,
           COALESCE(e.target_memory_id, e.target_goal_id, tfe.current_memory_id) AS target_entity_id
      FROM proxima_core.edges e
      LEFT JOIN proxima_core.fact_entities sfe
        ON sfe.fact_entity_id = e.source_fact_entity_id
      LEFT JOIN proxima_core.fact_entities tfe
        ON tfe.fact_entity_id = e.target_fact_entity_id
),
edge_heads AS (
    SELECT edge_endpoints.*,
           EXISTS (
               SELECT 1 FROM readable_memories rm
                WHERE rm.memory_id = edge_endpoints.source_memory_id
           ) AS source_readable,
           EXISTS (
               SELECT 1 FROM readable_memories rm
                WHERE rm.memory_id = edge_endpoints.target_memory_id
           ) AS target_visible,
           EXISTS (
               SELECT 1
                 FROM (SELECT memory_id AS entity_id, owner_kind, owner_id FROM proxima_core.memories UNION ALL SELECT goal_id AS entity_id, owner_kind, owner_id FROM proxima_core.goals) weo
                WHERE weo.entity_id = edge_endpoints.source_entity_id
                  AND weo.owner_kind = $3
                  AND weo.owner_id IS NOT DISTINCT FROM $4
           ) AS source_world_visible
      FROM edge_endpoints
),
walk AS (
    SELECT 1 AS distance,
           ARRAY[$5::uuid, e.target_memory_id] AS path,
           e.edge_id, e.relation, e.relation_class,
           e.source_kind, e.source_memory_id,
           e.target_kind, e.target_memory_id,
           e.target_memory_id AS next_memory_id,
           e.target_visible AS next_readable
    FROM edge_heads e
    WHERE e.source_memory_id = $5
      AND e.source_readable
      AND e.target_memory_id IS NOT NULL
      AND e.relation_class IN ('Provenance', 'Supersession')
      AND NOT (e.source_world_visible AND NOT e.target_visible)
    UNION ALL
    SELECT w.distance + 1,
           w.path || e.target_memory_id,
           e.edge_id, e.relation, e.relation_class,
           e.source_kind, e.source_memory_id,
           e.target_kind, e.target_memory_id,
           e.target_memory_id,
           e.target_visible
    FROM walk w
    JOIN edge_heads e
      ON e.source_memory_id = w.next_memory_id
     AND e.target_memory_id IS NOT NULL
     AND e.relation_class IN ('Provenance', 'Supersession')
    WHERE w.distance < $6
      AND w.next_readable
      AND e.source_readable
      AND NOT e.target_memory_id = ANY(w.path)
      AND NOT (e.source_world_visible AND NOT e.target_visible)
)
SELECT distance, edge_id, relation, relation_class,
       source_kind, source_memory_id, target_kind, target_memory_id,
       next_memory_id, next_readable
FROM walk
ORDER BY distance ASC, edge_id DESC
LIMIT $7
";

const DESCENDANTS_SQL: &str = "
WITH RECURSIVE read_set(kind, id) AS (
    SELECT * FROM unnest($1::proxima_core.owner_ref_kind[], $2::uuid[])
),
readable_memories AS (
    SELECT m.memory_id
      FROM proxima_core.memories m
     WHERE EXISTS (
               SELECT 1
                 FROM (SELECT memory_id AS entity_id, owner_kind, owner_id FROM proxima_core.memories UNION ALL SELECT goal_id AS entity_id, owner_kind, owner_id FROM proxima_core.goals) eo
                 JOIN read_set rs
                   ON eo.owner_kind = rs.kind
                  AND eo.owner_id IS NOT DISTINCT FROM rs.id
                WHERE eo.entity_id = m.memory_id
           )
       AND m.tombstoned_at IS NULL
       {reader_filter}
),
edge_endpoints AS (
    SELECT e.edge_id, e.relation, e.relation_class,
           e.source_kind, e.target_kind,
           COALESCE(e.source_memory_id, sfe.current_memory_id) AS source_memory_id,
           COALESCE(e.target_memory_id, tfe.current_memory_id) AS target_memory_id,
           COALESCE(e.source_memory_id, e.source_goal_id, sfe.current_memory_id) AS source_entity_id,
           COALESCE(e.target_memory_id, e.target_goal_id, tfe.current_memory_id) AS target_entity_id
      FROM proxima_core.edges e
      LEFT JOIN proxima_core.fact_entities sfe
        ON sfe.fact_entity_id = e.source_fact_entity_id
      LEFT JOIN proxima_core.fact_entities tfe
        ON tfe.fact_entity_id = e.target_fact_entity_id
),
edge_heads AS (
    SELECT edge_endpoints.*,
           EXISTS (
               SELECT 1 FROM readable_memories rm
                WHERE rm.memory_id = edge_endpoints.source_memory_id
           ) AS source_readable,
           EXISTS (
               SELECT 1 FROM readable_memories rm
                WHERE rm.memory_id = edge_endpoints.target_memory_id
           ) AS target_visible,
           EXISTS (
               SELECT 1
                 FROM (SELECT memory_id AS entity_id, owner_kind, owner_id FROM proxima_core.memories UNION ALL SELECT goal_id AS entity_id, owner_kind, owner_id FROM proxima_core.goals) weo
                WHERE weo.entity_id = edge_endpoints.source_entity_id
                  AND weo.owner_kind = $3
                  AND weo.owner_id IS NOT DISTINCT FROM $4
           ) AS source_world_visible
      FROM edge_endpoints
),
walk AS (
    SELECT 1 AS distance,
           ARRAY[$5::uuid, e.source_memory_id] AS path,
           e.edge_id, e.relation, e.relation_class,
           e.source_kind, e.source_memory_id,
           e.target_kind, e.target_memory_id,
           e.source_memory_id AS next_memory_id,
           e.source_readable AS next_readable
    FROM edge_heads e
    WHERE e.target_memory_id = $5
      AND e.source_readable
      AND e.source_memory_id IS NOT NULL
      AND e.relation_class IN ('Provenance', 'Supersession')
      AND NOT (e.source_world_visible AND NOT e.target_visible)
    UNION ALL
    SELECT w.distance + 1,
           w.path || e.source_memory_id,
           e.edge_id, e.relation, e.relation_class,
           e.source_kind, e.source_memory_id,
           e.target_kind, e.target_memory_id,
           e.source_memory_id,
           e.source_readable
    FROM walk w
    JOIN edge_heads e
      ON e.target_memory_id = w.next_memory_id
     AND e.source_memory_id IS NOT NULL
     AND e.relation_class IN ('Provenance', 'Supersession')
    WHERE w.distance < $6
      AND w.next_readable
      AND e.source_readable
      AND NOT e.source_memory_id = ANY(w.path)
      AND NOT (e.source_world_visible AND NOT e.target_visible)
)
SELECT distance, edge_id, relation, relation_class,
       source_kind, source_memory_id, target_kind, target_memory_id,
       next_memory_id, next_readable
FROM walk
ORDER BY distance ASC, edge_id DESC
LIMIT $7
";
