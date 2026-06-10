use std::collections::{BTreeMap, BTreeSet};

use proxima_core::verbs::query::{
    EntityKind, MemoryLineageDirection, MemoryLineageEdge, MemoryLineageNode, MemoryLineageRequest,
    MemoryLineageResponse,
};
use proxima_core::{
    MemoryId, OwnerPrincipalKind, Principal, RelationClass, SchemaId, StorageError, WakeChainDepth,
};
use sqlx::PgPool;

#[derive(Debug, sqlx::FromRow)]
struct EdgeWalkRow {
    distance: i32,
    edge_id: uuid::Uuid,
    relation: String,
    relation_class: RelationClass,
    source_memory_id: uuid::Uuid,
    target_memory_id: uuid::Uuid,
    next_memory_id: uuid::Uuid,
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
    req: &MemoryLineageRequest,
) -> Result<MemoryLineageResponse, StorageError> {
    let limit = req.limit.min(200);
    let depth = req.depth.min(8);
    let (owner_kind, owner_principal_id) = match &req.owner.principal {
        Principal::User(user) => (OwnerPrincipalKind::User, user.into_inner()),
        Principal::Group(group) => (OwnerPrincipalKind::Group, group.into_inner()),
    };

    let reader_id = req
        .reader_personality_instance_id
        .map(proxima_core::PersonalityInstanceId::into_inner);

    if !start_memory_visible(
        pool,
        owner_kind,
        owner_principal_id,
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
        owner_kind,
        owner_principal_id,
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
        distances
            .entry(row.next_memory_id)
            .and_modify(|prior| *prior = (*prior).min(distance))
            .or_insert(distance);
    }
    let memory_ids: Vec<_> = distances.keys().copied().collect();
    let node_rows =
        load_nodes(pool, owner_kind, owner_principal_id, &memory_ids, reader_id).await?;
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
        .filter(|row| {
            visible_ids.contains(&row.source_memory_id)
                && visible_ids.contains(&row.target_memory_id)
        })
        .map(|row| MemoryLineageEdge {
            edge_id: row.edge_id,
            relation: row.relation,
            relation_class: row.relation_class.as_str().to_string(),
            source_memory_id: MemoryId::new(row.source_memory_id),
            target_memory_id: MemoryId::new(row.target_memory_id),
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
    owner_kind: OwnerPrincipalKind,
    owner_principal_id: uuid::Uuid,
    memory_id: MemoryId,
    reader_id: Option<uuid::Uuid>,
) -> Result<bool, StorageError> {
    let present: Option<(uuid::Uuid,)> = sqlx::query_as(
        r"SELECT memory_id
             FROM proxima_core.memories
             WHERE owner_principal_kind = $1
               AND owner_principal_id = $2
               AND memory_id = $3
               AND (
                   $4::uuid IS NULL
                   OR kind IS NULL
                   OR personality_instance_id = $4
                   OR EXISTS (
                       SELECT 1
                         FROM proxima_core.read_scope_matrix r
                        WHERE r.owner_principal_kind = memories.owner_principal_kind
                          AND r.owner_principal_id = memories.owner_principal_id
                          AND r.owner_org_id = memories.owner_org_id
                          AND r.reader_personality_instance_id = $4
                          AND r.readable_personality_instance_id = memories.personality_instance_id
                   )
               )",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(memory_id.into_inner())
    .bind(reader_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| StorageError::Internal(e.to_string()))?;
    Ok(present.is_some())
}

async fn walk_edges(
    pool: &PgPool,
    req: &MemoryLineageRequest,
    owner_kind: OwnerPrincipalKind,
    owner_principal_id: uuid::Uuid,
    depth: u8,
    limit: u32,
    reader_id: Option<uuid::Uuid>,
) -> Result<Vec<EdgeWalkRow>, StorageError> {
    let sql = match req.direction {
        MemoryLineageDirection::Ancestors => ANCESTORS_SQL,
        MemoryLineageDirection::Descendants => DESCENDANTS_SQL,
    };
    sqlx::query_as(sql)
        .bind(owner_kind)
        .bind(owner_principal_id)
        .bind(req.start_memory_id.into_inner())
        .bind(i32::from(depth))
        .bind(i64::from(limit))
        .bind(reader_id)
        .fetch_all(pool)
        .await
        .map_err(|e| StorageError::Internal(e.to_string()))
}

async fn load_nodes(
    pool: &PgPool,
    owner_kind: OwnerPrincipalKind,
    owner_principal_id: uuid::Uuid,
    memory_ids: &[uuid::Uuid],
    reader_id: Option<uuid::Uuid>,
) -> Result<Vec<NodeRow>, StorageError> {
    let rows: Vec<NodeRow> = sqlx::query_as(
        r"SELECT memory_id,
                  kind,
                  schema_id,
                  left(COALESCE(text, ''), 480) AS snippet,
                  wake_chain_depth
             FROM proxima_core.memories
             WHERE owner_principal_kind = $1
               AND owner_principal_id = $2
               AND memory_id = ANY($3::uuid[])
               AND (
                   $4::uuid IS NULL
                   OR kind IS NULL
                   OR personality_instance_id = $4
                   OR EXISTS (
                       SELECT 1
                         FROM proxima_core.read_scope_matrix r
                        WHERE r.owner_principal_kind = memories.owner_principal_kind
                          AND r.owner_principal_id = memories.owner_principal_id
                          AND r.owner_org_id = memories.owner_org_id
                          AND r.reader_personality_instance_id = $4
                          AND r.readable_personality_instance_id = memories.personality_instance_id
                   )
               )",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(memory_ids)
    .bind(reader_id)
    .fetch_all(pool)
    .await
    .map_err(|e| StorageError::Internal(e.to_string()))?;

    let expected: BTreeSet<_> = memory_ids.iter().copied().collect();
    let actual: BTreeSet<_> = rows.iter().map(|row: &NodeRow| row.memory_id).collect();
    if expected != actual {
        return Err(StorageError::Internal(
            "lineage node visibility mismatch".into(),
        ));
    }
    Ok(rows)
}

const ANCESTORS_SQL: &str = r"
WITH RECURSIVE walk AS (
    SELECT 1 AS distance,
           ARRAY[$3::uuid, e.target_memory_id] AS path,
           e.edge_id, e.relation, e.relation_class,
           e.source_memory_id, e.target_memory_id,
           e.target_memory_id AS next_memory_id
    FROM proxima_core.edges e
    WHERE e.owner_principal_kind = $1
      AND e.owner_principal_id = $2
      AND e.source_memory_id = $3
      AND e.target_memory_id IS NOT NULL
      AND e.relation_class IN ('Provenance', 'Supersession')
      AND (
          $6::uuid IS NULL
          OR EXISTS (
              SELECT 1
                FROM proxima_core.memories m
               WHERE m.memory_id = e.target_memory_id
                 AND m.owner_principal_kind = e.owner_principal_kind
                 AND m.owner_principal_id = e.owner_principal_id
                 AND (
                     m.kind IS NULL
                     OR m.personality_instance_id = $6
                     OR EXISTS (
                         SELECT 1
                           FROM proxima_core.read_scope_matrix r
                          WHERE r.owner_principal_kind = m.owner_principal_kind
                            AND r.owner_principal_id = m.owner_principal_id
                            AND r.owner_org_id = m.owner_org_id
                            AND r.reader_personality_instance_id = $6
                            AND r.readable_personality_instance_id = m.personality_instance_id
                     )
                 )
          )
      )
    UNION ALL
    SELECT w.distance + 1,
           w.path || e.target_memory_id,
           e.edge_id, e.relation, e.relation_class,
           e.source_memory_id, e.target_memory_id,
           e.target_memory_id
    FROM walk w
    JOIN proxima_core.edges e
      ON e.owner_principal_kind = $1
     AND e.owner_principal_id = $2
     AND e.source_memory_id = w.next_memory_id
     AND e.target_memory_id IS NOT NULL
     AND e.relation_class IN ('Provenance', 'Supersession')
    WHERE w.distance < $4
      AND NOT e.target_memory_id = ANY(w.path)
      AND (
          $6::uuid IS NULL
          OR EXISTS (
              SELECT 1
                FROM proxima_core.memories m
               WHERE m.memory_id = e.target_memory_id
                 AND m.owner_principal_kind = e.owner_principal_kind
                 AND m.owner_principal_id = e.owner_principal_id
                 AND (
                     m.kind IS NULL
                     OR m.personality_instance_id = $6
                     OR EXISTS (
                         SELECT 1
                           FROM proxima_core.read_scope_matrix r
                          WHERE r.owner_principal_kind = m.owner_principal_kind
                            AND r.owner_principal_id = m.owner_principal_id
                            AND r.owner_org_id = m.owner_org_id
                            AND r.reader_personality_instance_id = $6
                            AND r.readable_personality_instance_id = m.personality_instance_id
                     )
                 )
          )
      )
)
SELECT distance, edge_id, relation, relation_class,
       source_memory_id, target_memory_id, next_memory_id
FROM walk
ORDER BY distance ASC, edge_id DESC
LIMIT $5
";

const DESCENDANTS_SQL: &str = r"
WITH RECURSIVE walk AS (
    SELECT 1 AS distance,
           ARRAY[$3::uuid, e.source_memory_id] AS path,
           e.edge_id, e.relation, e.relation_class,
           e.source_memory_id, e.target_memory_id,
           e.source_memory_id AS next_memory_id
    FROM proxima_core.edges e
    WHERE e.owner_principal_kind = $1
      AND e.owner_principal_id = $2
      AND e.target_memory_id = $3
      AND e.source_memory_id IS NOT NULL
      AND e.relation_class IN ('Provenance', 'Supersession')
      AND (
          $6::uuid IS NULL
          OR EXISTS (
              SELECT 1
                FROM proxima_core.memories m
               WHERE m.memory_id = e.source_memory_id
                 AND m.owner_principal_kind = e.owner_principal_kind
                 AND m.owner_principal_id = e.owner_principal_id
                 AND (
                     m.kind IS NULL
                     OR m.personality_instance_id = $6
                     OR EXISTS (
                         SELECT 1
                           FROM proxima_core.read_scope_matrix r
                          WHERE r.owner_principal_kind = m.owner_principal_kind
                            AND r.owner_principal_id = m.owner_principal_id
                            AND r.owner_org_id = m.owner_org_id
                            AND r.reader_personality_instance_id = $6
                            AND r.readable_personality_instance_id = m.personality_instance_id
                     )
                 )
          )
      )
    UNION ALL
    SELECT w.distance + 1,
           w.path || e.source_memory_id,
           e.edge_id, e.relation, e.relation_class,
           e.source_memory_id, e.target_memory_id,
           e.source_memory_id
    FROM walk w
    JOIN proxima_core.edges e
      ON e.owner_principal_kind = $1
     AND e.owner_principal_id = $2
     AND e.target_memory_id = w.next_memory_id
     AND e.source_memory_id IS NOT NULL
     AND e.relation_class IN ('Provenance', 'Supersession')
    WHERE w.distance < $4
      AND NOT e.source_memory_id = ANY(w.path)
      AND (
          $6::uuid IS NULL
          OR EXISTS (
              SELECT 1
                FROM proxima_core.memories m
               WHERE m.memory_id = e.source_memory_id
                 AND m.owner_principal_kind = e.owner_principal_kind
                 AND m.owner_principal_id = e.owner_principal_id
                 AND (
                     m.kind IS NULL
                     OR m.personality_instance_id = $6
                     OR EXISTS (
                         SELECT 1
                           FROM proxima_core.read_scope_matrix r
                          WHERE r.owner_principal_kind = m.owner_principal_kind
                            AND r.owner_principal_id = m.owner_principal_id
                            AND r.owner_org_id = m.owner_org_id
                            AND r.reader_personality_instance_id = $6
                            AND r.readable_personality_instance_id = m.personality_instance_id
                     )
                 )
          )
      )
)
SELECT distance, edge_id, relation, relation_class,
       source_memory_id, target_memory_id, next_memory_id
FROM walk
ORDER BY distance ASC, edge_id DESC
LIMIT $5
";
