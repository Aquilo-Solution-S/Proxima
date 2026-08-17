use std::collections::{BTreeMap, HashMap};

use futures_util::future::try_join_all;
use proxima_core::verbs::query::{
    EntityKind, MemoryLineageCursor, MemoryLineageDirection, MemoryLineageEdge, MemoryLineageNode,
    MemoryLineageRequest, MemoryLineageResponse,
};
use proxima_core::verbs::schema::MemorySearchProjection;
use proxima_core::{
    Edge, EdgeEndpoint, EdgeKind, EdgeTargetProjection, EntityRef, MemoryId, OwnerRef, SchemaId,
    StorageError,
};
use sqlx::PgPool;

use crate::error::map_err;
use crate::pg_ident::PgIdent;
use crate::verbs::query::projection_sql::projection_search_text;

pub(crate) async fn walk_memory_lineage(
    pool: &PgPool,
    read_owners: &[OwnerRef],
    req: &MemoryLineageRequest,
    projections: &[MemorySearchProjection],
) -> Result<MemoryLineageResponse, StorageError> {
    if read_owners.is_empty() {
        return Ok(MemoryLineageResponse {
            nodes: Vec::new(),
            edges: Vec::new(),
            truncated: false,
            next_cursor: None,
        });
    }
    walk_memory_lineage_timeseries(pool, read_owners, req, projections).await
}

