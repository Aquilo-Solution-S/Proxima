//! Hydration of `change_event` rows into typed `ChangeEvent`s for the
//! pull-read verbs (`change_history`, `list_change_events_*`). The
//! LISTEN/NOTIFY outbox publisher was retired — `change_event` is a
//! pull-only durable log; consumers read it by `seq` cursor.

use proxima_core::{
    ChangeEvent, ChangeEventKind, ChangeEventKindTag, EdgeEndpoint, EdgeKind, EdgeTargetProjection,
    EntityKind, EntityRef, GoalId, MemoryId, OwnerRef, OwnerRefKind, SchemaId, SchemaVersion,
    StorageError,
};
use uuid::Uuid;

use crate::error::internal;
use crate::verbs::edge_index::{PgEndpointKind, endpoint_from_columns};

#[derive(Debug, sqlx::FromRow)]
struct ChangeEventRow {
    seq: Uuid,
    owner_kind: OwnerRefKind,
    owner_id: Option<Uuid>,
    kind: ChangeEventKindTag,
    entity_kind: Option<EntityKind>,
    entity_memory_id: Option<Uuid>,
    entity_goal_id: Option<Uuid>,
    entity_schema_id: Option<String>,
    entity_schema_version: Option<i32>,
    supersedes_memory_id: Option<Uuid>,
    supersedes_goal_id: Option<Uuid>,
    edge_kind: Option<EdgeKind>,
    edge_source_kind: Option<PgEndpointKind>,
    edge_source_id: Option<Uuid>,
    edge_target_kind: Option<PgEndpointKind>,
    edge_target_id: Option<Uuid>,
    entity_memory_present: bool,
    edge_target_available: bool,
    edge_target_visible: bool,
}

/// Build the column list and the three correlated subqueries that define what a
/// reader may see of a `change_event` row, shared by the single and batch
/// hydrators so the two cannot disagree about it.
///
/// `edge_target_visible` is the one that matters: it is what makes
/// [`decode_edge_event`] return [`EdgeTargetProjection::Redacted`] instead
/// of a real endpoint, so a second copy of it is a second place redaction
/// can be weakened. Both hydrators feed [`ChangeEventRow`], which is what
/// binds this list to the struct that reads it.
///
/// `$2`/`$3` are the read-owner kind/id arrays. `$1` is left to the caller:
/// it is the only part that differs between the two queries, being a `seq`
/// in one and a `seq` array in the other.
fn change_event_projection() -> String {
    format!(
        r"SELECT seq,
                  owner_kind,
                  owner_id,
                  kind,
                  entity_kind,
                  entity_memory_id, entity_goal_id,
                  entity_schema_id, entity_schema_version,
                  supersedes_memory_id, supersedes_goal_id,
                  edge_kind, edge_source_kind, edge_source_id,
                  edge_target_kind, edge_target_id,
                  (
                      entity_memory_id IS NULL
                      OR EXISTS (
                          SELECT 1
                            FROM proxima_core.memories m
                           WHERE m.memory_id = change_event.entity_memory_id
                             AND m.tombstoned_at IS NULL
                      )
                  ) AS entity_memory_present,
                  (
                      edge_kind IS NULL OR EXISTS (
                          SELECT 1
                            FROM (SELECT memory_id AS entity_id FROM proxima_core.memories UNION ALL SELECT goal_id AS entity_id FROM proxima_core.goals UNION ALL SELECT fact_entity_id AS entity_id FROM proxima_core.fact_entities) teo
                           WHERE teo.entity_id = edge_target_id
                      )
                  ) AS edge_target_available,
                  (
                      edge_kind IS NULL OR EXISTS (
                          SELECT 1
                            FROM {eo_union} teo
                            JOIN unnest($2::proxima_core.owner_ref_kind[], $3::uuid[]) AS rs(kind, id)
                              ON teo.owner_kind = rs.kind
                             AND teo.owner_id IS NOT DISTINCT FROM rs.id
                           WHERE teo.entity_id = COALESCE(
                               (SELECT fe.current_memory_id FROM proxima_core.fact_entities fe
                                 WHERE fe.fact_entity_id = edge_target_id),
                               edge_target_id)
                      )
                  ) AS edge_target_visible
             FROM proxima_core.change_event",
        eo_union = crate::verbs::query::entity_owner_union(),
    )
}

