//! Hydration of `change_event` rows into typed `ChangeEvent`s for the
//! pull-read verbs (`event_history`, `list_change_events_*`). The
//! LISTEN/NOTIFY outbox publisher was retired — `change_event` is a
//! pull-only durable log; consumers read it by `seq` cursor.

use proxima_core::{
    ChangeEvent, ChangeEventKind, ChangeEventKindTag, EntityKind, EntityRef, GoalId, MemoryId,
    OrgId, Owner, OwnerPrincipalKind, SchemaId, SchemaVersion, StorageError,
};
use uuid::Uuid;

#[derive(Debug, sqlx::FromRow)]
struct ChangeEventRow {
    seq: Uuid,
    owner_principal_kind: OwnerPrincipalKind,
    owner_principal_id: Uuid,
    owner_org_id: Uuid,
    kind: ChangeEventKindTag,
    entity_kind: Option<EntityKind>,
    entity_memory_id: Option<Uuid>,
    entity_goal_id: Option<Uuid>,
    entity_schema_id: Option<String>,
    entity_schema_version: Option<i32>,
    supersedes_memory_id: Option<Uuid>,
    supersedes_goal_id: Option<Uuid>,
    edge_id: Option<Uuid>,
    edge_relation: Option<String>,
    edge_source_memory_id: Option<Uuid>,
    edge_source_goal_id: Option<Uuid>,
    edge_target_memory_id: Option<Uuid>,
    edge_target_goal_id: Option<Uuid>,
    entity_personality_instance_id: Option<Uuid>,
    wake_chain_depth: i16,
    entity_memory_present: bool,
}

/// Hydrate a single `change_event` row into a typed `ChangeEvent`.
///
/// The migration guarantees exactly one of `(entity_memory_id,
/// entity_goal_id)` is non-NULL for `EntityAppend`, and same for
/// supersedes columns.
pub(crate) async fn hydrate_change_event(
    pool: &sqlx::PgPool,
    seq: Uuid,
) -> Result<Option<ChangeEvent>, StorageError> {
    let row = sqlx::query_as::<_, ChangeEventRow>(
        r"SELECT seq,
                  owner_principal_kind,
                  owner_principal_id, owner_org_id,
                  kind,
                  entity_kind,
                  entity_memory_id, entity_goal_id,
                  entity_schema_id, entity_schema_version,
                  supersedes_memory_id, supersedes_goal_id,
                  edge_id, edge_relation,
                  edge_source_memory_id, edge_source_goal_id,
                  edge_target_memory_id, edge_target_goal_id,
                  entity_personality_instance_id,
                  wake_chain_depth,
                  (
                      entity_memory_id IS NULL
                      OR EXISTS (
                          SELECT 1
                            FROM proxima_core.memories m
                           WHERE m.memory_id = change_event.entity_memory_id
                             AND m.tombstoned_at IS NULL
                      )
                  ) AS entity_memory_present
             FROM proxima_core.change_event WHERE seq = $1",
    )
    .bind(seq)
    .fetch_optional(pool)
    .await
    .map_err(|e| StorageError::Internal(e.to_string()))?;

    row.as_ref().map(decode_change_event_row).transpose()
}

/// Batched hydrate. Returns events ordered by `seq DESC`; the caller
/// is responsible for any further reordering.
pub(crate) async fn hydrate_change_events_batch(
    pool: &sqlx::PgPool,
    seqs: &[Uuid],
) -> Result<Vec<ChangeEvent>, StorageError> {
    if seqs.is_empty() {
        return Ok(Vec::new());
    }
    let rows = sqlx::query_as::<_, ChangeEventRow>(
        r"SELECT seq,
                  owner_principal_kind,
                  owner_principal_id, owner_org_id,
                  kind,
                  entity_kind,
                  entity_memory_id, entity_goal_id,
                  entity_schema_id, entity_schema_version,
                  supersedes_memory_id, supersedes_goal_id,
                  edge_id, edge_relation,
                  edge_source_memory_id, edge_source_goal_id,
                  edge_target_memory_id, edge_target_goal_id,
                  entity_personality_instance_id,
                  wake_chain_depth,
                  (
                      entity_memory_id IS NULL
                      OR EXISTS (
                          SELECT 1
                            FROM proxima_core.memories m
                           WHERE m.memory_id = change_event.entity_memory_id
                             AND m.tombstoned_at IS NULL
                      )
                  ) AS entity_memory_present
             FROM proxima_core.change_event
             WHERE seq = ANY($1::uuid[]) ORDER BY seq DESC",
    )
    .bind(seqs)
    .fetch_all(pool)
    .await
    .map_err(|e| StorageError::Internal(e.to_string()))?;

    rows.iter().map(decode_change_event_row).collect()
}

fn decode_change_event_row(row: &ChangeEventRow) -> Result<ChangeEvent, StorageError> {
    let owner = Owner {
        principal: row.owner_principal_kind.with_uuid(row.owner_principal_id),
        org_id: OrgId::new(row.owner_org_id),
    };

    let authoring_instance = decode_personality(row.entity_personality_instance_id);
    let wake_chain_depth = u16::try_from(row.wake_chain_depth).unwrap_or(0);

    let kind = match row.kind {
        ChangeEventKindTag::EdgeAppend => decode_edge_append(row)?,
        ChangeEventKindTag::EntityAppend => decode_entity_append_or_absent_delete(row)?,
        ChangeEventKindTag::EntityDelete => decode_entity_delete(row)?,
    };

    Ok(ChangeEvent {
        seq: row.seq,
        owner,
        kind,
        authoring_personality_instance_id: authoring_instance,
        wake_chain_depth,
    })
}

fn decode_edge_append(row: &ChangeEventRow) -> Result<ChangeEventKind, StorageError> {
    let edge_id = row
        .edge_id
        .ok_or_else(|| StorageError::Internal("missing edge_id".into()))?;
    let relation = row
        .edge_relation
        .clone()
        .ok_or_else(|| StorageError::Internal("missing edge_relation".into()))?;
    let source = decode_entity_ref(row.edge_source_memory_id, row.edge_source_goal_id)?;
    let target = decode_entity_ref(row.edge_target_memory_id, row.edge_target_goal_id)?;
    Ok(ChangeEventKind::EdgeAppend {
        edge_id,
        relation,
        source,
        target,
    })
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

/// Map a row's optional personality instance to the public
/// `ChangeEvent` shape. Nil uuid marks external authoring.
fn decode_personality(instance_id: Option<Uuid>) -> Option<Uuid> {
    instance_id.filter(|id| !id.is_nil())
}

fn decode_entity_ref(
    memory_id: Option<Uuid>,
    goal_id: Option<Uuid>,
) -> Result<EntityRef, StorageError> {
    match (memory_id, goal_id) {
        (Some(m), None) => Ok(EntityRef::Memory(MemoryId::new(m))),
        (None, Some(g)) => Ok(EntityRef::Goal(GoalId::new(g))),
        (Some(_), Some(_)) | (None, None) => Err(StorageError::Internal(
            "change_event endpoint columns violate CHECK constraint".into(),
        )),
    }
}
