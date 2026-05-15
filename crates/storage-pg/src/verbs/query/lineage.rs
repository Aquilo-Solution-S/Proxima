use std::collections::{BTreeMap, BTreeSet};

use proxima_core::verbs::query::{
    EntityKind, MemoryLineageDirection, MemoryLineageEdge, MemoryLineageNode, MemoryLineageRequest,
    MemoryLineageResponse,
};
use proxima_core::{MemoryId, Principal, SchemaId, StorageError, WakeChainDepth};
use sqlx::PgPool;

#[derive(Debug, sqlx::FromRow)]
struct EdgeWalkRow {
    distance: i32,
    edge_id: uuid::Uuid,
    relation: String,
    relation_class: String,
    source_memory_id: uuid::Uuid,
    target_memory_id: uuid::Uuid,
    next_memory_id: uuid::Uuid,
}

#[derive(Debug, sqlx::FromRow)]
struct NodeRow {
    memory_id: uuid::Uuid,
    kind: Option<String>,
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
        Principal::User(user) => ("User", user.into_inner()),
        Principal::Group(group) => ("Group", group.into_inner()),
    };

    if !start_memory_visible(pool, owner_kind, owner_principal_id, req.start_memory_id).await? {
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
    let node_rows = load_nodes(pool, owner_kind, owner_principal_id, &memory_ids).await?;
    let nodes = node_rows
        .into_iter()
        .map(|row| {
            Ok(MemoryLineageNode {
                memory_id: MemoryId::new(row.memory_id),
                kind: parse_kind(row.kind.as_deref())?,
                schema_id: SchemaId::new(row.schema_id),
                snippet: row.snippet.unwrap_or_default(),
                wake_chain_depth: WakeChainDepth::new(
                    u16::try_from(row.wake_chain_depth).unwrap_or(0),
                ),
                distance: *distances.get(&row.memory_id).unwrap_or(&0),
            })
        })
        .collect::<Result<Vec<_>, StorageError>>()?;

    let edges = edge_rows
        .into_iter()
        .map(|row| MemoryLineageEdge {
            edge_id: row.edge_id,
            relation: row.relation,
            relation_class: row.relation_class,
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
    owner_kind: &str,
    owner_principal_id: uuid::Uuid,
    memory_id: MemoryId,
) -> Result<bool, StorageError> {
    let present: Option<(uuid::Uuid,)> = sqlx::query_as(
        "SELECT memory_id
         FROM proxima_core.memories
         WHERE owner_principal_kind = $1
           AND owner_principal_id = $2
           AND memory_id = $3",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(memory_id.into_inner())
    .fetch_optional(pool)
    .await
    .map_err(|e| StorageError::Internal(e.to_string()))?;
    Ok(present.is_some())
}

async fn walk_edges(
    pool: &PgPool,
    req: &MemoryLineageRequest,
    owner_kind: &str,
    owner_principal_id: uuid::Uuid,
    depth: u8,
    limit: u32,
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
        .fetch_all(pool)
        .await
        .map_err(|e| StorageError::Internal(e.to_string()))
}

async fn load_nodes(
    pool: &PgPool,
    owner_kind: &str,
    owner_principal_id: uuid::Uuid,
    memory_ids: &[uuid::Uuid],
) -> Result<Vec<NodeRow>, StorageError> {
    let rows = sqlx::query_as(
        "SELECT memory_id, kind, schema_id, left(COALESCE(text, ''), 480) AS snippet,
                wake_chain_depth
         FROM proxima_core.memories
         WHERE owner_principal_kind = $1
           AND owner_principal_id = $2
           AND memory_id = ANY($3::uuid[])",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(memory_ids)
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

fn parse_kind(kind: Option<&str>) -> Result<EntityKind, StorageError> {
    match kind {
        None => Ok(EntityKind::Fact),
        Some("Abstraction") => Ok(EntityKind::Abstraction),
        Some("Perspective") => Ok(EntityKind::Perspective),
        Some(other) => Err(StorageError::Internal(format!(
            "unexpected lineage memory kind: {other}"
        ))),
    }
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
)
SELECT distance, edge_id, relation, relation_class,
       source_memory_id, target_memory_id, next_memory_id
FROM walk
ORDER BY distance ASC, edge_id DESC
LIMIT $5
";
