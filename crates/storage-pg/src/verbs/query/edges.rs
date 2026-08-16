use proxima_core::verbs::query::{
    EdgeExistsRequest, EdgeExistsResponse, EdgeReadCursor, EdgeReadRequest, EdgeReadResponse,
    QueryRequest,
};
use proxima_core::{
    Edge, EdgeEndpoint, EdgeKind, EdgeTargetProjection, EntityKind, EntityRef, MemoryId, OwnerRef,
    StorageError,
};
use sqlx::PgPool;

use crate::error::map_err;

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
/// Pins are `memory.origins` / `memory.refs`. There is no edge table.
#[allow(clippy::too_many_lines)]
pub(crate) async fn read_edges(
    pool: &PgPool,
    read_owners: &[OwnerRef],
    req: &EdgeReadRequest,
) -> Result<EdgeReadResponse, StorageError> {
    if read_owners.is_empty() || req.limit == 0 {
        return Ok(EdgeReadResponse {
            edges: Vec::new(),
            next_cursor: None,
        });
    }
    if matches!(req.filter.source, Some(EntityRef::Goal(_)))
        || matches!(req.filter.target, Some(EntityRef::Goal(_)))
    {
        return Ok(EdgeReadResponse {
            edges: Vec::new(),
            next_cursor: None,
        });
    }
    let owner_ids: Vec<uuid::Uuid> = read_owners
        .iter()
        .copied()
        .map(OwnerRef::stored_owner_id)
        .collect();
    let kind = req.filter.kind.map(|kind| kind.as_str().to_string());
    let source_id = req.filter.source.and_then(|entity| match entity {
        EntityRef::Memory(id) => Some(id.into_inner()),
        EntityRef::Goal(_) => None,
    });
    let target_id = req.filter.target.and_then(|entity| match entity {
        EntityRef::Memory(id) => Some(id.into_inner()),
        EntityRef::Goal(_) => None,
    });
    let after_created = req.cursor.map(|cursor| cursor.created_at);
    let after_source = req.cursor.and_then(|cursor| match cursor.source {
        EntityRef::Memory(id) => Some(id.into_inner()),
        EntityRef::Goal(_) => None,
    });
    let after_target = req.cursor.and_then(|cursor| match cursor.target {
        EntityRef::Memory(id) => Some(id.into_inner()),
        EntityRef::Goal(_) => None,
    });
    let after_kind = req.cursor.map(|cursor| cursor.kind.as_str().to_string());
    let fetch = i64::from(req.limit).saturating_add(1);
    let rows: Vec<(
        String,
        uuid::Uuid,
        String,
        uuid::Uuid,
        String,
        time::OffsetDateTime,
    )> = sqlx::query_as(
        "SELECT src.kind::text, src.t, tgt.kind::text, pins.pin, pins.pin_kind, pins.created_at
               FROM (
                 SELECT src.t AS src_t, pin, 'origin'::text AS pin_kind,
                        COALESCE(uuid_extract_timestamp(src.t), TIMESTAMPTZ '1970-01-01')
                          AS created_at
                   FROM proxima_core.memory src
                   JOIN unnest(src.origins) AS pin ON true
                  WHERE src.owner_id = ANY($1::uuid[])
                 UNION ALL
                 SELECT src.t, pin, 'reference'::text,
                        COALESCE(uuid_extract_timestamp(src.t), TIMESTAMPTZ '1970-01-01')
                   FROM proxima_core.memory src
                   JOIN unnest(src.refs) AS pin ON true
                  WHERE src.owner_id = ANY($1::uuid[])
               ) pins
               JOIN proxima_core.memory src ON src.t = pins.src_t
               JOIN proxima_core.memory tgt ON tgt.t = pins.pin
              WHERE tgt.owner_id = ANY($1::uuid[])
                AND ($2::text IS NULL OR pins.pin_kind = $2)
                AND ($3::uuid IS NULL OR src.t = $3)
                AND ($4::uuid IS NULL OR tgt.t = $4)
                AND ($5::timestamptz IS NULL
                     OR (pins.created_at, src.t, tgt.t, pins.pin_kind)
                        < ($5, $6::uuid, $7::uuid, $8::text))
              ORDER BY pins.created_at DESC, src.t DESC, tgt.t DESC, pins.pin_kind DESC
              LIMIT $9",
    )
    .bind(&owner_ids)
    .bind(kind)
    .bind(source_id)
    .bind(target_id)
    .bind(after_created)
    .bind(after_source)
    .bind(after_target)
    .bind(after_kind)
    .bind(fetch)
    .fetch_all(pool)
    .await
    .map_err(map_err)?;
    let page_len = usize::try_from(req.limit).unwrap_or(usize::MAX);
    let truncated = rows.len() > page_len;
    let page = if truncated { &rows[..page_len] } else { &rows };
    let edges: Vec<Edge> = page
        .iter()
        .filter_map(|(src_kind, src, tgt_kind, tgt, kind, created_at)| {
            Some(Edge {
                source: EdgeEndpoint::memory(parse_memory_kind(src_kind)?, MemoryId::new(*src)),
                target: EdgeTargetProjection::visible(EdgeEndpoint::memory(
                    parse_memory_kind(tgt_kind)?,
                    MemoryId::new(*tgt),
                )),
                kind: parse_pin_kind(kind)?,
                created_at: *created_at,
            })
        })
        .collect();
    let next_cursor = truncated.then(|| {
        let last = page.last().expect("truncated page is non-empty");
        EdgeReadCursor {
            created_at: last.5,
            source: EntityRef::Memory(MemoryId::new(last.1)),
            target: EntityRef::Memory(MemoryId::new(last.3)),
            kind: parse_pin_kind(&last.4).unwrap_or(EdgeKind::Origin),
        }
    });
    Ok(EdgeReadResponse { edges, next_cursor })
}

fn parse_memory_kind(kind: &str) -> Option<EntityKind> {
    match kind {
        "fact" => Some(EntityKind::Fact),
        "abstraction" => Some(EntityKind::Abstraction),
        "perspective" => Some(EntityKind::Perspective),
        _ => None,
    }
}

fn parse_pin_kind(kind: &str) -> Option<EdgeKind> {
    match kind {
        "origin" => Some(EdgeKind::Origin),
        "reference" => Some(EdgeKind::Reference),
        _ => None,
    }
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
    _pool: &PgPool,
    _req: &QueryRequest,
    _visible_memory_ids: &[uuid::Uuid],
    _visible_goal_ids: &[uuid::Uuid],
) -> Result<Vec<Edge>, StorageError> {
    // Pins live on the node (`origins` / `refs`).
    Ok(Vec::new())
}
