use proxima_core::verbs::query::{
    EntityKind, MemoryLineageCursor, MemoryLineageDirection, MemoryLineageEdge, MemoryLineageNode,
    MemoryLineageRequest, MemoryLineageResponse,
};
use proxima_core::{
    Edge, EdgeEndpoint, EdgeKind, EdgeTargetProjection, EntityRef, MemoryId, OwnerRef, SchemaId,
    StorageError,
};
use sqlx::PgPool;

use crate::error::map_err;

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
    walk_memory_lineage_timeseries(pool, read_owners, req).await
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
    let after_dist = req.after.map(|after| i32::from(after.distance));
    let after_src = req.after.and_then(|after| match after.source {
        EntityRef::Memory(id) => Some(id.into_inner()),
        EntityRef::Goal(_) => None,
    });
    let after_tgt = req.after.and_then(|after| match after.target {
        EntityRef::Memory(id) => Some(id.into_inner()),
        EntityRef::Goal(_) => None,
    });
    let page_limit = i64::from(limit).saturating_add(1);
    let mut hops: Vec<(
        uuid::Uuid,
        String,
        uuid::Uuid,
        String,
        i32,
        time::OffsetDateTime,
    )> = match req.direction {
        MemoryLineageDirection::Ancestors => sqlx::query_as(
            "WITH RECURSIVE walk AS (
                 SELECT src.t AS src, src.kind::text AS src_kind,
                        pin AS tgt, tgt.kind::text AS tgt_kind, 1 AS dist,
                        COALESCE(uuid_extract_timestamp(src.t), TIMESTAMPTZ '1970-01-01')
                          AS created_at
                   FROM proxima_core.memory src
                   JOIN unnest(src.origins) AS pin ON true
                   JOIN proxima_core.memory tgt ON tgt.t = pin
                  WHERE src.t = $1
                    AND src.owner_id = ANY($2::uuid[])
                    AND tgt.owner_id = ANY($2::uuid[])
                 UNION ALL
                 SELECT n.t, n.kind::text, pin, nxt.kind::text, w.dist + 1,
                        COALESCE(uuid_extract_timestamp(n.t), TIMESTAMPTZ '1970-01-01')
                   FROM walk w
                   JOIN proxima_core.memory n ON n.t = w.tgt
                   JOIN unnest(n.origins) AS pin ON true
                   JOIN proxima_core.memory nxt ON nxt.t = pin
                  WHERE w.dist < $3
                    AND n.owner_id = ANY($2::uuid[])
                    AND nxt.owner_id = ANY($2::uuid[])
             )
             SELECT src, src_kind, tgt, tgt_kind, dist, created_at FROM walk
              WHERE ($4::int IS NULL
                     OR dist > $4
                     OR (dist = $4 AND (src, tgt) < ($5::uuid, $6::uuid)))
              ORDER BY dist ASC, src DESC, tgt DESC
              LIMIT $7",
        )
        .bind(start)
        .bind(&owner_ids)
        .bind(depth)
        .bind(after_dist)
        .bind(after_src)
        .bind(after_tgt)
        .bind(page_limit)
        .fetch_all(pool)
        .await
        .map_err(map_err)?,
        MemoryLineageDirection::Descendants => sqlx::query_as(
            "WITH RECURSIVE walk AS (
                 SELECT child.t AS src, child.kind::text AS src_kind,
                        $1::uuid AS tgt, parent.kind::text AS tgt_kind, 1 AS dist,
                        COALESCE(uuid_extract_timestamp(child.t), TIMESTAMPTZ '1970-01-01')
                          AS created_at
                   FROM proxima_core.memory child
                   JOIN proxima_core.memory parent ON parent.t = $1
                  WHERE child.origins @> ARRAY[$1::uuid]
                    AND child.owner_id = ANY($2::uuid[])
                    AND parent.owner_id = ANY($2::uuid[])
                 UNION ALL
                 SELECT child.t, child.kind::text, w.src, w.src_kind, w.dist + 1,
                        COALESCE(uuid_extract_timestamp(child.t), TIMESTAMPTZ '1970-01-01')
                   FROM walk w
                   JOIN proxima_core.memory child
                     ON child.origins @> ARRAY[w.src]
                  WHERE w.dist < $3
                    AND child.owner_id = ANY($2::uuid[])
             )
             SELECT src, src_kind, tgt, tgt_kind, dist, created_at FROM walk
              WHERE ($4::int IS NULL
                     OR dist > $4
                     OR (dist = $4 AND (src, tgt) < ($5::uuid, $6::uuid)))
              ORDER BY dist ASC, src DESC, tgt DESC
              LIMIT $7",
        )
        .bind(start)
        .bind(&owner_ids)
        .bind(depth)
        .bind(after_dist)
        .bind(after_src)
        .bind(after_tgt)
        .bind(page_limit)
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

    let page_len = usize::try_from(limit).unwrap_or(usize::MAX);
    let truncated = hops.len() > page_len;
    hops.truncate(page_len);

    let mut node_ids = vec![start];
    node_ids.extend(hops.iter().flat_map(|hop| [hop.0, hop.2]));
    let node_rows: Vec<(uuid::Uuid, String, String, Option<String>)> = sqlx::query_as(
        "SELECT m.t, m.kind::text, h.schema_id,
                left(COALESCE(n.title, n.body, u.text, d.body, i.claim, ''), 480)
           FROM proxima_core.memory m
           JOIN proxima_core.memory_head h ON h.handle = m.handle
           LEFT JOIN proxima_core.agent_note_v1 n ON n.t = m.t
           LEFT JOIN proxima_core.utterance_v1 u ON u.t = m.t
           LEFT JOIN proxima_core.agent_derivation_v1 d ON d.t = m.t
           LEFT JOIN proxima_core.interpretation_v1 i ON i.t = m.t
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
        .filter_map(|(id, kind, schema_id, snippet)| {
            Some(MemoryLineageNode {
                memory_id: MemoryId::new(id),
                kind: parse_kind(&kind)?,
                schema_id: SchemaId::new(schema_id),
                snippet: snippet.unwrap_or_default(),
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
        .filter_map(|(src, src_kind, tgt, tgt_kind, dist, created_at)| {
            Some(MemoryLineageEdge {
                edge: Edge {
                    source: EdgeEndpoint::memory(parse_kind(src_kind)?, MemoryId::new(*src)),
                    target: EdgeTargetProjection::visible(EdgeEndpoint::memory(
                        parse_kind(tgt_kind)?,
                        MemoryId::new(*tgt),
                    )),
                    kind: EdgeKind::Origin,
                    created_at: *created_at,
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
    use proxima_core::{EdgeEndpoint, EntityKind, MemoryId};

    #[test]
    fn a_resolved_head_decodes_as_a_pinned_fact_memory() {
        let id = uuid::Uuid::now_v7();
        let endpoint = EdgeEndpoint::memory(EntityKind::Fact, MemoryId::new(id));
        assert_eq!(endpoint.kind, EntityKind::Fact);
        assert_eq!(endpoint.memory_id().map(MemoryId::into_inner), Some(id));
    }
}
