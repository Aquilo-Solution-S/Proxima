use std::collections::{BTreeMap, BTreeSet};

use proxima_core::verbs::query::{
    EntityKind, MemoryLineageCursor, MemoryLineageDirection, MemoryLineageEdge, MemoryLineageNode,
    MemoryLineageRequest, MemoryLineageResponse,
};
use proxima_core::{
    Edge, EdgeKind, EdgeTargetProjection, EntityRef, MemoryId, OwnerRef, OwnerRefKind, SchemaId,
    StorageError,
};
use sqlx::PgPool;

use crate::error::map_err;
use crate::verbs::edge_index::{PgEndpointKind, endpoint_from_columns};

use super::{entity_owner_union, read_owner_columns, read_owner_predicate};

#[derive(Debug, sqlx::FromRow)]
struct EdgeWalkRow {
    distance: i32,
    source_kind: PgEndpointKind,
    source_memory_id: uuid::Uuid,
    target_kind: PgEndpointKind,
    target_memory_id: uuid::Uuid,
    created_at: time::OffsetDateTime,
    next_memory_id: uuid::Uuid,
    next_readable: bool,
}

#[derive(Debug, sqlx::FromRow)]
struct NodeRow {
    memory_id: uuid::Uuid,
    kind: Option<EntityKind>,
    schema_id: String,
    snippet: Option<String>,
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
            next_cursor: None,
        });
    }
    let limit = req.limit.min(200);
    let depth = req.depth.min(8);
    let (read_owner_kinds, read_owner_ids) = read_owner_columns(read_owners);

    if !start_memory_visible(
        pool,
        &read_owner_kinds,
        &read_owner_ids,
        req.start_memory_id,
    )
    .await?
    {
        return Ok(MemoryLineageResponse {
            nodes: Vec::new(),
            edges: Vec::new(),
            truncated: false,
            next_cursor: None,
        });
    }

    let edge_rows = walk_edges(
        pool,
        req,
        &read_owner_kinds,
        &read_owner_ids,
        depth,
        limit.saturating_add(1),
    )
    .await?;
    let truncated = edge_rows.len() > usize::try_from(limit).unwrap_or(200);
    let edge_rows: Vec<_> = edge_rows
        .into_iter()
        .take(usize::try_from(limit).unwrap_or(200))
        .collect();
    let next_cursor = (truncated && !edge_rows.is_empty()).then(|| {
        let last = edge_rows.last().expect("non-empty page");
        MemoryLineageCursor {
            distance: u8::try_from(last.distance).unwrap_or(u8::MAX),
            source: EntityRef::Memory(MemoryId::new(last.source_memory_id)),
            target: EntityRef::Memory(MemoryId::new(last.target_memory_id)),
        }
    });

    let distances = page_node_distances(req.start_memory_id, &edge_rows);
    let memory_ids: Vec<_> = distances.keys().copied().collect();
    let node_rows = load_nodes(pool, &read_owner_kinds, &read_owner_ids, &memory_ids).await?;
    let visible_ids: BTreeSet<_> = node_rows.iter().map(|row| row.memory_id).collect();
    let nodes = node_rows
        .into_iter()
        .map(|row| MemoryLineageNode {
            memory_id: MemoryId::new(row.memory_id),
            kind: row.kind.unwrap_or(EntityKind::Fact),
            schema_id: SchemaId::new(row.schema_id),
            snippet: row.snippet.unwrap_or_default(),
            distance: *distances.get(&row.memory_id).unwrap_or(&0),
        })
        .collect();

    let edges = edge_rows
        .into_iter()
        .filter(|row| visible_ids.contains(&row.source_memory_id))
        .map(|row| MemoryLineageEdge {
            edge: Edge {
                source: endpoint_from_columns(row.source_kind, row.source_memory_id),
                target: if row.next_readable {
                    EdgeTargetProjection::visible(endpoint_from_columns(
                        row.target_kind,
                        row.target_memory_id,
                    ))
                } else {
                    EdgeTargetProjection::Redacted
                },
                // The walk traverses origin and nothing else: lineage is the
                // provenance chain, and supersession left the edge table for
                // a pointer on the row.
                kind: EdgeKind::Origin,
                created_at: row.created_at,
            },
            distance: u8::try_from(row.distance).unwrap_or(u8::MAX),
        })
        .collect();

    Ok(MemoryLineageResponse {
        nodes,
        edges,
        truncated,
        next_cursor,
    })
}

