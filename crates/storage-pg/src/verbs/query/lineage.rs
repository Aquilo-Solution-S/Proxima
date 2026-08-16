use std::collections::{BTreeMap, BTreeSet};

use proxima_core::verbs::query::{
    EntityKind, MemoryLineageCursor, MemoryLineageDirection, MemoryLineageEdge, MemoryLineageNode,
    MemoryLineageRequest, MemoryLineageResponse,
};
use proxima_core::{
    Edge, EdgeEndpoint, EdgeKind, EdgeTargetProjection, EntityRef, MemoryId, OwnerRef,
    OwnerRefKind, SchemaId, StorageError,
};
use sqlx::PgPool;

use crate::error::map_err;
use crate::verbs::edge_index::{PgEndpointKind, endpoint_from_columns};

use super::{read_owner_columns, read_owner_predicate};

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
    kind: EntityKind,
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
    return walk_memory_lineage_timeseries(pool, read_owners, req).await;
    #[allow(unreachable_code)]
    let _ = req;
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
            kind: row.kind,
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
                         FROM unnest($1::proxima_core.owner_kind[], $2::uuid[]) AS rs(kind, id)
                        WHERE {read_owner_predicate}
                   )
               AND m.memory_id = $3
               AND m.tombstoned_at IS NULL",
        read_owner_predicate = read_owner_predicate("m", "rs"),
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
    let sql = walk_sql(req.direction);
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

/// The walk statement for one direction; both directions bind the same ten
/// parameters in the same order.
fn walk_sql(direction: MemoryLineageDirection) -> String {
    match direction {
        MemoryLineageDirection::Ancestors => ancestors_sql(),
        MemoryLineageDirection::Descendants => descendants_sql(),
    }
}

/// [`walk_sql`] for plan and equivalence assertions in tests. Same cfg gate
/// as the search `*_sql_for_tests` exports.
#[cfg(any(test, feature = "test-fixtures", debug_assertions))]
#[doc(hidden)]
#[must_use]
pub fn lineage_walk_sql_for_tests(direction: MemoryLineageDirection) -> String {
    walk_sql(direction)
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
                         FROM unnest($1::proxima_core.owner_kind[], $2::uuid[]) AS rs(kind, id)
                        WHERE {read_owner_predicate}
                   )
               AND m.memory_id = ANY($3::uuid[])
               AND m.tombstoned_at IS NULL",
        read_owner_predicate = read_owner_predicate("m", "rs"),
    );
    // SQL-POLICY: fixed-fragment
    let query = sqlx::query_as::<_, NodeRow>(sqlx::AssertSqlSafe(sql))
        .bind(read_owner_kinds)
        .bind(read_owner_ids)
        .bind(memory_ids);
    let rows = query.fetch_all(pool).await.map_err(map_err)?;

    Ok(rows)
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

/// Resolved endpoint ids and projected kinds, as the walk spells them.
const WALK_SOURCE_ID: &str = "COALESCE(sfe.current_memory_id, e.source_id)";
const WALK_TARGET_ID: &str = "COALESCE(tfe.current_memory_id, e.target_id)";
const WALK_SOURCE_KIND: &str =
    "CASE WHEN e.source_kind = 'FactEntityHead'::proxima_core.edge_endpoint_kind
                THEN 'Fact'::proxima_core.edge_endpoint_kind ELSE e.source_kind END";
const WALK_TARGET_KIND: &str =
    "CASE WHEN e.target_kind = 'FactEntityHead'::proxima_core.edge_endpoint_kind
                THEN 'Fact'::proxima_core.edge_endpoint_kind ELSE e.target_kind END";

/// Head-resolution joins and the origin/non-Goal filter, shared by both
/// prefilter directions; identical to the `edge_endpoints` CTE's body.
const WALK_EDGE_JOINS: &str = "
      LEFT JOIN proxima_core.fact_entities sfe
        ON e.source_kind = 'FactEntityHead'::proxima_core.edge_endpoint_kind
       AND sfe.fact_entity_id = e.source_id
      LEFT JOIN proxima_core.fact_entities tfe
        ON e.target_kind = 'FactEntityHead'::proxima_core.edge_endpoint_kind
       AND tfe.fact_entity_id = e.target_id";
const WALK_EDGE_FILTER: &str = "e.kind = 'origin'::proxima_core.edge_kind
       AND e.source_kind <> 'Goal'::proxima_core.edge_endpoint_kind
       AND e.target_kind <> 'Goal'::proxima_core.edge_endpoint_kind";