/// Hydrate a single `change_event` row into a typed `ChangeEvent`.
///
/// The migration guarantees exactly one of `(entity_memory_id,
/// entity_goal_id)` is non-NULL for `EntityAppend`, and same for
/// supersedes columns.
pub(crate) async fn hydrate_change_event(
    pool: &sqlx::PgPool,
    read_owners: &[OwnerRef],
    seq: Uuid,
) -> Result<Option<ChangeEvent>, StorageError> {
    let (read_owner_kinds, read_owner_ids) =
        crate::access::owner_columns::owner_arrays(read_owners);
    // SQL-POLICY: fixed-fragment — the only interpolation is this module's
    // own projection, itself built only from the shared entity-owner-union
    // constant; `seq` and both owner arrays are bound.
    let row = sqlx::query_as::<_, ChangeEventRow>(sqlx::AssertSqlSafe(format!(
        "{projection} WHERE seq = $1",
        projection = change_event_projection()
    )))
    .bind(seq)
    .bind(&read_owner_kinds)
    .bind(&read_owner_ids)
    .fetch_optional(pool)
    .await
    .map_err(internal)?;

    row.as_ref().map(decode_change_event_row).transpose()
}

/// Batched hydrate. Returns events ordered by `seq DESC`; the caller
/// is responsible for any further reordering.
pub(crate) async fn hydrate_change_events_batch(
    pool: &sqlx::PgPool,
    read_owners: &[OwnerRef],
    seqs: &[Uuid],
) -> Result<Vec<ChangeEvent>, StorageError> {
    if seqs.is_empty() {
        return Ok(Vec::new());
    }
    let (read_owner_kinds, read_owner_ids) =
        crate::access::owner_columns::owner_arrays(read_owners);
    // SQL-POLICY: fixed-fragment — the only interpolation is this module's
    // own projection, itself built only from the shared entity-owner-union
    // constant; `seqs` and both owner arrays are bound.
    let rows = sqlx::query_as::<_, ChangeEventRow>(sqlx::AssertSqlSafe(format!(
        "{projection} WHERE seq = ANY($1::uuid[]) ORDER BY seq DESC",
        projection = change_event_projection()
    )))
    .bind(seqs)
    .bind(&read_owner_kinds)
    .bind(&read_owner_ids)
    .fetch_all(pool)
    .await
    .map_err(internal)?;

    rows.iter().map(decode_change_event_row).collect()
}

fn decode_change_event_row(row: &ChangeEventRow) -> Result<ChangeEvent, StorageError> {
    let owner = row.owner_kind.with_uuid(row.owner_id).ok_or_else(|| {
        StorageError::Internal(format!(
            "invalid change_event owner_ref shape at seq {}",
            row.seq
        ))
    })?;

    let kind = match row.kind {
        ChangeEventKindTag::EdgeAppend => decode_edge_append(row)?,
        ChangeEventKindTag::EdgeDelete => decode_edge_delete(row)?,
        ChangeEventKindTag::EntityAppend => decode_entity_append_or_absent_delete(row)?,
        ChangeEventKindTag::EntityDelete => decode_entity_delete(row)?,
    };

    Ok(ChangeEvent {
        seq: row.seq,
        owner,
        kind,
    })
}

fn decode_edge_append(row: &ChangeEventRow) -> Result<ChangeEventKind, StorageError> {
    let edge = decode_edge_event(row)?;
    Ok(ChangeEventKind::EdgeAppend {
        source: edge.source,
        target: edge.target,
        kind: edge.kind,
    })
}

fn decode_edge_delete(row: &ChangeEventRow) -> Result<ChangeEventKind, StorageError> {
    let edge = decode_edge_event(row)?;
    Ok(ChangeEventKind::EdgeDelete {
        source: edge.source,
        target: edge.target,
        kind: edge.kind,
    })
}

#[derive(Debug)]
struct EdgeEvent {
    source: EdgeEndpoint,
    target: EdgeTargetProjection,
    kind: EdgeKind,
}