/// Node ids for one page of the walk, keyed to their minimal observed
/// distance: the start, each edge's anchor endpoint, and each readable
/// next endpoint. The anchor (the endpoint the walk arrived FROM, one
/// hop nearer the start) is always readable per the walk SQL, but on a
/// paged walk its node row may have been emitted on an earlier page —
/// loading it with this page's nodes keeps every page self-contained
/// and the source-visibility edge filter working past the first page.
fn page_node_distances(
    start_memory_id: MemoryId,
    edge_rows: &[EdgeWalkRow],
) -> BTreeMap<uuid::Uuid, u8> {
    let mut distances = BTreeMap::from([(start_memory_id.into_inner(), 0_u8)]);
    for row in edge_rows {
        let distance = u8::try_from(row.distance).unwrap_or(u8::MAX);
        let anchor = if row.next_memory_id == row.target_memory_id {
            row.source_memory_id
        } else {
            row.target_memory_id
        };
        distances
            .entry(anchor)
            .and_modify(|prior| *prior = (*prior).min(distance.saturating_sub(1)))
            .or_insert(distance.saturating_sub(1));
        if row.next_readable {
            distances
                .entry(row.next_memory_id)
                .and_modify(|prior| *prior = (*prior).min(distance))
                .or_insert(distance);
        }
    }
    distances
}

async fn start_memory_visible(
    pool: &PgPool,
    read_owner_kinds: &[OwnerRefKind],
    read_owner_ids: &[Option<uuid::Uuid>],
    memory_id: MemoryId,
) -> Result<bool, StorageError> {
    let sql = format!(
        "SELECT m.memory_id
             FROM proxima_core.memories m
             WHERE EXISTS (
                       SELECT 1
                         FROM {entity_owner_union} eo
                         JOIN unnest($1::proxima_core.owner_ref_kind[], $2::uuid[]) AS rs(kind, id)
                           ON {read_owner_predicate}
                        WHERE eo.entity_id = m.memory_id
                   )
               AND m.memory_id = $3
               AND m.tombstoned_at IS NULL",
        entity_owner_union = entity_owner_union(),
        read_owner_predicate = read_owner_predicate("eo", "rs"),
    );
    // SQL-POLICY: fixed-fragment
    let query = sqlx::query_as::<_, (uuid::Uuid,)>(sqlx::AssertSqlSafe(sql))
        .bind(read_owner_kinds)
        .bind(read_owner_ids)
        .bind(memory_id.into_inner());
    let present = query.fetch_optional(pool).await.map_err(map_err)?;
    Ok(present.is_some())
}

async fn walk_edges(
    pool: &PgPool,
    req: &MemoryLineageRequest,
    read_owner_kinds: &[OwnerRefKind],
    read_owner_ids: &[Option<uuid::Uuid>],
    depth: u8,
    limit: u32,
) -> Result<Vec<EdgeWalkRow>, StorageError> {
    let sql = match req.direction {
        MemoryLineageDirection::Ancestors => ancestors_sql(),
        MemoryLineageDirection::Descendants => descendants_sql(),
    };
    let (world_kind, world_id) =
        crate::access::owner_columns::owner_binds(&proxima_core::access::world());
    let after_distance = req.after.map(|after| i32::from(after.distance));
    let after_source = req.after.map(|after| lineage_cursor_id(after.source));
    let after_target = req.after.map(|after| lineage_cursor_id(after.target));
    // SQL-POLICY: fixed-fragment
    let query = sqlx::query_as::<_, EdgeWalkRow>(sqlx::AssertSqlSafe(sql))
        .bind(read_owner_kinds)
        .bind(read_owner_ids)
        .bind(world_kind)
        .bind(world_id)
        .bind(req.start_memory_id.into_inner())
        .bind(i32::from(depth))
        .bind(i64::from(limit))
        .bind(after_distance)
        .bind(after_source)
        .bind(after_target);
    query.fetch_all(pool).await.map_err(map_err)
}