/// Per-endpoint readability, replacing the materialized `readable_memories`
/// probe: identical predicate (read-set membership plus the tombstone
/// filter), applied to one memory id instead of the whole table.
fn walk_readable(id_expr: &str) -> String {
    format!(
        "EXISTS (
               SELECT 1
                 FROM proxima_core.memories vm
                 JOIN read_set rs
                   ON {predicate}
                WHERE vm.memory_id = {id_expr}
                  AND vm.tombstoned_at IS NULL
           )",
        predicate = read_owner_predicate("vm", "rs"),
    )
}

/// Per-endpoint world visibility, replacing the OFF arm's owner-union probe.
/// Every id probed here is a memory id by construction — Goal endpoints are
/// filtered out of the walk, and a dangling head id matches neither a
/// memory row nor either arm of the union — so reading `memories` directly
/// is the same predicate without the goals arm. No tombstone filter, to
/// match the union's semantics exactly.
fn walk_world_visible(id_expr: &str) -> String {
    format!(
        "EXISTS (
               SELECT 1
                 FROM proxima_core.memories wm
                WHERE wm.memory_id = {id_expr}
                  AND wm.owner_kind = $3
                  AND wm.owner_id IS NOT DISTINCT FROM $4
           )"
    )
}

/// Ancestors walk. Per step, probe only the edge rows that can anchor at
/// the step's memory — the memory id itself plus the fact-entity ids
/// currently heading it (`idx_fact_entities_current_memory`, migration
/// 0017) — through `idx_edges_source`, then resolve and visibility-check
/// just those.
///
/// The probe is an exact superset of the resolved-anchor predicate it
/// guards (a resolved source equals the anchor only via the raw column or
/// via a collected head id), and that predicate is kept verbatim as the
/// residual, so the traversal cannot change. Recursive self-reference
/// appears exactly once; the lateral probe references only its columns.
fn ancestors_sql() -> String {
    let src = WALK_SOURCE_ID;
    let tgt = WALK_TARGET_ID;
    format!(
        "
WITH RECURSIVE read_set(kind, id) AS (
    SELECT * FROM unnest($1::proxima_core.owner_kind[], $2::uuid[])
),
walk AS (
    SELECT 1 AS distance,
           ARRAY[$5::uuid, {tgt}] AS path,
           {src_kind} AS source_kind,
           {src} AS source_memory_id,
           {tgt_kind} AS target_kind,
           {tgt} AS target_memory_id,
           e.created_at,
           {tgt} AS next_memory_id,
           {tgt_readable} AS next_readable
      FROM (SELECT $5::uuid AS id
            UNION ALL
            SELECT fe.fact_entity_id
              FROM proxima_core.fact_entities fe
             WHERE fe.current_memory_id = $5::uuid) probe
      JOIN proxima_core.edges e ON e.source_id = probe.id{joins}
     WHERE {edge_filter}
       AND {src} = $5::uuid
       AND {src_readable}
       AND NOT ({src_world} AND NOT {tgt_readable})
    UNION ALL
    SELECT w.distance + 1,
           w.path || {tgt},
           {src_kind},
           {src},
           {tgt_kind},
           {tgt},
           e.created_at,
           {tgt},
           {tgt_readable}
      FROM walk w
      JOIN LATERAL (SELECT w.next_memory_id AS id
                    UNION ALL
                    SELECT fe.fact_entity_id
                      FROM proxima_core.fact_entities fe
                     WHERE fe.current_memory_id = w.next_memory_id) probe ON true
      JOIN proxima_core.edges e ON e.source_id = probe.id{joins}
     WHERE {edge_filter}
       AND {src} = w.next_memory_id
       AND w.distance < $6
       AND w.next_readable
       AND {src_readable}
       AND NOT {tgt} = ANY(w.path)
       AND NOT ({src_world} AND NOT {tgt_readable})
)
{WALK_TAIL}",
        src_kind = WALK_SOURCE_KIND,
        tgt_kind = WALK_TARGET_KIND,
        src_readable = walk_readable(src),
        tgt_readable = walk_readable(tgt),
        src_world = walk_world_visible(src),
        joins = WALK_EDGE_JOINS,
        edge_filter = WALK_EDGE_FILTER,
    )
}

