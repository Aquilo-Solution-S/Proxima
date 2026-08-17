use std::collections::{BTreeMap, HashMap, HashSet};

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
use uuid::Uuid;

use crate::error::map_err;
use crate::pg_ident::PgIdent;
use crate::verbs::query::projection_sql::projection_search_text;

/// Outbound origin pins of the frontier, newest `(src, tgt)` first.
const ANCESTOR_HOP_SQL: &str = "SELECT DISTINCT ON (m.t, pin)
       m.t AS src, m.kind::text AS src_kind, pin AS tgt,
       COALESCE(uuid_extract_timestamp(m.t), TIMESTAMPTZ '1970-01-01') AS created_at
  FROM proxima_core.memory m
  JOIN unnest(m.origins) AS pin ON true
 WHERE m.t = ANY($1::uuid[])
   AND m.owner_id = ANY($2::uuid[])
   AND ($3::uuid IS NULL OR (m.t, pin) < ($3::uuid, $4::uuid))
 ORDER BY m.t DESC, pin DESC
 LIMIT $5";

/// Children whose `origins` overlap the frontier; `tgt` is only a parent
/// in that frontier (not the child's other pins).
const DESCENDANT_HOP_SQL: &str = "SELECT DISTINCT ON (child.t, pin)
       child.t AS src, child.kind::text AS src_kind, pin AS tgt,
       COALESCE(uuid_extract_timestamp(child.t), TIMESTAMPTZ '1970-01-01') AS created_at
  FROM proxima_core.memory child
  JOIN unnest(child.origins) AS pin ON true
 WHERE child.owner_id = ANY($2::uuid[])
   AND child.origins && $1::uuid[]
   AND pin = ANY($1::uuid[])
   AND ($3::uuid IS NULL OR (child.t, pin) < ($3::uuid, $4::uuid))
 ORDER BY child.t DESC, pin DESC
 LIMIT $5";

const ANCESTOR_FRONTIER_SQL: &str = "SELECT DISTINCT pin
  FROM proxima_core.memory m
  JOIN unnest(m.origins) AS pin ON true
 WHERE m.t = ANY($1::uuid[])
   AND m.owner_id = ANY($2::uuid[])";

const DESCENDANT_FRONTIER_SQL: &str = "SELECT DISTINCT child.t
  FROM proxima_core.memory child
 WHERE child.owner_id = ANY($2::uuid[])
   AND child.origins && $1::uuid[]";

#[cfg(any(test, feature = "test-fixtures", debug_assertions))]
#[doc(hidden)]
#[must_use]
pub fn ancestor_hop_sql_for_tests() -> &'static str {
    ANCESTOR_HOP_SQL
}

#[cfg(any(test, feature = "test-fixtures", debug_assertions))]
#[doc(hidden)]
#[must_use]
pub fn descendant_hop_sql_for_tests() -> &'static str {
    DESCENDANT_HOP_SQL
}

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

async fn walk_memory_lineage_timeseries(
    pool: &PgPool,
    read_owners: &[OwnerRef],
    req: &MemoryLineageRequest,
    projections: &[MemorySearchProjection],
) -> Result<MemoryLineageResponse, StorageError> {
    let owner_ids: Vec<Uuid> = read_owners
        .iter()
        .copied()
        .map(OwnerRef::stored_owner_id)
        .collect();
    let start = req.start_memory_id.into_inner();
    let depth = req.depth.clamp(1, 8);
    let limit = req.limit.min(200);
    let page_len = usize::try_from(limit).unwrap_or(usize::MAX);
    let page_limit = page_len.saturating_add(1);
    // Pins live on the source. Target existence/owner is not a walk
    // filter: a foreign or missing origin redacts, it does not drop.
    // The next source is admitted only when that row is in S_read.
    let mut hops = walk_lineage_hops(
        pool,
        req.direction,
        start,
        &owner_ids,
        depth,
        page_limit,
        req.after,
    )
    .await?;

    let truncated = hops.len() > page_len;
    hops.truncate(page_len);
    assemble_lineage_page(pool, &owner_ids, start, hops, truncated, projections).await
}