/// The walk is over memory rows, so a lineage cursor's endpoints always name
/// memories; the other two `EntityRef` forms are accepted and read for their
/// id rather than refused, because a cursor is opaque to its holder.
fn lineage_cursor_id(entity: EntityRef) -> uuid::Uuid {
    match entity {
        EntityRef::Memory(id) => id.into_inner(),
        EntityRef::Goal(id) => id.into_inner(),
        EntityRef::FactEntity(id) => id.into_inner(),
    }
}

async fn load_nodes(
    pool: &PgPool,
    read_owner_kinds: &[OwnerRefKind],
    read_owner_ids: &[Option<uuid::Uuid>],
    memory_ids: &[uuid::Uuid],
) -> Result<Vec<NodeRow>, StorageError> {
    let sql = format!(
        "SELECT m.memory_id,
                  m.kind,
                  m.schema_id,
                  left(COALESCE(m.text, ''), 480) AS snippet
             FROM proxima_core.memories m
             WHERE EXISTS (
                       SELECT 1
                         FROM {entity_owner_union} eo
                         JOIN unnest($1::proxima_core.owner_ref_kind[], $2::uuid[]) AS rs(kind, id)
                           ON {read_owner_predicate}
                        WHERE eo.entity_id = m.memory_id
                   )
               AND m.memory_id = ANY($3::uuid[])
               AND m.tombstoned_at IS NULL",
        entity_owner_union = entity_owner_union(),
        read_owner_predicate = read_owner_predicate("eo", "rs"),
    );
    // SQL-POLICY: fixed-fragment
    let query = sqlx::query_as::<_, NodeRow>(sqlx::AssertSqlSafe(sql))
        .bind(read_owner_kinds)
        .bind(read_owner_ids)
        .bind(memory_ids);
    let rows = query.fetch_all(pool).await.map_err(map_err)?;

    Ok(rows)
}

/// Shared prelude for both walk directions: the readable memory set, then the
/// `origin` edges with Fact-entity endpoints resolved to their current heads.
///
/// Only `origin` rows are traversed. Lineage is the provenance chain — what a
/// memory was made from — and that is now exactly one kind. Supersession
/// walked here too under the old model; it is a pointer on the row now, and a
/// lineage walk that followed it would be answering a different question.
fn walk_prelude() -> String {
    format!(
        "
WITH RECURSIVE read_set(kind, id) AS (
    SELECT * FROM unnest($1::proxima_core.owner_ref_kind[], $2::uuid[])
),
readable_memories AS (
    SELECT m.memory_id
      FROM proxima_core.memories m
     WHERE EXISTS (
               SELECT 1
                 FROM {entity_owner_union} eo
                 JOIN read_set rs
                   ON {read_owner_predicate}
                WHERE eo.entity_id = m.memory_id
           )
       AND m.tombstoned_at IS NULL
),
edge_endpoints AS (
    SELECT e.kind, e.created_at,
           CASE WHEN e.source_kind = 'FactEntityHead'::proxima_core.edge_endpoint_kind
                THEN 'Fact'::proxima_core.edge_endpoint_kind ELSE e.source_kind END
                AS source_kind,
           CASE WHEN e.target_kind = 'FactEntityHead'::proxima_core.edge_endpoint_kind
                THEN 'Fact'::proxima_core.edge_endpoint_kind ELSE e.target_kind END
                AS target_kind,
           COALESCE(sfe.current_memory_id, e.source_id) AS source_memory_id,
           COALESCE(tfe.current_memory_id, e.target_id) AS target_memory_id
      FROM proxima_core.edges e
      LEFT JOIN proxima_core.fact_entities sfe
        ON e.source_kind = 'FactEntityHead'::proxima_core.edge_endpoint_kind
       AND sfe.fact_entity_id = e.source_id
      LEFT JOIN proxima_core.fact_entities tfe
        ON e.target_kind = 'FactEntityHead'::proxima_core.edge_endpoint_kind
       AND tfe.fact_entity_id = e.target_id
     WHERE e.kind = 'origin'::proxima_core.edge_kind
       AND e.source_kind <> 'Goal'::proxima_core.edge_endpoint_kind
       AND e.target_kind <> 'Goal'::proxima_core.edge_endpoint_kind
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
                 FROM {entity_owner_union} weo
                WHERE weo.entity_id = edge_endpoints.source_memory_id
                  AND weo.owner_kind = $3
                  AND weo.owner_id IS NOT DISTINCT FROM $4
           ) AS source_world_visible
      FROM edge_endpoints
)",
        entity_owner_union = entity_owner_union(),
        read_owner_predicate = read_owner_predicate("eo", "rs"),
    )
}