/// [`ancestors_sql`] mirrored: the anchor is the resolved TARGET,
/// the probe rides `idx_edges_target`, and the walk advances to the source
/// endpoint, exactly as the OFF descendants arm does.
fn descendants_sql() -> String {
    let src = WALK_SOURCE_ID;
    let tgt = WALK_TARGET_ID;
    format!(
        "
WITH RECURSIVE read_set(kind, id) AS (
    SELECT * FROM unnest($1::proxima_core.owner_kind[], $2::uuid[])
),
walk AS (
    SELECT 1 AS distance,
           ARRAY[$5::uuid, {src}] AS path,
           {src_kind} AS source_kind,
           {src} AS source_memory_id,
           {tgt_kind} AS target_kind,
           {tgt} AS target_memory_id,
           e.created_at,
           {src} AS next_memory_id,
           {src_readable} AS next_readable
      FROM (SELECT $5::uuid AS id
            UNION ALL
            SELECT fe.fact_entity_id
              FROM proxima_core.fact_entities fe
             WHERE fe.current_memory_id = $5::uuid) probe
      JOIN proxima_core.edges e ON e.target_id = probe.id{joins}
     WHERE {edge_filter}
       AND {tgt} = $5::uuid
       AND {src_readable}
       AND NOT ({src_world} AND NOT {tgt_readable})
    UNION ALL
    SELECT w.distance + 1,
           w.path || {src},
           {src_kind},
           {src},
           {tgt_kind},
           {tgt},
           e.created_at,
           {src},
           {src_readable}
      FROM walk w
      JOIN LATERAL (SELECT w.next_memory_id AS id
                    UNION ALL
                    SELECT fe.fact_entity_id
                      FROM proxima_core.fact_entities fe
                     WHERE fe.current_memory_id = w.next_memory_id) probe ON true
      JOIN proxima_core.edges e ON e.target_id = probe.id{joins}
     WHERE {edge_filter}
       AND {tgt} = w.next_memory_id
       AND w.distance < $6
       AND w.next_readable
       AND {src_readable}
       AND NOT {src} = ANY(w.path)
       AND NOT ({src_world} AND NOT {tgt_readable})
)
{WALK_TAIL}",
        src_kind = WALK_SOURCE_KIND,
        tgt_kind = WALK_TARGET_KIND,
        src_readable = walk_readable(src),
        tgt_readable = walk_readable(tgt),
        src_world = walk_world_visible(src),
        joins = WALK_EDGE_JOINS,
        edge_filter = WALK_EDGE_FILTER,
    )
}