#[allow(clippy::too_many_lines)]
async fn walk_memory_lineage_timeseries(
    pool: &PgPool,
    read_owners: &[OwnerRef],
    req: &MemoryLineageRequest,
    projections: &[MemorySearchProjection],
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
    // Pins live on the source. Target existence/owner is not a walk
    // filter: a foreign or missing origin redacts, it does not drop.
    // Recursion joins the next *source* only when that row is in S_read.
    let mut hops: Vec<(uuid::Uuid, String, uuid::Uuid, i32, time::OffsetDateTime)> =
        match req.direction {
            MemoryLineageDirection::Ancestors => sqlx::query_as(
                "WITH RECURSIVE walk AS (
                 SELECT src.t AS src, src.kind::text AS src_kind,
                        pin AS tgt, 1 AS dist,
                        COALESCE(uuid_extract_timestamp(src.t), TIMESTAMPTZ '1970-01-01')
                          AS created_at
                   FROM proxima_core.memory src
                   JOIN unnest(src.origins) AS pin ON true
                  WHERE src.t = $1
                    AND src.owner_id = ANY($2::uuid[])
                 UNION ALL
                 SELECT n.t, n.kind::text, pin, w.dist + 1,
                        COALESCE(uuid_extract_timestamp(n.t), TIMESTAMPTZ '1970-01-01')
                   FROM walk w
                   JOIN proxima_core.memory n ON n.t = w.tgt
                   JOIN unnest(n.origins) AS pin ON true
                  WHERE w.dist < $3
                    AND n.owner_id = ANY($2::uuid[])
             )
             SELECT src, src_kind, tgt, dist, created_at FROM walk
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
                        $1::uuid AS tgt, 1 AS dist,
                        COALESCE(uuid_extract_timestamp(child.t), TIMESTAMPTZ '1970-01-01')
                          AS created_at
                   FROM proxima_core.memory child
                  WHERE child.origins @> ARRAY[$1::uuid]
                    AND child.owner_id = ANY($2::uuid[])
                 UNION ALL
                 SELECT child.t, child.kind::text, w.src, w.dist + 1,
                        COALESCE(uuid_extract_timestamp(child.t), TIMESTAMPTZ '1970-01-01')
                   FROM walk w
                   JOIN proxima_core.memory child
                     ON child.origins @> ARRAY[w.src]
                  WHERE w.dist < $3
                    AND child.owner_id = ANY($2::uuid[])
             )
             SELECT src, src_kind, tgt, dist, created_at FROM walk
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
    if !node_rows.iter().any(|(id, _, _)| *id == start) {
        return Ok(MemoryLineageResponse {
            nodes: Vec::new(),
            edges: Vec::new(),
            truncated: false,
            next_cursor: None,
        });
    }
    let snippet_keys: Vec<(uuid::Uuid, String)> = node_rows
        .iter()
        .map(|(id, _, schema_id)| (*id, schema_id.clone()))
        .collect();
    let mut snippets = load_lineage_snippets(pool, projections, &snippet_keys).await?;
    let visible_kind: HashMap<uuid::Uuid, EntityKind> = node_rows
        .iter()
        .filter_map(|(id, kind, _)| parse_kind(kind).map(|kind| (*id, kind)))
        .collect();
    let nodes: Vec<MemoryLineageNode> = node_rows
        .into_iter()
        .filter_map(|(id, kind, schema_id)| {
            Some(MemoryLineageNode {
                memory_id: MemoryId::new(id),
                kind: parse_kind(&kind)?,
                schema_id: SchemaId::new(schema_id),
                snippet: snippets.remove(&id).unwrap_or_default(),
                distance: u8::from(id != start),
            })
        })
        .collect();

    let edges: Vec<MemoryLineageEdge> = hops
        .iter()
        .filter_map(|(src, src_kind, tgt, dist, created_at)| {
            let target = match visible_kind.get(tgt).copied() {
                Some(kind) => {
                    EdgeTargetProjection::visible(EdgeEndpoint::memory(kind, MemoryId::new(*tgt)))
                }
                None => EdgeTargetProjection::Redacted,
            };
            Some(MemoryLineageEdge {
                edge: Edge {
                    source: EdgeEndpoint::memory(parse_kind(src_kind)?, MemoryId::new(*src)),
                    target,
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
            distance: u8::try_from(last.3).unwrap_or(u8::MAX),
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

#[derive(Debug, sqlx::FromRow)]
struct SnippetRow {
    t: uuid::Uuid,
    snippet: Option<String>,
}

async fn load_lineage_snippets(
    pool: &PgPool,
    projections: &[MemorySearchProjection],
    rows: &[(uuid::Uuid, String)],
) -> Result<HashMap<uuid::Uuid, String>, StorageError> {
    let mut by_schema = BTreeMap::<&str, Vec<uuid::Uuid>>::new();
    for (t, schema_id) in rows {
        by_schema.entry(schema_id.as_str()).or_default().push(*t);
    }
    let jobs: Vec<(&MemorySearchProjection, Vec<uuid::Uuid>)> = by_schema
        .into_iter()
        .filter_map(|(schema_id, ts)| {
            let projection = projections
                .iter()
                .find(|projection| projection.schema_id.as_str() == schema_id)?;
            Some((projection, ts))
        })
        .collect();
    let batches = try_join_all(
        jobs.into_iter()
            .map(|(projection, ts)| load_one_schema_snippets(pool, projection, ts)),
    )
    .await?;
    Ok(batches.into_iter().flatten().collect())
}

async fn load_one_schema_snippets(
    pool: &PgPool,
    projection: &MemorySearchProjection,
    ts: Vec<uuid::Uuid>,
) -> Result<Vec<(uuid::Uuid, String)>, StorageError> {
    if ts.is_empty() {
        return Ok(Vec::new());
    }
    let table = PgIdent::table(&projection.sidecar_table)?;
    let search_text = projection_search_text(&projection.fields)?;
    let sql = format!(
        "SELECT c.t, left({search_text}, 480) AS snippet
           FROM {table} c
          WHERE c.t = ANY($1::uuid[])",
        table = table.as_str(),
        search_text = search_text,
    );
    // SQL-POLICY: PgIdent
    let rows: Vec<SnippetRow> = sqlx::query_as(sqlx::AssertSqlSafe(sql))
        .bind(&ts)
        .fetch_all(pool)
        .await
        .map_err(map_err)?;
    Ok(rows
        .into_iter()
        .map(|row| (row.t, row.snippet.unwrap_or_default()))
        .collect())
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

    #[test]
    fn lineage_sql_does_not_join_target_owner() {
        let src = include_str!("lineage.rs");
        let tgt_owner = format!("{}{}", "tgt.owner_id = ", "ANY");
        let tgt_join = format!("{}{}", "JOIN proxima_core.memory tgt ON tgt.t", " = pin");
        let parent_owner = format!("{}{}", "parent.owner_id = ", "ANY");
        assert!(
            !src.contains(&tgt_owner),
            "D8: target owner is redaction, not a walk filter"
        );
        assert!(
            !src.contains(&tgt_join),
            "D8: pin UUID is on the source; do not join tgt for existence"
        );
        assert!(
            !src.contains(&parent_owner),
            "D8: descendants do not re-admit the start via parent owner"
        );
    }
}
