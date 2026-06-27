use proxima_core::personality::{ChangeEventForWake, PersonalityInstanceId, WakeChainDepth};
use proxima_core::{Owner, OwnerPrincipalKind, Principal, StorageError};
use sqlx::PgPool;
use sqlx::Row;

use crate::change_event::hydrate_change_event;
use crate::error::map_err;

pub async fn list_change_events_after(
    pool: &PgPool,
    read_owners: &[Principal],
    after: uuid::Uuid,
    limit: usize,
) -> Result<Vec<ChangeEventForWake>, StorageError> {
    if read_owners.is_empty() {
        return Ok(Vec::new());
    }
    let (read_owner_kinds, read_owner_ids) = read_owner_columns(read_owners);
    let (world_kind, world_id) = proxima_core::access::world().columns();
    // Edge change-events MUST obey the same source-owned visibility as
    // `read_edges`: an edge is listed iff its source is readable AND the
    // public-source guard holds (NOT (source World-readable AND target not
    // readable)). Otherwise `proxima://events` would disclose a private edge
    // target that the redacted edge-read surface correctly hides. Endpoints
    // resolve fact-entity ids to their current head, exactly as in edges.rs.
    let rows = sqlx::query(
        r"SELECT ce.seq, ce.entity_personality_instance_id, ce.wake_chain_depth
             FROM proxima_core.change_event ce
             WHERE EXISTS (
                SELECT 1
                  FROM unnest($1::proxima_core.owner_principal_kind[], $2::uuid[]) AS s(kind, id)
                 WHERE ce.owner_principal_kind = s.kind
                   AND ce.owner_principal_id = s.id
             )
               AND ce.seq > $3
               AND (
                    ce.edge_id IS NULL
                    OR (
                        EXISTS (
                            SELECT 1
                              FROM proxima_core.entity_owner seo
                              JOIN unnest($1::proxima_core.owner_principal_kind[], $2::uuid[]) AS rs(kind, id)
                                ON seo.owner_principal_kind = rs.kind
                               AND seo.owner_principal_id = rs.id
                             WHERE seo.entity_id = COALESCE(
                                       ce.edge_source_memory_id, ce.edge_source_goal_id,
                                       (SELECT fe.current_memory_id FROM proxima_core.fact_entities fe
                                         WHERE fe.fact_entity_id = ce.edge_source_fact_entity_id))
                        )
                        AND NOT (
                            EXISTS (
                                SELECT 1
                                  FROM proxima_core.entity_owner weo
                                 WHERE weo.entity_id = COALESCE(
                                           ce.edge_source_memory_id, ce.edge_source_goal_id,
                                           (SELECT fe.current_memory_id FROM proxima_core.fact_entities fe
                                             WHERE fe.fact_entity_id = ce.edge_source_fact_entity_id))
                                   AND weo.owner_principal_kind = $5
                                   AND weo.owner_principal_id = $6
                            )
                            AND NOT EXISTS (
                                SELECT 1
                                  FROM proxima_core.entity_owner teo
                                  JOIN unnest($1::proxima_core.owner_principal_kind[], $2::uuid[]) AS rt(kind, id)
                                    ON teo.owner_principal_kind = rt.kind
                                   AND teo.owner_principal_id = rt.id
                                 WHERE teo.entity_id = COALESCE(
                                           ce.edge_target_memory_id, ce.edge_target_goal_id,
                                           (SELECT fe.current_memory_id FROM proxima_core.fact_entities fe
                                             WHERE fe.fact_entity_id = ce.edge_target_fact_entity_id))
                            )
                        )
                    )
               )
             ORDER BY ce.seq ASC
             LIMIT $4",
    )
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
        if let Some(event) = hydrate_change_event(pool, seq).await? {
            let personality_instance_id = r
                .try_get::<Option<uuid::Uuid>, _>("entity_personality_instance_id")
                .map_err(map_err)?;
            let wake_chain_depth = r.try_get::<i16, _>("wake_chain_depth").map_err(map_err)?;
            out.push(ChangeEventForWake {
                event,
                authoring_personality_instance_id: personality_instance_id
                    .filter(|id| !id.is_nil())
                    .map(PersonalityInstanceId::new),
                wake_chain_depth: WakeChainDepth::new(u16::try_from(wake_chain_depth).unwrap_or(0)),
            });
        }
    }
    Ok(out)
}

fn read_owner_columns(read_owners: &[Principal]) -> (Vec<OwnerPrincipalKind>, Vec<uuid::Uuid>) {
    let kinds = read_owners
        .iter()
        .map(|principal| principal.columns().0)
        .collect();
    let ids = read_owners
        .iter()
        .map(|principal| principal.columns().1)
        .collect();
    (kinds, ids)
}

pub async fn list_change_events_for_replay(
    pool: &PgPool,
    owner: &Owner,
    after: uuid::Uuid,
    until: Option<uuid::Uuid>,
    limit: usize,
) -> Result<Vec<ChangeEventForWake>, StorageError> {
    // Owner scope is the principal; see `list_change_events_after`.
    let (owner_kind, owner_principal_id) = owner.columns();
    let rows = sqlx::query!(
        r#"SELECT seq, entity_personality_instance_id, wake_chain_depth
             FROM proxima_core.change_event
             WHERE owner_principal_kind = $1
               AND owner_principal_id = $2
               AND seq > $3
               AND ($4::uuid IS NULL OR seq <= $4)
             ORDER BY seq ASC
             LIMIT $5"#,
        owner_kind as OwnerPrincipalKind,
        owner_principal_id,
        after,
        until,
        i64::try_from(limit).unwrap_or(i64::MAX),
    )
    .fetch_all(pool)
    .await
    .map_err(map_err)?;

    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        if let Some(event) = hydrate_change_event(pool, r.seq).await? {
            out.push(ChangeEventForWake {
                event,
                authoring_personality_instance_id: r
                    .entity_personality_instance_id
                    .filter(|id| !id.is_nil())
                    .map(PersonalityInstanceId::new),
                wake_chain_depth: WakeChainDepth::new(
                    u16::try_from(r.wake_chain_depth).unwrap_or(0),
                ),
            });
        }
    }
    Ok(out)
}