async fn assemble_lineage_page(
    pool: &PgPool,
    owner_ids: &[Uuid],
    start: Uuid,
    hops: Vec<WalkHop>,
    truncated: bool,
    projections: &[MemorySearchProjection],
) -> Result<MemoryLineageResponse, StorageError> {
    let mut node_ids = vec![start];
    node_ids.extend(hops.iter().flat_map(|hop| [hop.src, hop.tgt]));
    let node_rows: Vec<(Uuid, String, String)> = sqlx::query_as(
        "SELECT m.t, m.kind::text, m.schema_id
           FROM proxima_core.memory m
          WHERE m.t = ANY($1::uuid[])
            AND m.owner_id = ANY($2::uuid[])",
    )
    .bind(&node_ids)
    .bind(owner_ids)
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
    let snippet_keys: Vec<(Uuid, String)> = node_rows
        .iter()
        .map(|(id, _, schema_id)| (*id, schema_id.clone()))
        .collect();
    let mut snippets = load_lineage_snippets(pool, projections, &snippet_keys).await?;
    let visible_kind: HashMap<Uuid, EntityKind> = node_rows
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
        .filter_map(|hop| project_lineage_edge(hop, &visible_kind))
        .collect();
    let next_cursor = truncated.then(|| {
        let last = hops.last().expect("truncated page is non-empty");
        MemoryLineageCursor {
            distance: last.dist,
            source: EntityRef::Memory(MemoryId::new(last.src)),
            target: EntityRef::Memory(MemoryId::new(last.tgt)),
        }
    });
    Ok(MemoryLineageResponse {
        nodes,
        edges,
        truncated,
        next_cursor,
    })
}

