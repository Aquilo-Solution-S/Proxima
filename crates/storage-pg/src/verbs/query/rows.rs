use proxima_core::verbs::goal_write::GoalState;
use proxima_core::verbs::query::{EdgeRow, EntityKind, GoalRow, MemoryRow, StatefulHeadsFilter};
use proxima_core::verbs::schema::SchemaInfo;
use proxima_core::{
    EntityRef, GoalId, GroupId, MemoryId, OrgId, Owner, Principal, SchemaId, SchemaVersion,
    StorageError, UserId,
};
use sqlx::PgPool;

use crate::pg_ident::PgIdent;

pub(super) fn memory_row_from_db(
    r: MemoryRowDb,
    schemas: &[SchemaInfo],
) -> Result<MemoryRow, StorageError> {
    let schema_version = u32::try_from(r.schema_version).map_err(|_| {
        StorageError::Internal(format!(
            "invalid memory schema_version {} for memory {}",
            r.schema_version, r.memory_id
        ))
    })?;

    let schema_id = SchemaId::new(r.schema_id);
    let schema_version = SchemaVersion::new(schema_version);
    let cbor_encoder = schemas
        .iter()
        .find(|s| s.schema_id == schema_id && s.schema_version == schema_version)
        .and_then(|s| s.cbor_encoder);

    Ok(MemoryRow {
        id: MemoryId::new(r.memory_id),
        kind: match r.kind.as_deref() {
            Some("Abstraction") => EntityKind::Abstraction,
            Some("Perspective") => EntityKind::Perspective,
            _ => EntityKind::Fact,
        },
        schema_id,
        schema_version,
        owner: Owner {
            principal: match r.owner_principal_kind.as_str() {
                "User" => Principal::User(UserId::new(r.owner_principal_id)),
                _ => Principal::Group(GroupId::new(r.owner_principal_id)),
            },
            org_id: OrgId::new(r.owner_org_id),
        },
        payload: r
            .payload_json
            .as_deref()
            .map(|text| json_text_to_cbor(text, cbor_encoder))
            .transpose()?
            .unwrap_or_default(),
    })
}

fn json_text_to_cbor(
    text: &str,
    encoder: Option<proxima_core::verbs::schema::PayloadCborEncoder>,
) -> Result<Vec<u8>, StorageError> {
    let value: serde_json::Value = serde_json::from_str(text)
        .map_err(|e| StorageError::Internal(format!("invalid payload JSON projection: {e}")))?;
    if let Some(encode) = encoder {
        return encode(&value)
            .map_err(|e| StorageError::Internal(format!("CBOR payload encode failed: {e}")));
    }
    let mut bytes = Vec::new();
    ciborium::ser::into_writer(&value, &mut bytes)
        .map_err(|e| StorageError::Internal(format!("CBOR payload encode failed: {e}")))?;
    Ok(bytes)
}

pub(super) fn goal_row_from_db(r: GoalRowDb) -> Result<GoalRow, StorageError> {
    let state = match r.state.as_str() {
        "Proposed" => GoalState::Proposed,
        "Active" => GoalState::Active,
        "Paused" => GoalState::Paused,
        "Achieved" => GoalState::Achieved,
        "Abandoned" => GoalState::Abandoned,
        "Rejected" => GoalState::Rejected,
        other => {
            return Err(StorageError::Internal(format!(
                "unknown goal state: {other}"
            )));
        }
    };
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
        owner: owner_from_parts(
            &r.owner_principal_kind,
            r.owner_principal_id,
            r.owner_org_id,
        ),
        title: r.title,
        text: r.text,
        state,
        parent_goal_ids: r.parent_goal_ids.into_iter().map(GoalId::new).collect(),
        supersedes: r.supersedes.map(GoalId::new),
        payload: r.payload,
    })
}

pub(super) fn edge_row_from_db(r: EdgeRowDb) -> Result<EdgeRow, StorageError> {
    let source = entity_ref_from_endpoint(r.source_memory_id, r.source_goal_id)?;
    let target = entity_ref_from_endpoint(r.target_memory_id, r.target_goal_id)?;
    Ok(EdgeRow {
        id: r.edge_id,
        relation: r.relation,
        relation_class: r.relation_class,
        source,
        target,
        owner: owner_from_parts(
            &r.owner_principal_kind,
            r.owner_principal_id,
            r.owner_org_id,
        ),
        payload: Vec::new(),
    })
}

fn entity_ref_from_endpoint(
    memory_id: Option<uuid::Uuid>,
    goal_id: Option<uuid::Uuid>,
) -> Result<EntityRef, StorageError> {
    match (memory_id, goal_id) {
        (Some(m), None) => Ok(EntityRef::Memory(MemoryId::new(m))),
        (None, Some(g)) => Ok(EntityRef::Goal(GoalId::new(g))),
        _ => Err(StorageError::Internal(
            "edge endpoint columns violate CHECK constraint".into(),
        )),
    }
}

fn owner_from_parts(kind: &str, principal_id: uuid::Uuid, org_id: uuid::Uuid) -> Owner {
    Owner {
        principal: match kind {
            "User" => Principal::User(UserId::new(principal_id)),
            _ => Principal::Group(GroupId::new(principal_id)),
        },
        org_id: OrgId::new(org_id),
    }
}

#[derive(Debug, sqlx::FromRow)]
pub(super) struct GoalRowDb {
    goal_id: uuid::Uuid,
    schema_id: String,
    schema_version: i32,
    owner_principal_kind: String,
    owner_principal_id: uuid::Uuid,
    owner_org_id: uuid::Uuid,
    title: String,
    text: String,
    state: String,
    supersedes: Option<uuid::Uuid>,
    payload: Vec<u8>,
    parent_goal_ids: Vec<uuid::Uuid>,
}

#[derive(Debug, sqlx::FromRow)]
pub(super) struct EdgeRowDb {
    edge_id: uuid::Uuid,
    relation: String,
    relation_class: String,
    source_memory_id: Option<uuid::Uuid>,
    source_goal_id: Option<uuid::Uuid>,
    target_memory_id: Option<uuid::Uuid>,
    target_goal_id: Option<uuid::Uuid>,
    owner_principal_kind: String,
    owner_principal_id: uuid::Uuid,
    owner_org_id: uuid::Uuid,
}

#[derive(Debug, sqlx::FromRow)]
#[allow(dead_code)]
pub(super) struct MemoryRowDb {
    memory_id: uuid::Uuid,
    owner_principal_kind: String,
    owner_principal_id: uuid::Uuid,
    owner_org_id: uuid::Uuid,
    schema_id: String,
    schema_version: i32,
    kind: Option<String>,
    event_id: Option<Vec<u8>>,
    payload_json: Option<String>,
}

pub(super) async fn read_seq_high_water(
    pool: &PgPool,
    owner_kind: &str,
    owner_principal_id: uuid::Uuid,
) -> Result<Option<uuid::Uuid>, StorageError> {
    let row: Option<(uuid::Uuid,)> = sqlx::query_as(
        "SELECT seq FROM proxima_core.change_event \
         WHERE owner_principal_kind = $1 AND owner_principal_id = $2 \
         ORDER BY seq DESC LIMIT 1",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| StorageError::Internal(e.to_string()))?;
    Ok(row.map(|(v,)| v))
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
    Ok(sf)
}