#[allow(clippy::too_many_lines)]
async fn walk_memory_lineage_timeseries(
    pool: &PgPool,
    read_owners: &[OwnerRef],
    req: &MemoryLineageRequest,
) -> Result<MemoryLineageResponse, StorageError> {
    let owner_ids: Vec<uuid::Uuid> = read_owners
        .iter()
        .copied()
        .map(OwnerRef::stored_owner_id)
        .collect();
    let start = req.start_memory_id.into_inner();
    let depth = i32::from(req.depth.clamp(1, 8));
    let limit = req.limit.min(200);
    let hops: Vec<(uuid::Uuid, String, uuid::Uuid, String, i32)> = match req.direction {
        MemoryLineageDirection::Ancestors => sqlx::query_as(
            "WITH RECURSIVE walk AS (
                 SELECT src.t AS src, src.kind::text AS src_kind,
                        pin AS tgt, tgt.kind::text AS tgt_kind, 1 AS dist
                   FROM proxima_core.memory src
                   JOIN unnest(src.origins) AS pin ON true
                   JOIN proxima_core.memory tgt ON tgt.t = pin
                  WHERE src.t = $1
                    AND src.owner_id = ANY($2::uuid[])
                    AND tgt.owner_id = ANY($2::uuid[])
                 UNION ALL
                 SELECT n.t, n.kind::text, pin, nxt.kind::text, w.dist + 1
                   FROM walk w
                   JOIN proxima_core.memory n ON n.t = w.tgt
                   JOIN unnest(n.origins) AS pin ON true
                   JOIN proxima_core.memory nxt ON nxt.t = pin
                  WHERE w.dist < $3
                    AND n.owner_id = ANY($2::uuid[])
                    AND nxt.owner_id = ANY($2::uuid[])
             )
             SELECT src, src_kind, tgt, tgt_kind, dist FROM walk",
        )
        .bind(start)
        .bind(&owner_ids)
        .bind(depth)
        .fetch_all(pool)
        .await
        .map_err(map_err)?,
        MemoryLineageDirection::Descendants => sqlx::query_as(
            "WITH RECURSIVE walk AS (
                 SELECT child.t AS src, child.kind::text AS src_kind,
                        $1::uuid AS tgt, parent.kind::text AS tgt_kind, 1 AS dist
                   FROM proxima_core.memory child
                   JOIN proxima_core.memory parent ON parent.t = $1
                  WHERE $1 = ANY(child.origins)
                    AND child.owner_id = ANY($2::uuid[])
                    AND parent.owner_id = ANY($2::uuid[])
                 UNION ALL
                 SELECT child.t, child.kind::text, w.src, w.src_kind, w.dist + 1
                   FROM walk w
                   JOIN proxima_core.memory child ON w.src = ANY(child.origins)
                  WHERE w.dist < $3
                    AND child.owner_id = ANY($2::uuid[])
             )
             SELECT src, src_kind, tgt, tgt_kind, dist FROM walk",
        )
        .bind(start)
        .bind(&owner_ids)
        .bind(depth)
        .fetch_all(pool)
        .await
        .map_err(map_err)?,
    };

    let start_kind_schema: Option<(String, String)> = sqlx::query_as(
        "SELECT m.kind::text, h.schema_id
           FROM proxima_core.memory m
           JOIN proxima_core.memory_head h ON h.handle = m.handle
          WHERE m.t = $1 AND m.owner_id = ANY($2::uuid[])",
    )
    .bind(start)
    .bind(&owner_ids)
    .fetch_optional(pool)
    .await
    .map_err(map_err)?;
    let Some((start_kind, start_schema)) = start_kind_schema else {
        return Ok(MemoryLineageResponse {
            nodes: Vec::new(),
            edges: Vec::new(),
            truncated: false,
            next_cursor: None,
        });
    };

    let mut hops = hops;
    hops.sort_by(|a, b| {
        a.4.cmp(&b.4)
            .then_with(|| b.0.cmp(&a.0))
            .then_with(|| b.2.cmp(&a.2))
    });
    if let Some(after) = req.after {
        hops.retain(|hop| {
            let dist = u8::try_from(hop.4).unwrap_or(u8::MAX);
            match dist.cmp(&after.distance) {
                std::cmp::Ordering::Greater => true,
                std::cmp::Ordering::Less => false,
                std::cmp::Ordering::Equal => {
                    let after_src = match after.source {
                        EntityRef::Memory(id) => id.into_inner(),
                        _ => return true,
                    };
                    let after_tgt = match after.target {
                        EntityRef::Memory(id) => id.into_inner(),
                        _ => return true,
                    };
                    hop.0 < after_src || (hop.0 == after_src && hop.2 < after_tgt)
                }
            }
        });
    }
    let page_len = usize::try_from(limit).unwrap_or(usize::MAX);
    let truncated = hops.len() > page_len;
    hops.truncate(page_len);

    let mut node_ids = vec![start];
    node_ids.extend(hops.iter().flat_map(|hop| [hop.0, hop.2]));
    let node_rows: Vec<(uuid::Uuid, String, String)> = sqlx::query_as(
        "SELECT m.t, m.kind::text, h.schema_id
           FROM proxima_core.memory m
           JOIN proxima_core.memory_head h ON h.handle = m.handle
          WHERE m.t = ANY($1::uuid[])
            AND m.owner_id = ANY($2::uuid[])",
    )
    .bind(&node_ids)
    .bind(&owner_ids)
    .fetch_all(pool)
    .await
    .map_err(map_err)?;
    let mut nodes: Vec<MemoryLineageNode> = node_rows
        .into_iter()
        .filter_map(|(id, kind, schema_id)| {
            Some(MemoryLineageNode {
                memory_id: MemoryId::new(id),
                kind: parse_kind(&kind)?,
                schema_id: SchemaId::new(schema_id),
                snippet: String::new(),
                distance: u8::from(id != start),
            })
        })
        .collect();
    if !nodes.iter().any(|n| n.memory_id.into_inner() == start)
        && let Some(kind) = parse_kind(&start_kind)
    {
        nodes.push(MemoryLineageNode {
            memory_id: MemoryId::new(start),
            kind,
            schema_id: SchemaId::new(start_schema),
            snippet: String::new(),
            distance: 0,
        });
    }

    let edges: Vec<MemoryLineageEdge> = hops
        .iter()
        .filter_map(|(src, src_kind, tgt, tgt_kind, dist)| {
            Some(MemoryLineageEdge {
                edge: Edge {
                    source: EdgeEndpoint::memory(parse_kind(src_kind)?, MemoryId::new(*src)),
                    target: EdgeTargetProjection::visible(EdgeEndpoint::memory(
                        parse_kind(tgt_kind)?,
                        MemoryId::new(*tgt),
                    )),
                    kind: EdgeKind::Origin,
                    created_at: time::OffsetDateTime::UNIX_EPOCH,
                },
                distance: u8::try_from(*dist).unwrap_or(u8::MAX),
            })
        })
        .collect();
    let next_cursor = truncated.then(|| {
        let last = hops.last().expect("truncated page is non-empty");
        MemoryLineageCursor {
            distance: u8::try_from(last.4).unwrap_or(u8::MAX),
            source: EntityRef::Memory(MemoryId::new(last.0)),
            target: EntityRef::Memory(MemoryId::new(last.2)),
        }
    });
    Ok(MemoryLineageResponse {
        nodes,
        edges,
        truncated,
        next_cursor,
    })
}

fn parse_kind(kind: &str) -> Option<EntityKind> {
    match kind {
        "fact" => Some(EntityKind::Fact),
        "abstraction" => Some(EntityKind::Abstraction),
        "perspective" => Some(EntityKind::Perspective),
        _ => None,
    }
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
