//! Hydration of `announce` rows into typed `ChangeEvent`s.

use proxima_core::{
    ChangeEvent, ChangeEventKind, EntityKind, EntityRef, GoalId, GroupId, MemoryId, OwnerRef,
    OwnerRefKind, SchemaId, SchemaVersion, StorageError, UserId,
};
use uuid::Uuid;

use crate::error::internal;
use crate::pg_enums::{PgAnnounceEntity, PgAnnounceOp, PgPinTargetKind};

#[derive(Debug, Clone, sqlx::FromRow)]
struct AnnounceRow {
    seq: Uuid,
    owner_id: Uuid,
    owner_kind: OwnerRefKind,
    op: PgAnnounceOp,
    entity: PgAnnounceEntity,
    handle: Uuid,
    t: Uuid,
    memory_kind: Option<PgPinTargetKind>,
    schema_id: Option<String>,
}

// Recover the immutable memory kind from retained lifecycle witnesses without
// changing the recorded event owner. Only the kind uses these witnesses.
const ANNOUNCE_BY_SEQ_SQL: &str = "
SELECT a.seq,
       a.owner_id,
       o.kind AS owner_kind,
       a.op AS op,
       a.entity AS entity,
       a.handle,
       a.t,
       COALESCE(m.kind::text::proxima_core.pin_target_kind,
                c.kind::text::proxima_core.pin_target_kind,
                e.kind) AS memory_kind,
       COALESCE(m.schema_id, gh.schema_id) AS schema_id
  FROM proxima_core.announce a
  JOIN proxima_core.owners o ON o.owner_id = a.owner_id
  LEFT JOIN proxima_core.memory m ON m.t = a.t
  LEFT JOIN proxima_core.cooled c ON c.t = a.t
  LEFT JOIN proxima_core.erased_pin_target e ON e.t = a.t
  LEFT JOIN proxima_core.goal_head gh ON gh.handle = a.handle AND a.entity = 'goal'
 WHERE a.seq = $1 AND a.owner_id = ANY($2::uuid[])
";

const ANNOUNCE_BY_SEQS_SQL: &str = "
SELECT a.seq,
       a.owner_id,
       o.kind AS owner_kind,
       a.op AS op,
       a.entity AS entity,
       a.handle,
       a.t,
       COALESCE(m.kind::text::proxima_core.pin_target_kind,
                c.kind::text::proxima_core.pin_target_kind,
                e.kind) AS memory_kind,
       COALESCE(m.schema_id, gh.schema_id) AS schema_id
  FROM proxima_core.announce a
  JOIN proxima_core.owners o ON o.owner_id = a.owner_id
  LEFT JOIN proxima_core.memory m ON m.t = a.t
  LEFT JOIN proxima_core.cooled c ON c.t = a.t
  LEFT JOIN proxima_core.erased_pin_target e ON e.t = a.t
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
    Ok(row.map(decode_announce_row))
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
    Ok(rows.into_iter().map(decode_announce_row).collect())
}

/// Infallible by construction: every closed vocabulary on the row is decoded
/// as its Postgres enum, so an unrecognised label fails at the sqlx boundary
/// where the row is read, not in a catch-all arm here that has to invent a
/// meaning for it.
fn decode_announce_row(row: AnnounceRow) -> ChangeEvent {
    let owner = match row.owner_kind {
        OwnerRefKind::Personal => OwnerRef::Personal(UserId::new(row.owner_id)),
        OwnerRefKind::Group => OwnerRef::Group(GroupId::new(row.owner_id)),
    };
    let entity_kind = match (row.entity, row.memory_kind) {
        (PgAnnounceEntity::Goal, _) => EntityKind::Goal,
        (PgAnnounceEntity::Memory, Some(kind)) => kind.into(),
        // No surviving kind witness: `memory`, `cooled` and `erased_pin_target`
        // all missed, so the row was hard-deleted under abandonment. The event
        // still has to name a kind; `Fact` is the floor of the F/A/P layering
        // and the only one that claims no derivation. Stated here rather than
        // reached through a catch-all, because it IS a guess.
        (PgAnnounceEntity::Memory, None) => EntityKind::Fact,
    };
    let entity = match row.entity {
        PgAnnounceEntity::Goal => EntityRef::Goal(GoalId::new(row.handle)),
        PgAnnounceEntity::Memory => EntityRef::Memory(MemoryId::new(row.t)),
    };
    let schema_id = SchemaId::new(row.schema_id.unwrap_or_default());
    let schema_version = SchemaVersion::new(1);
    // Exhaustive on purpose. A fifth `announce_op` must not be able to fall
    // through to `EntityAppend` — that would announce a deletion as a write to
    // every consumer of the pull-only change log.
    let kind = match row.op {
        PgAnnounceOp::Forget | PgAnnounceOp::Erase => ChangeEventKind::EntityDelete {
            entity_kind,
            entity,
            schema_id,
            schema_version,
        },
        PgAnnounceOp::Transfer => ChangeEventKind::EntityTransfer {
            entity_kind,
            entity,
            schema_id,
            schema_version,
        },
        PgAnnounceOp::Append => ChangeEventKind::EntityAppend {
            entity_kind,
            entity,
            schema_id,
            schema_version,
        },
    };
    ChangeEvent {
        seq: row.seq,
        owner,
        kind,
    }
}