/// The row carries the whole edge, so the endpoint kinds come back with the
/// event instead of through a second lookup keyed by an id that no longer
/// exists.
fn decode_edge_event(row: &ChangeEventRow) -> Result<EdgeEvent, StorageError> {
    let kind = row
        .edge_kind
        .ok_or_else(|| StorageError::Internal("missing edge_kind".into()))?;
    let source = decode_edge_endpoint(row.edge_source_kind, row.edge_source_id, "source")?;
    let target = if !row.edge_target_available {
        EdgeTargetProjection::Unavailable
    } else if row.edge_target_visible {
        EdgeTargetProjection::visible(decode_edge_endpoint(
            row.edge_target_kind,
            row.edge_target_id,
            "target",
        )?)
    } else {
        EdgeTargetProjection::Redacted
    };
    Ok(EdgeEvent {
        source,
        target,
        kind,
    })
}

fn decode_edge_endpoint(
    kind: Option<PgEndpointKind>,
    id: Option<Uuid>,
    side: &str,
) -> Result<EdgeEndpoint, StorageError> {
    let (Some(kind), Some(id)) = (kind, id) else {
        return Err(StorageError::Internal(format!(
            "change_event edge {side} endpoint columns violate CHECK constraint"
        )));
    };
    Ok(endpoint_from_columns(kind, id))
}

fn decode_entity_append_or_absent_delete(
    row: &ChangeEventRow,
) -> Result<ChangeEventKind, StorageError> {
    let entity = decode_entity_event(row)?;
    if matches!(entity.entity, EntityRef::Memory(_)) && !row.entity_memory_present {
        return Ok(ChangeEventKind::EntityDelete {
            entity_kind: entity.kind,
            entity: entity.entity,
            schema_id: entity.schema_id,
            schema_version: entity.schema_version,
        });
    }
    let supersedes = match (row.supersedes_memory_id, row.supersedes_goal_id) {
        (Some(m), None) => Some(EntityRef::Memory(MemoryId::new(m))),
        (None, Some(g)) => Some(EntityRef::Goal(GoalId::new(g))),
        (None, None) => None,
        (Some(_), Some(_)) => {
            return Err(StorageError::Internal(
                "change_event supersedes columns violate CHECK constraint".into(),
            ));
        }
    };
    Ok(ChangeEventKind::EntityAppend {
        entity_kind: entity.kind,
        entity: entity.entity,
        schema_id: entity.schema_id,
        schema_version: entity.schema_version,
        supersedes,
    })
}

fn decode_entity_delete(row: &ChangeEventRow) -> Result<ChangeEventKind, StorageError> {
    let entity = decode_entity_event(row)?;
    Ok(ChangeEventKind::EntityDelete {
        entity_kind: entity.kind,
        entity: entity.entity,
        schema_id: entity.schema_id,
        schema_version: entity.schema_version,
    })
}

#[derive(Debug)]
struct EntityEvent {
    kind: EntityKind,
    entity: EntityRef,
    schema_id: SchemaId,
    schema_version: SchemaVersion,
}

fn decode_entity_event(row: &ChangeEventRow) -> Result<EntityEvent, StorageError> {
    let kind = row
        .entity_kind
        .ok_or_else(|| StorageError::Internal("missing entity_kind".into()))?;
    let entity = match (row.entity_memory_id, row.entity_goal_id) {
        (Some(m), None) => EntityRef::Memory(MemoryId::new(m)),
        (None, Some(g)) => EntityRef::Goal(GoalId::new(g)),
        (Some(_), Some(_)) | (None, None) => {
            return Err(StorageError::Internal(
                "change_event entity columns violate CHECK constraint".into(),
            ));
        }
    };
    let schema_id = SchemaId::new(
        row.entity_schema_id
            .clone()
            .ok_or_else(|| StorageError::Internal("missing entity_schema_id".into()))?,
    );
    let schema_version = SchemaVersion::new(
        row.entity_schema_version
            .ok_or_else(|| StorageError::Internal("missing entity_schema_version".into()))?
            .cast_unsigned(),
    );
    Ok(EntityEvent {
        kind,
        entity,
        schema_id,
        schema_version,
    })
}
