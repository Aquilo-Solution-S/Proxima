use proxima_core::verbs::goal_write::GoalState;
use proxima_core::verbs::query::{GoalRow, MemoryRow, StatefulHeadsFilter};
use proxima_core::{
    Edge, EdgeKind, EdgeTargetProjection, GoalId, MemoryId, Owner, OwnerRefKind, SchemaId,
    SchemaVersion, SidecarPayload, StorageError,
};
use sqlx::PgPool;

use crate::error::map_err;
use crate::pg_ident::PgIdent;
use crate::verbs::edge_index::{PgEndpointKind, endpoint_from_columns};

pub(super) fn memory_row_from_db(
    r: MemoryRowDb,
    payload: Option<SidecarPayload>,
) -> Result<MemoryRow, StorageError> {
    let schema_version = u32::try_from(r.schema_version).map_err(|_| {
        StorageError::Internal(format!(
            "invalid memory schema_version {} for memory {}",
            r.schema_version, r.memory_id
        ))
    })?;

    let schema_id = SchemaId::new(r.schema_id);
    let schema_version = SchemaVersion::new(schema_version);

    Ok(MemoryRow {
        handle: r.handle,
        id: MemoryId::new(r.memory_id),
        kind: parse_memory_kind(&r.kind)?,
        schema_id,
        schema_version,
        owner: owner_from_parts(r.owner_kind, r.owner_id)?,
        payload,
    })
}

pub(super) fn goal_row_from_db(r: GoalRowDb) -> Result<GoalRow, StorageError> {
    let state = r.state;
    let schema_version = u32::try_from(r.schema_version).map_err(|_| {
        StorageError::Internal(format!(
            "invalid goal schema_version {} for goal {}",
            r.schema_version, r.goal_id
        ))
    })?;
    Ok(GoalRow {
        handle: r.handle,
        id: GoalId::new(r.goal_id),
        schema_id: SchemaId::new(r.schema_id),
        schema_version: SchemaVersion::new(schema_version),
        owner: owner_from_parts(r.owner_kind, r.owner_id)?,
        title: r.title,
        text: r.text,
        state,
        dependency_goal_ids: r.dependency_goal_ids.into_iter().map(GoalId::new).collect(),
        supersedes: r.supersedes.map(GoalId::new),
        payload: r.payload,
    })
}

/// Project one stored edge for one reader.
///
/// Four fields is the whole model, so there is nothing to hydrate and nothing
/// that can fail: no id to dereference, no payload to join, no status. The
/// only decision is which of the three target projections the reader gets,
/// and a withheld target discloses neither id nor kind.
pub(super) fn edge_from_db(r: &EdgeRowDb) -> Edge {
    Edge {
        source: endpoint_from_columns(r.source_kind, r.source_id),
        target: if r.target_unavailable {
            EdgeTargetProjection::Unavailable
        } else if r.target_visible {
            EdgeTargetProjection::visible(endpoint_from_columns(r.target_kind, r.target_id))
        } else {
            EdgeTargetProjection::Redacted
        },
        kind: r.kind,
        created_at: r.created_at,
    }
}

fn parse_memory_kind(kind: &str) -> Result<proxima_core::change_event::EntityKind, StorageError> {
    match kind {
        "fact" | "Fact" => Ok(proxima_core::change_event::EntityKind::Fact),
        "abstraction" | "Abstraction" => Ok(proxima_core::change_event::EntityKind::Abstraction),
        "perspective" | "Perspective" => Ok(proxima_core::change_event::EntityKind::Perspective),
        other => Err(StorageError::Internal(format!(
            "invalid memory kind {other}"
        ))),
    }
}

fn owner_from_parts(
    kind: OwnerRefKind,
    owner_id: Option<uuid::Uuid>,
) -> Result<Owner, StorageError> {
    match kind {
        OwnerRefKind::World => Ok(proxima_core::OwnerRef::World),
        OwnerRefKind::Personal => owner_id
            .map(|id| proxima_core::OwnerRef::Personal(proxima_core::UserId::new(id)))
            .ok_or_else(|| StorageError::Internal("personal owner_id missing".into())),
        OwnerRefKind::Group => owner_id
            .map(|id| proxima_core::OwnerRef::Group(proxima_core::GroupId::new(id)))
            .ok_or_else(|| StorageError::Internal("group owner_id missing".into())),
    }
}

