use proxima_core::verbs::query::{
    EdgeExistsRequest, EdgeExistsResponse, EdgeReadCursor, EdgeReadRequest, EdgeReadResponse,
    QueryRequest,
};
use proxima_core::{Edge, EntityRef, OwnerRef, StorageError};
use sqlx::PgPool;

use crate::error::map_err;
use crate::verbs::edge_index::PgEndpointKind;

use super::rows::{EdgeRowDb, edge_from_db};
use super::{entity_owner_union, read_owner_columns, read_owner_predicate};

/// Hard upper bound on edges returned by snapshot-edge mode.
/// Decoupled from `QueryRequest::limit`, which sizes the node window.
pub const MAX_SNAPSHOT_EDGES: usize = 50_000;

/// One page of edges, newest first.
///
/// Ordering is `created_at DESC, (source, target, kind) DESC` — `created_at`
/// plus the whole primary key. There is no id to tie-break with, which is why
/// the rest of the key has to be in the order: the key is the row, so the key
/// is also what makes the order total and the keyset skip-free.
///
/// Fact-entity endpoints resolve through their current head, because that is
/// what a follow-head address means at read time. `target_unavailable` covers
/// both a compliance redaction and an endpoint whose row is gone.
fn read_edges_sql() -> String {
    format!(
        "
WITH edge_heads AS (
    SELECT e.source_kind, e.source_id, e.target_kind, e.target_id, e.kind, e.created_at,
           CASE WHEN e.source_kind = 'FactEntityHead'::proxima_core.edge_endpoint_kind
                THEN 'Fact'::proxima_core.edge_endpoint_kind ELSE e.source_kind END
                AS source_projected_kind,
           CASE WHEN e.target_kind = 'FactEntityHead'::proxima_core.edge_endpoint_kind
                THEN 'Fact'::proxima_core.edge_endpoint_kind ELSE e.target_kind END
                AS target_projected_kind,
           COALESCE(sfe.current_memory_id, e.source_id) AS source_entity_id,
           COALESCE(tfe.current_memory_id, e.target_id) AS target_entity_id,
           (etr.operation_id IS NOT NULL
            OR (e.target_kind = 'FactEntityHead'::proxima_core.edge_endpoint_kind
                AND tfe.fact_entity_id IS NULL)
            OR (e.target_kind = 'Goal'::proxima_core.edge_endpoint_kind
                AND tg.goal_id IS NULL)
            OR (e.target_kind NOT IN ('Goal'::proxima_core.edge_endpoint_kind,
                                      'FactEntityHead'::proxima_core.edge_endpoint_kind)
                AND tm.memory_id IS NULL)) AS target_unavailable
      FROM proxima_core.edges e
      LEFT JOIN proxima_core.fact_entities sfe
        ON e.source_kind = 'FactEntityHead'::proxima_core.edge_endpoint_kind
       AND sfe.fact_entity_id = e.source_id
      LEFT JOIN proxima_core.fact_entities tfe
        ON e.target_kind = 'FactEntityHead'::proxima_core.edge_endpoint_kind
       AND tfe.fact_entity_id = e.target_id
      LEFT JOIN proxima_core.memories tm
        ON e.target_kind NOT IN ('Goal'::proxima_core.edge_endpoint_kind,
                                 'FactEntityHead'::proxima_core.edge_endpoint_kind)
       AND tm.memory_id = e.target_id
      LEFT JOIN proxima_core.goals tg
        ON e.target_kind = 'Goal'::proxima_core.edge_endpoint_kind
       AND tg.goal_id = e.target_id
      LEFT JOIN proxima_core.compliance_edge_target_redactions etr
        ON etr.source_kind = e.source_kind AND etr.source_id = e.source_id
       AND etr.target_kind = e.target_kind AND etr.target_id = e.target_id
       AND etr.kind = e.kind
     WHERE ($5::proxima_core.edge_kind IS NULL OR e.kind = $5)
       AND ($6::uuid IS NULL OR (e.source_id = $6 AND e.source_kind = ANY($7::proxima_core.edge_endpoint_kind[])))
       AND ($8::uuid IS NULL OR (e.target_id = $8 AND e.target_kind = ANY($9::proxima_core.edge_endpoint_kind[])))
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
SELECT source_projected_kind AS source_kind, source_entity_id AS source_id,
       target_projected_kind AS target_kind, target_entity_id AS target_id,
       kind, created_at, target_visible, target_unavailable
  FROM visible
 WHERE source_readable
   AND NOT (source_world_visible AND NOT target_visible)
   AND ($8::uuid IS NULL OR target_visible)
   -- Keyset over the PROJECTED coordinates, which is what a cursor is
   -- handed out in: an edge has no id, so the position after created_at is
   -- the rest of the key. The endpoint kinds are omitted because the
   -- endpoint ids already determine them — a uuid names at most one row in
   -- one table — so the triple is total on its own.
   AND ($10::timestamptz IS NULL
        OR (created_at, source_entity_id, target_entity_id, kind)
           < ($10, $11::uuid, $12::uuid, $13::proxima_core.edge_kind))
 ORDER BY created_at DESC, source_entity_id DESC, target_entity_id DESC, kind DESC
 LIMIT $14
",
        entity_owner_union = entity_owner_union(),
        source_read_owner_predicate = read_owner_predicate("seo", "rs"),
        target_read_owner_predicate = read_owner_predicate("teo", "rs"),
    )
}

/// Endpoint kinds an `EntityRef` filter may address.
///
/// An `EntityRef` names an id and an address form, not a layer, so a memory
/// filter admits all three memory kinds. The form is still checked: a
/// Fact-entity head and a memory row are different endpoints even when a
/// caller supplies the same uuid.
fn filter_endpoint_kinds(entity: EntityRef) -> Vec<PgEndpointKind> {
    match entity {
        EntityRef::Memory(_) => vec![
            PgEndpointKind::Fact,
            PgEndpointKind::Abstraction,
            PgEndpointKind::Perspective,
        ],
        EntityRef::Goal(_) => vec![PgEndpointKind::Goal],
        EntityRef::FactEntity(_) => vec![PgEndpointKind::FactEntityHead],
    }
}

fn entity_ref_id(entity: EntityRef) -> uuid::Uuid {
    match entity {
        EntityRef::Memory(id) => id.into_inner(),
        EntityRef::Goal(id) => id.into_inner(),
        EntityRef::FactEntity(id) => id.into_inner(),
    }
}

pub(crate) async fn read_edges(
    pool: &PgPool,
    read_owners: &[OwnerRef],
    req: &EdgeReadRequest,
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
        .bind(req.filter.kind)
        .bind(req.filter.source.map(entity_ref_id))
        .bind(
            req.filter
                .source
                .map(filter_endpoint_kinds)
                .unwrap_or_default(),
        )
        .bind(req.filter.target.map(entity_ref_id))
        .bind(
            req.filter
                .target
                .map(filter_endpoint_kinds)
                .unwrap_or_default(),
        )
        .bind(req.cursor.map(|cursor| cursor.created_at))
        .bind(req.cursor.map(|cursor| entity_ref_id(cursor.source)))
        .bind(req.cursor.map(|cursor| entity_ref_id(cursor.target)))
        .bind(req.cursor.map(|cursor| cursor.kind))
        .bind(fetch_limit)
        .fetch_all(pool)
        .await
        .map_err(map_err)?;
    let has_more = rows.len() > limit;
    rows.truncate(limit);
    let edges = rows.iter().map(edge_from_db).collect::<Vec<_>>();
    let next_cursor = if has_more {
        edges.last().and_then(edge_read_cursor)
    } else {
        None
    };
    Ok(EdgeReadResponse { edges, next_cursor })
}

/// The keyset position of one returned edge. `None` for an edge whose target
/// is withheld — there is no coordinate to resume from that the reader is
/// allowed to know.
fn edge_read_cursor(edge: &Edge) -> Option<EdgeReadCursor> {
    edge.target.endpoint().map(|target| EdgeReadCursor {
        created_at: edge.created_at,
        source: edge.source.entity,
        target: target.entity,
        kind: edge.kind,
    })
}

pub(crate) async fn edge_exists(
    pool: &PgPool,
    read_owners: &[OwnerRef],
    req: &EdgeExistsRequest,
) -> Result<EdgeExistsResponse, StorageError> {
    let read = EdgeReadRequest {
        owner: req.owner,
        filter: req.filter.clone(),
        limit: 1,
        cursor: None,
    };
    let response = read_edges(pool, read_owners, &read).await?;
    Ok(EdgeExistsResponse {
        exists: !response.edges.is_empty(),
    })
}

/// Snapshot-mode edges: every edge whose two endpoints are both inside the
/// node window the query already returned.
pub(super) async fn query_edges(
    pool: &PgPool,
    req: &QueryRequest,
    visible_memory_ids: &[uuid::Uuid],
    visible_goal_ids: &[uuid::Uuid],
) -> Result<Vec<Edge>, StorageError> {
    let id_hydration = !req.memory_ids.is_empty() || !req.goal_ids.is_empty();
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

/// Snapshot closure. `edge_heads` filters the edges scan on the RAW
/// endpoint columns — the window's own ids plus the fact-entity ids
/// currently heading a windowed memory (`head_probe`, riding
/// `idx_fact_entities_current_memory`, migration 0017) — before resolving
/// heads, so it rides `idx_edges_source`/`idx_edges_target` instead of
/// resolving every edge in the table.
///
/// The prefilter is an exact superset of the resolved predicate it guards: a
/// resolved id matches only via the raw id or via a collected head id. The
/// original resolved-column filter is kept verbatim as the residual, so the
/// row set cannot change.
const EDGES_BETWEEN_VISIBLE_NODES_SQL: &str = "WITH head_probe AS (
             SELECT COALESCE(array_agg(fact_entity_id), '{}') AS ids
               FROM proxima_core.fact_entities
              WHERE current_memory_id = ANY($1::uuid[])
                 OR current_memory_id = ANY($2::uuid[])
         ),
         edge_heads AS (
             SELECT e.kind, e.created_at,
                    CASE WHEN e.source_kind = 'FactEntityHead'::proxima_core.edge_endpoint_kind
                         THEN 'Fact'::proxima_core.edge_endpoint_kind ELSE e.source_kind END
                         AS source_kind,
                    CASE WHEN e.target_kind = 'FactEntityHead'::proxima_core.edge_endpoint_kind
                         THEN 'Fact'::proxima_core.edge_endpoint_kind ELSE e.target_kind END
                         AS target_kind,
                    COALESCE(sfe.current_memory_id, e.source_id) AS source_id,
                    COALESCE(tfe.current_memory_id, e.target_id) AS target_id
               FROM proxima_core.edges e
               LEFT JOIN proxima_core.fact_entities sfe
                 ON e.source_kind = 'FactEntityHead'::proxima_core.edge_endpoint_kind
                AND sfe.fact_entity_id = e.source_id
               LEFT JOIN proxima_core.fact_entities tfe
                 ON e.target_kind = 'FactEntityHead'::proxima_core.edge_endpoint_kind
                AND tfe.fact_entity_id = e.target_id
              WHERE (e.source_id = ANY($1::uuid[])
                     OR e.source_id = ANY($2::uuid[])
                     OR e.source_id = ANY((SELECT ids FROM head_probe)::uuid[]))
                AND (e.target_id = ANY($1::uuid[])
                     OR e.target_id = ANY($2::uuid[])
                     OR e.target_id = ANY((SELECT ids FROM head_probe)::uuid[]))
         )
         SELECT source_kind, source_id, target_kind, target_id, kind, created_at,
                true AS target_visible, false AS target_unavailable
           FROM edge_heads
          WHERE (source_id = ANY($1::uuid[]) OR source_id = ANY($2::uuid[]))
            AND (target_id = ANY($1::uuid[]) OR target_id = ANY($2::uuid[]))
          ORDER BY created_at DESC, source_kind DESC, source_id DESC,
                   target_kind DESC, target_id DESC, kind DESC
          LIMIT $3";

/// The snapshot-closure statement the given tuning selects, for plan and
/// equivalence assertions in tests. Same cfg gate as the search
/// `*_sql_for_tests` exports.
#[cfg(any(test, feature = "test-fixtures", debug_assertions))]
#[doc(hidden)]
#[must_use]
pub fn edges_between_visible_nodes_sql_for_tests() -> &'static str {
    EDGES_BETWEEN_VISIBLE_NODES_SQL
}

async fn query_edges_between_visible_nodes(
    pool: &PgPool,
    visible_memory_ids: &[uuid::Uuid],
    visible_goal_ids: &[uuid::Uuid],
) -> Result<Vec<Edge>, StorageError> {
    let rows = sqlx::query_as::<_, EdgeRowDb>(EDGES_BETWEEN_VISIBLE_NODES_SQL)
        .bind(visible_memory_ids)
        .bind(visible_goal_ids)
        .bind(i64::try_from(MAX_SNAPSHOT_EDGES).expect("MAX_SNAPSHOT_EDGES fits in i64"))
        .fetch_all(pool)
        .await
        .map_err(map_err)?;
    Ok(rows.iter().map(edge_from_db).collect())
}
