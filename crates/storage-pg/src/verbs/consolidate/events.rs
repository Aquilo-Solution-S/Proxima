use proxima_core::read_models::ChangeEventForWake;
use proxima_core::{Owner, OwnerRef, OwnerRefKind, StorageError};
use sqlx::PgPool;
use sqlx::Row;

use crate::change_event::hydrate_change_event;
use crate::error::map_err;

pub async fn list_change_events_after(
    pool: &PgPool,
    read_owners: &[OwnerRef],
    after: uuid::Uuid,
    limit: usize,
) -> Result<Vec<ChangeEventForWake>, StorageError> {
    if read_owners.is_empty() {
        return Ok(Vec::new());
    }
    let (read_owner_kinds, read_owner_ids) = read_owner_columns(read_owners);
    let (world_kind, world_id) =
        crate::access::owner_columns::owner_binds(&proxima_core::access::world());
    let edge_visibility = edge_event_visibility_predicate(1, 2, 5, 6);
    let sql = format!(
        "SELECT ce.seq
             FROM proxima_core.change_event ce
             WHERE EXISTS (
                SELECT 1
                  FROM unnest($1::proxima_core.owner_ref_kind[], $2::uuid[]) AS s(kind, id)
                 WHERE ce.owner_kind = s.kind
                   AND ce.owner_id IS NOT DISTINCT FROM s.id
             )
               AND ce.seq > $3
               AND {edge_visibility}
             ORDER BY ce.seq ASC
             LIMIT $4"
    );
    let rows = sqlx::query(&sql)
        .bind(&read_owner_kinds)
        .bind(&read_owner_ids)
        .bind(after)
        .bind(i64::try_from(limit).unwrap_or(i64::MAX))
        .bind(world_kind)
        .bind(world_id)
        .fetch_all(pool)
        .await
        .map_err(map_err)?;

    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let seq: uuid::Uuid = r.try_get("seq").map_err(map_err)?;
        if let Some(event) = hydrate_change_event(pool, read_owners, seq).await? {
            out.push(ChangeEventForWake { event });
        }
    }
    Ok(out)
}

fn read_owner_columns(read_owners: &[OwnerRef]) -> (Vec<OwnerRefKind>, Vec<Option<uuid::Uuid>>) {
    crate::access::owner_columns::owner_arrays(read_owners)
}

pub async fn list_change_events_for_replay(
    pool: &PgPool,
    owner: &Owner,
    after: uuid::Uuid,
    until: Option<uuid::Uuid>,
    limit: usize,
) -> Result<Vec<ChangeEventForWake>, StorageError> {
    let (read_owner_kinds, read_owner_ids) = read_owner_columns(std::slice::from_ref(owner));
    let (world_kind, world_id) =
        crate::access::owner_columns::owner_binds(&proxima_core::access::world());
    let edge_visibility = edge_event_visibility_predicate(1, 2, 6, 7);
    let sql = format!(
        "SELECT ce.seq
             FROM proxima_core.change_event ce
             WHERE EXISTS (
                SELECT 1
                  FROM unnest($1::proxima_core.owner_ref_kind[], $2::uuid[]) AS s(kind, id)
                 WHERE ce.owner_kind = s.kind
                   AND ce.owner_id IS NOT DISTINCT FROM s.id
             )
               AND ce.seq > $3
               AND ($4::uuid IS NULL OR ce.seq <= $4)
               AND {edge_visibility}
             ORDER BY ce.seq ASC
             LIMIT $5"
    );
    let rows = sqlx::query(&sql)
        .bind(&read_owner_kinds)
        .bind(&read_owner_ids)
        .bind(after)
        .bind(until)
        .bind(i64::try_from(limit).unwrap_or(i64::MAX))
        .bind(world_kind)
        .bind(world_id)
        .fetch_all(pool)
        .await
        .map_err(map_err)?;

    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let seq: uuid::Uuid = r.try_get("seq").map_err(map_err)?;
        if let Some(event) = hydrate_change_event(pool, std::slice::from_ref(owner), seq).await? {
            out.push(ChangeEventForWake { event });
        }
    }
    Ok(out)
}

/// Source-owned edge visibility for `change_event` edge rows, matching
/// `read_edges`: source readable and not (source World-readable and target
/// unreadable). Fact-entity endpoints resolve through their current head.
#[must_use]
pub(crate) fn edge_event_visibility_predicate(
    read_kinds_param: usize,
    read_ids_param: usize,
    world_kind_param: usize,
    world_id_param: usize,
) -> String {
    let source_entity = "COALESCE(
        ce.edge_source_memory_id, ce.edge_source_goal_id,
        (SELECT fe.current_memory_id FROM proxima_core.fact_entities fe
          WHERE fe.fact_entity_id = ce.edge_source_fact_entity_id))";
    let target_entity = "COALESCE(
        ce.edge_target_memory_id, ce.edge_target_goal_id,
        (SELECT fe.current_memory_id FROM proxima_core.fact_entities fe
          WHERE fe.fact_entity_id = ce.edge_target_fact_entity_id))";
    format!(
        "(
                    ce.edge_id IS NULL
                    OR (
                        EXISTS (
                            SELECT 1
                              FROM (SELECT memory_id AS entity_id, owner_kind, owner_id FROM proxima_core.memories UNION ALL SELECT goal_id AS entity_id, owner_kind, owner_id FROM proxima_core.goals) seo
                              JOIN unnest(${read_kinds_param}::proxima_core.owner_ref_kind[], ${read_ids_param}::uuid[]) AS rs(kind, id)
                                ON seo.owner_kind = rs.kind
                               AND seo.owner_id IS NOT DISTINCT FROM rs.id
                             WHERE seo.entity_id = {source_entity}
                        )
                        AND NOT (
                            EXISTS (
                                SELECT 1
                                  FROM (SELECT memory_id AS entity_id, owner_kind, owner_id FROM proxima_core.memories UNION ALL SELECT goal_id AS entity_id, owner_kind, owner_id FROM proxima_core.goals) weo
                                 WHERE weo.entity_id = {source_entity}
                                   AND weo.owner_kind = ${world_kind_param}
                                   AND weo.owner_id IS NOT DISTINCT FROM ${world_id_param}
                            )
                            AND NOT EXISTS (
                                SELECT 1
                                  FROM (SELECT memory_id AS entity_id, owner_kind, owner_id FROM proxima_core.memories UNION ALL SELECT goal_id AS entity_id, owner_kind, owner_id FROM proxima_core.goals) teo
                                  JOIN unnest(${read_kinds_param}::proxima_core.owner_ref_kind[], ${read_ids_param}::uuid[]) AS rt(kind, id)
                                    ON teo.owner_kind = rt.kind
                                   AND teo.owner_id IS NOT DISTINCT FROM rt.id
                                 WHERE teo.entity_id = {target_entity}
                            )
                        )
                    )
               )"
    )
}