/// Keyset tail shared by both directions. `(distance ASC, source DESC, target
/// DESC)` is the total order; the cursor is the edge itself because an edge
/// has no id.
const WALK_TAIL: &str = "
SELECT DISTINCT distance, source_kind, source_memory_id, target_kind, target_memory_id,
       created_at, next_memory_id, next_readable
FROM walk
WHERE ($8::int IS NULL
       OR distance > $8
       OR (distance = $8
           AND (source_memory_id, target_memory_id) < ($9::uuid, $10::uuid)))
ORDER BY distance ASC, source_memory_id DESC, target_memory_id DESC
LIMIT $7
";

fn ancestors_sql() -> String {
    format!(
        "{prelude},
walk AS (
    SELECT 1 AS distance,
           ARRAY[$5::uuid, e.target_memory_id] AS path,
           e.source_kind, e.source_memory_id,
           e.target_kind, e.target_memory_id, e.created_at,
           e.target_memory_id AS next_memory_id,
           e.target_visible AS next_readable
    FROM edge_heads e
    WHERE e.source_memory_id = $5
      AND e.source_readable
      AND NOT (e.source_world_visible AND NOT e.target_visible)
    UNION ALL
    SELECT w.distance + 1,
           w.path || e.target_memory_id,
           e.source_kind, e.source_memory_id,
           e.target_kind, e.target_memory_id, e.created_at,
           e.target_memory_id,
           e.target_visible
    FROM walk w
    JOIN edge_heads e
      ON e.source_memory_id = w.next_memory_id
    WHERE w.distance < $6
      AND w.next_readable
      AND e.source_readable
      AND NOT e.target_memory_id = ANY(w.path)
      AND NOT (e.source_world_visible AND NOT e.target_visible)
)
{WALK_TAIL}",
        prelude = walk_prelude(),
    )
}

fn descendants_sql() -> String {
    format!(
        "{prelude},
walk AS (
    SELECT 1 AS distance,
           ARRAY[$5::uuid, e.source_memory_id] AS path,
           e.source_kind, e.source_memory_id,
           e.target_kind, e.target_memory_id, e.created_at,
           e.source_memory_id AS next_memory_id,
           e.source_readable AS next_readable
    FROM edge_heads e
    WHERE e.target_memory_id = $5
      AND e.source_readable
      AND NOT (e.source_world_visible AND NOT e.target_visible)
    UNION ALL
    SELECT w.distance + 1,
           w.path || e.source_memory_id,
           e.source_kind, e.source_memory_id,
           e.target_kind, e.target_memory_id, e.created_at,
           e.source_memory_id,
           e.source_readable
    FROM walk w
    JOIN edge_heads e
      ON e.target_memory_id = w.next_memory_id
    WHERE w.distance < $6
      AND w.next_readable
      AND e.source_readable
      AND NOT e.source_memory_id = ANY(w.path)
      AND NOT (e.source_world_visible AND NOT e.target_visible)
)
{WALK_TAIL}",
        prelude = walk_prelude(),
    )
}

/// A lineage edge always sources at a memory row, so the endpoint decode
/// below can never see a Goal address; this is the assertion that says so.
#[cfg(test)]
mod tests {
    use super::{PgEndpointKind, endpoint_from_columns};
    use proxima_core::{EdgeEndpoint, EntityKind};

    #[test]
    fn a_resolved_head_decodes_as_a_pinned_fact_memory() {
        let id = uuid::Uuid::now_v7();
        let endpoint: EdgeEndpoint = endpoint_from_columns(PgEndpointKind::Fact, id);
        assert_eq!(endpoint.kind, EntityKind::Fact);
        assert_eq!(
            endpoint.memory_id().map(proxima_core::MemoryId::into_inner),
            Some(id)
        );
    }
}