fn project_lineage_edge(
    hop: &WalkHop,
    visible_kind: &HashMap<Uuid, EntityKind>,
) -> Option<MemoryLineageEdge> {
    let target = match visible_kind.get(&hop.tgt).copied() {
        Some(kind) => {
            EdgeTargetProjection::visible(EdgeEndpoint::memory(kind, MemoryId::new(hop.tgt)))
        }
        None => EdgeTargetProjection::Redacted,
    };
    Some(MemoryLineageEdge {
        edge: Edge {
            source: EdgeEndpoint::memory(parse_kind(&hop.src_kind)?, MemoryId::new(hop.src)),
            target,
            kind: EdgeKind::Origin,
            created_at: hop.created_at,
        },
        distance: hop.dist,
    })
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct HopRow {
    src: Uuid,
    src_kind: String,
    tgt: Uuid,
    created_at: time::OffsetDateTime,
}

#[derive(Debug, Clone)]
struct WalkHop {
    src: Uuid,
    src_kind: String,
    tgt: Uuid,
    dist: u8,
    created_at: time::OffsetDateTime,
}

fn resume_point(after: Option<MemoryLineageCursor>) -> (u8, Option<(Uuid, Uuid)>) {
    let Some(after) = after else {
        return (1, None);
    };
    match (after.source, after.target) {
        (EntityRef::Memory(src), EntityRef::Memory(tgt)) => (
            after.distance.max(1),
            Some((src.into_inner(), tgt.into_inner())),
        ),
        _ => (after.distance.saturating_add(1).max(1), None),
    }
}

async fn walk_lineage_hops(
    pool: &PgPool,
    direction: MemoryLineageDirection,
    start: Uuid,
    owner_ids: &[Uuid],
    depth: u8,
    page_limit: usize,
    after: Option<MemoryLineageCursor>,
) -> Result<Vec<WalkHop>, StorageError> {
    let (start_d, mut keyset) = resume_point(after);
    if start_d > depth {
        return Ok(Vec::new());
    }
    let mut seen = HashSet::from([start]);
    let mut frontier = vec![start];
    for _ in 1..start_d {
        if frontier.is_empty() {
            return Ok(Vec::new());
        }
        frontier = take_unseen_frontier(
            next_frontier(pool, direction, &frontier, owner_ids).await?,
            &mut seen,
        );
    }
    let mut hops = Vec::new();
    let mut dist = start_d;
    while dist <= depth && hops.len() < page_limit && !frontier.is_empty() {
        let remaining = i64::try_from(page_limit - hops.len()).unwrap_or(i64::MAX);
        let after_pair = keyset.take();
        let rows = hop_edges(pool, direction, &frontier, owner_ids, after_pair, remaining).await?;
        hops.extend(rows.into_iter().map(|row| WalkHop {
            src: row.src,
            src_kind: row.src_kind,
            tgt: row.tgt,
            dist,
            created_at: row.created_at,
        }));
        if hops.len() >= page_limit {
            break;
        }
        frontier = take_unseen_frontier(
            next_frontier(pool, direction, &frontier, owner_ids).await?,
            &mut seen,
        );
        dist = dist.saturating_add(1);
    }
    Ok(hops)
}

fn take_unseen_frontier(next: Vec<Uuid>, seen: &mut HashSet<Uuid>) -> Vec<Uuid> {
    next.into_iter().filter(|id| seen.insert(*id)).collect()
}

async fn hop_edges(
    pool: &PgPool,
    direction: MemoryLineageDirection,
    frontier: &[Uuid],
    owner_ids: &[Uuid],
    after: Option<(Uuid, Uuid)>,
    limit: i64,
) -> Result<Vec<HopRow>, StorageError> {
    if frontier.is_empty() || limit <= 0 {
        return Ok(Vec::new());
    }
    let (after_src, after_tgt) = match after {
        Some((src, tgt)) => (Some(src), Some(tgt)),
        None => (None, None),
    };
    let query = match direction {
        MemoryLineageDirection::Ancestors => sqlx::query_as(ANCESTOR_HOP_SQL),
        MemoryLineageDirection::Descendants => sqlx::query_as(DESCENDANT_HOP_SQL),
    };
    query
        .bind(frontier)
        .bind(owner_ids)
        .bind(after_src)
        .bind(after_tgt)
        .bind(limit)
        .fetch_all(pool)
        .await
        .map_err(map_err)
}

async fn next_frontier(
    pool: &PgPool,
    direction: MemoryLineageDirection,
    frontier: &[Uuid],
    owner_ids: &[Uuid],
) -> Result<Vec<Uuid>, StorageError> {
    if frontier.is_empty() {
        return Ok(Vec::new());
    }
    let query = match direction {
        MemoryLineageDirection::Ancestors => sqlx::query_scalar(ANCESTOR_FRONTIER_SQL),
        MemoryLineageDirection::Descendants => sqlx::query_scalar(DESCENDANT_FRONTIER_SQL),
    };
    query
        .bind(frontier)
        .bind(owner_ids)
        .fetch_all(pool)
        .await
        .map_err(map_err)
}

#[derive(Debug, sqlx::FromRow)]
struct SnippetRow {
    t: Uuid,
    snippet: Option<String>,
}

async fn load_lineage_snippets(
    pool: &PgPool,
    projections: &[MemorySearchProjection],
    rows: &[(Uuid, String)],
) -> Result<HashMap<Uuid, String>, StorageError> {
    let mut by_schema = BTreeMap::<&str, Vec<Uuid>>::new();
    for (t, schema_id) in rows {
        by_schema.entry(schema_id.as_str()).or_default().push(*t);
    }
    let jobs: Vec<(&MemorySearchProjection, Vec<Uuid>)> = by_schema
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
    ts: Vec<Uuid>,
) -> Result<Vec<(Uuid, String)>, StorageError> {
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

    use super::{ANCESTOR_HOP_SQL, DESCENDANT_HOP_SQL};

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
        let prod = src.split("mod tests").next().expect("production");
        let tgt_owner = format!("{}{}", "tgt.owner_id = ", "ANY");
        let tgt_join = format!("{}{}", "JOIN proxima_core.memory tgt ON tgt.t", " = pin");
        let parent_owner = format!("{}{}", "parent.owner_id = ", "ANY");
        assert!(
            !prod.contains(&tgt_owner),
            "D8: target owner is redaction, not a walk filter"
        );
        assert!(
            !prod.contains(&tgt_join),
            "D8: pin UUID is on the source; do not join tgt for existence"
        );
        assert!(
            !prod.contains(&parent_owner),
            "D8: descendants do not re-admit the start via parent owner"
        );
        let head_schema = format!("{}{}", "h.schema", "_id");
        assert!(
            !prod.contains(&head_schema),
            "W5: lineage reads schema_id from memory, not memory_head"
        );
    }

    #[test]
    fn lineage_sql_is_level_hops() {
        let src = include_str!("lineage.rs");
        let prod = src.split("mod tests").next().expect("production");
        let recursive = format!("{}{}", "WITH RECUR", "SIVE");
        let union_all = format!("{}{}", "UNION ", "ALL");
        assert!(
            !prod.contains(&recursive),
            "lineage must not expand a recursive CTE"
        );
        assert!(
            !prod.contains(&union_all),
            "lineage must not UNION ALL paths"
        );
        assert!(
            ANCESTOR_HOP_SQL.contains("LIMIT") && DESCENDANT_HOP_SQL.contains("LIMIT"),
            "each hop query must page in SQL"
        );
    }
}