#[derive(Debug, sqlx::FromRow)]
pub(super) struct GoalRowDb {
    pub(super) handle: uuid::Uuid,
    pub(super) goal_id: uuid::Uuid,
    pub(super) created_at: time::OffsetDateTime,
    schema_id: String,
    schema_version: i32,
    owner_kind: OwnerRefKind,
    owner_id: Option<uuid::Uuid>,
    title: String,
    text: String,
    state: GoalState,
    supersedes: Option<uuid::Uuid>,
    payload: Vec<u8>,
    dependency_goal_ids: Vec<uuid::Uuid>,
}

#[derive(Debug, sqlx::FromRow)]
pub(super) struct EdgeRowDb {
    pub(super) source_kind: PgEndpointKind,
    pub(super) source_id: uuid::Uuid,
    pub(super) target_kind: PgEndpointKind,
    pub(super) target_id: uuid::Uuid,
    pub(super) kind: EdgeKind,
    pub(super) created_at: time::OffsetDateTime,
    pub(super) target_visible: bool,
    pub(super) target_unavailable: bool,
}

#[derive(Debug, sqlx::FromRow)]
pub(super) struct MemoryRowDb {
    pub(super) memory_id: uuid::Uuid,
    pub(super) handle: uuid::Uuid,
    pub(super) created_at: time::OffsetDateTime,
    owner_kind: OwnerRefKind,
    owner_id: Option<uuid::Uuid>,
    pub(super) schema_id: String,
    pub(super) schema_version: i32,
    pub(super) kind: String,
}

/// Cursor high-water over the requester's READ set — never a client-supplied
/// principal. Computed across `read_owners` so it spans exactly the change
/// events the requester may see (the same set `list_change_events_after`
/// filters by); using `req.owner` here would leak whether/when a foreign
/// owner has events.
pub(crate) async fn read_seq_high_water(
    pool: &PgPool,
    owner_ids: &[uuid::Uuid],
) -> Result<Option<uuid::Uuid>, StorageError> {
    let sql = read_seq_high_water_sql();
    // SQL-POLICY: fixed-fragment
    let row: Option<(uuid::Uuid,)> = sqlx::query_as(sqlx::AssertSqlSafe(sql))
        .bind(owner_ids)
        .fetch_optional(pool)
        .await
        .map_err(map_err)?;
    Ok(row.map(|(v,)| v))
}

fn read_seq_high_water_sql() -> String {
    "SELECT seq FROM proxima_core.announce \
     WHERE owner_id = ANY($1::uuid[]) \
     ORDER BY seq DESC LIMIT 1"
        .into()
}

/// Emit the exact high-water statement [`read_seq_high_water`] would run.
#[cfg(any(test, feature = "test-fixtures", debug_assertions))]
#[doc(hidden)]
#[must_use]
pub fn read_seq_high_water_sql_for_tests() -> String {
    read_seq_high_water_sql()
}

/// Validate identifiers from `StatefulHeadsFilter` before splicing them
/// into SQL. The values come from build-time-registered schemas
/// (`FactPayload::sidecar_table`, `FactPayload::natural_key_columns`)
/// which are `&'static str` constants — author-controlled, not
/// caller-controlled. This is a defense-in-depth check that catches
/// typos and rejects anything that doesn't look like a postgres
/// identifier.
#[allow(dead_code)]
pub(super) fn validate_stateful_filter(
    sf: &StatefulHeadsFilter,
) -> Result<&StatefulHeadsFilter, StorageError> {
    PgIdent::table(&sf.sidecar_table)?;
    if sf.natural_key_columns.is_empty() {
        return Err(StorageError::Internal(
            "stateful_heads with empty natural_key_columns".into(),
        ));
    }
    for col in &sf.natural_key_columns {
        PgIdent::column(col)?;
    }
    if let Some(tombstone) = &sf.tombstone {
        PgIdent::column(&tombstone.column)?;
    }
    Ok(sf)
}
