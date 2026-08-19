//! Hydration of `announce` rows into typed `ChangeEvent`s.

use proxima_core::{
    ChangeEvent, ChangeEventKind, EntityKind, EntityRef, GoalId, GroupId, MemoryId, OwnerRef,
    SchemaId, SchemaVersion, StorageError, UserId,
};
use uuid::Uuid;

use crate::error::internal;

#[derive(Debug, Clone, sqlx::FromRow)]
struct AnnounceRow {
    seq: Uuid,
    owner_id: Uuid,
    owner_kind: String,
    op: String,
    entity: String,
    handle: Uuid,
    t: Uuid,
    memory_kind: Option<String>,
    schema_id: Option<String>,
}

const ANNOUNCE_BY_SEQ_SQL: &str = "
SELECT a.seq,
       a.owner_id,
       o.kind::text AS owner_kind,
       a.op::text AS op,
       a.entity::text AS entity,
       a.handle,
       a.t,
       m.kind::text AS memory_kind,
       COALESCE(m.schema_id, gh.schema_id) AS schema_id
  FROM proxima_core.announce a
  JOIN proxima_core.owners o ON o.owner_id = a.owner_id
  LEFT JOIN proxima_core.memory m ON m.t = a.t
  LEFT JOIN proxima_core.goal_head gh ON gh.handle = a.handle AND a.entity = 'goal'
 WHERE a.seq = $1 AND a.owner_id = ANY($2::uuid[])
";

const ANNOUNCE_BY_SEQS_SQL: &str = "
SELECT a.seq,
       a.owner_id,
       o.kind::text AS owner_kind,
       a.op::text AS op,
       a.entity::text AS entity,
       a.handle,
       a.t,
       m.kind::text AS memory_kind,
       COALESCE(m.schema_id, gh.schema_id) AS schema_id
  FROM proxima_core.announce a
  JOIN proxima_core.owners o ON o.owner_id = a.owner_id
  LEFT JOIN proxima_core.memory m ON m.t = a.t
  LEFT JOIN proxima_core.goal_head gh ON gh.handle = a.handle AND a.entity = 'goal'
 WHERE a.seq = ANY($1::uuid[]) AND a.owner_id = ANY($2::uuid[])
 ORDER BY a.seq DESC
";

pub(crate) async fn hydrate_change_event(
    pool: &sqlx::PgPool,
    read_owners: &[OwnerRef],
    seq: Uuid,
) -> Result<Option<ChangeEvent>, StorageError> {
    let owner_ids: Vec<Uuid> = read_owners
        .iter()
        .copied()
        .map(OwnerRef::stored_owner_id)
        .collect();
    let row = sqlx::query_as::<_, AnnounceRow>(ANNOUNCE_BY_SEQ_SQL)
        .bind(seq)
        .bind(&owner_ids)
        .fetch_optional(pool)
        .await
        .map_err(internal)?;
    row.map(decode_announce_row).transpose()
}

pub(crate) async fn hydrate_change_events_batch(
    pool: &sqlx::PgPool,
    read_owners: &[OwnerRef],
    seqs: &[Uuid],
) -> Result<Vec<ChangeEvent>, StorageError> {
    if seqs.is_empty() {
        return Ok(Vec::new());
    }
    let owner_ids: Vec<Uuid> = read_owners
        .iter()
        .copied()
        .map(OwnerRef::stored_owner_id)
        .collect();
    let rows = sqlx::query_as::<_, AnnounceRow>(ANNOUNCE_BY_SEQS_SQL)
        .bind(seqs)
        .bind(&owner_ids)
        .fetch_all(pool)
        .await
        .map_err(internal)?;
    rows.into_iter().map(decode_announce_row).collect()
}

fn decode_announce_row(row: AnnounceRow) -> Result<ChangeEvent, StorageError> {
    let owner = match row.owner_kind.as_str() {
        "world" => OwnerRef::World,
        "personal" => OwnerRef::Personal(UserId::new(row.owner_id)),
        "group" => OwnerRef::Group(GroupId::new(row.owner_id)),
        other => {
            return Err(StorageError::Internal(format!(
                "unknown owner kind {other} at seq {}",
                row.seq
            )));
        }
    };
    let entity_kind = match row.entity.as_str() {
        "goal" => EntityKind::Goal,
        _ => match row.memory_kind.as_deref() {
            Some("abstraction") => EntityKind::Abstraction,
            Some("perspective") => EntityKind::Perspective,
            _ => EntityKind::Fact,
        },
    };
    let entity = match row.entity.as_str() {
        "goal" => EntityRef::Goal(GoalId::new(row.handle)),
        _ => EntityRef::Memory(MemoryId::new(row.t)),
    };
    let schema_id = SchemaId::new(row.schema_id.unwrap_or_default());
    let schema_version = SchemaVersion::new(1);
    let kind = match row.op.as_str() {
        "forget" | "erase" => ChangeEventKind::EntityDelete {
            entity_kind,
            entity,
            schema_id,
            schema_version,
        },
        "transfer" => ChangeEventKind::EntityTransfer {
            entity_kind,
            entity,
            schema_id,
            schema_version,
        },
        _ => ChangeEventKind::EntityAppend {
            entity_kind,
            entity,
            schema_id,
            schema_version,
        },
    };
    Ok(ChangeEvent {
        seq: row.seq,
        owner,
        kind,
    })
}
