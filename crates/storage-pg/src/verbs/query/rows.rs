use proxima_core::change_event::EntityKind;
use proxima_core::verbs::goal_write::GoalState;
use proxima_core::verbs::query::{GoalRow, MemoryRow, StatefulHeadsFilter};
use proxima_core::{
    Edge, EdgeKind, EdgeTargetProjection, GoalId, MemoryId, Owner, OwnerRefKind, SchemaId,
    SchemaVersion, SidecarPayload, StorageError,
};
use sqlx::PgPool;

use crate::error::map_err;
use crate::pg_ident::PgIdent;
use crate::verbs::consolidate::edge_event_visibility_predicate;
use crate::verbs::edge_index::{PgEndpointKind, endpoint_from_columns};

use super::read_owner_equality_predicate;

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
        id: MemoryId::new(r.memory_id),
        kind: r.kind.unwrap_or(EntityKind::Fact),
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

fn owner_from_parts(
    kind: OwnerRefKind,
    owner_id: Option<uuid::Uuid>,
) -> Result<Owner, StorageError> {
    kind.with_uuid(owner_id)
        .ok_or_else(|| StorageError::Internal("invalid OwnerRef columns".into()))
}

#[derive(Debug, sqlx::FromRow)]
pub(super) struct GoalRowDb {
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
    pub(super) created_at: time::OffsetDateTime,
    owner_kind: OwnerRefKind,
    owner_id: Option<uuid::Uuid>,
    pub(super) schema_id: String,
    pub(super) schema_version: i32,
    pub(super) kind: Option<EntityKind>,
}

/// Cursor high-water over the requester's READ set — never a client-supplied
/// principal. Computed across `read_owners` so it spans exactly the change
/// events the requester may see (the same set `list_change_events_after`
/// filters by); using `req.owner` here would leak whether/when a foreign
/// owner has events.
pub(crate) async fn read_seq_high_water(
    pool: &PgPool,
    read_owner_kinds: &[OwnerRefKind],
    read_owner_ids: &[Option<uuid::Uuid>],
) -> Result<Option<uuid::Uuid>, StorageError> {
    let (world_kind, world_id) =
        crate::access::owner_columns::owner_binds(&proxima_core::access::world());
    let sql = read_seq_high_water_sql();
    // SQL-POLICY: fixed-fragment
    let row: Option<(uuid::Uuid,)> = sqlx::query_as(sqlx::AssertSqlSafe(sql))
        .bind(read_owner_kinds)
        .bind(read_owner_ids)
        .bind(world_kind)
        .bind(world_id)
        .fetch_optional(pool)
        .await
        .map_err(map_err)?;
    Ok(row.map(|(v,)| v))
}

/// The high-water statement. `$1`/`$2` are the read owner arrays;
/// `$3`/`$4` are the World owner columns the edge-event visibility probe
/// binds.
///
/// One top-1 probe of `idx_change_event_owner_seq` per read owner, merged
/// by a top-1 over the arms. A whole-table `ORDER BY seq DESC` walk through
/// an `EXISTS` over the read set has no index prefix. Same maximum: that
/// filter admits exactly the rows some read owner matches (`change_event`'s
/// CHECKs make `ce.owner_id` never NULL, so the World member matches
/// nothing either way), and the max over a union is the max of the
/// per-member maxima.
fn read_seq_high_water_sql() -> String {
    let edge_visibility = edge_event_visibility_predicate(1, 2, 3, 4);
    format!(
        r"SELECT hw.seq
         FROM unnest($1::proxima_core.owner_ref_kind[], $2::uuid[]) AS s(kind, id)
         JOIN LATERAL (
             SELECT ce.seq FROM proxima_core.change_event ce
              WHERE {read_owner_predicate}
                AND {edge_visibility}
              ORDER BY ce.seq DESC LIMIT 1
         ) hw ON TRUE
         ORDER BY hw.seq DESC LIMIT 1",
        read_owner_predicate = read_owner_equality_predicate("ce", "s"),
    )
}

/// Emit the exact high-water statement [`read_seq_high_water`] would run —
/// the golden-pin surface, compiled only for tests.
/// Same cfg gate as the search `*_sql_for_tests` exports.
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
