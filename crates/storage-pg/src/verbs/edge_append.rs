#![allow(clippy::doc_markdown)]
//! Atomic edge + typed sidecar write.
//!
//! `append_edge_in_tx` inserts one `proxima_core.edges` row plus an
//! optional typed sidecar row (keyed on `edge_id`) plus the
//! `EdgeAppend` change_event row, all in a single transaction.
//!
//! Used by typed F-layer edges (e.g. `proxima-code/calls`).

use proxima_core::verbs::schema::PayloadKind;
use proxima_core::{
    CapabilityTag, EdgeAuthorshipKind, EdgeId, EndpointBinding, EntityKind, Owner, OwnerRefKind,
    RegisteredRelation, RelationOwnerPolicy, SchemaId, SchemaVersion, StorageError,
    validate_operator_edge_shape,
};

use crate::error::map_err;
use crate::sidecars::PgSidecarFuture;

/// Namespace for edge ids minted from the edge's own content. Random
/// once, then fixed forever: changing it would make existing edges
/// unrecognisable and re-runs would duplicate them.
const CONTENT_EDGE_NAMESPACE: uuid::Uuid =
    uuid::Uuid::from_u128(0x89a4_7127_5a52_4f7d_8519_1506_d1a9_875d);

/// An edge id derived from the edge itself, making the write idempotent.
///
/// Edge ids are otherwise `now_v7()` and `proxima_core.edges` has no
/// unique constraint over (source, target, relation), so
/// `ON CONFLICT (edge_id) DO NOTHING` never fires and writing "the same"
/// edge twice writes two rows.
pub(crate) fn content_addressed_edge_id(
    owner: Owner,
    relation: &str,
    source_memory_id: uuid::Uuid,
    target_memory_id: uuid::Uuid,
    authorship_kind: EdgeAuthorshipKind,
) -> uuid::Uuid {
    // 0x00 separators: without them ("ab","c") and ("a","bc") hash alike,
    // and relation names are caller-supplied strings.
    let mut name = Vec::new();
    name.extend_from_slice(owner.stable_key_uuid().as_bytes());
    name.push(0);
    name.extend_from_slice(relation.as_bytes());
    name.push(0);
    name.extend_from_slice(source_memory_id.as_bytes());
    name.push(0);
    name.extend_from_slice(target_memory_id.as_bytes());
    name.push(0);
    name.extend_from_slice(format!("{authorship_kind:?}").as_bytes());
    uuid::Uuid::new_v5(&CONTENT_EDGE_NAMESPACE, &name)
}

/// Draft of an edge to be written.
#[derive(Debug, Clone)]
pub(crate) struct EdgeDraft<'a> {
    pub(crate) edge_id: uuid::Uuid,
    pub(crate) relation: RegisteredRelation<'a>,
    pub(crate) source_kind: EntityKind,
    pub(crate) source_memory_id: Option<uuid::Uuid>,
    pub(crate) source_goal_id: Option<uuid::Uuid>,
    pub(crate) source_fact_entity_id: Option<uuid::Uuid>,
    pub(crate) target_kind: EntityKind,
    pub(crate) target_memory_id: Option<uuid::Uuid>,
    pub(crate) target_goal_id: Option<uuid::Uuid>,
    pub(crate) target_fact_entity_id: Option<uuid::Uuid>,
    pub(crate) authorship_kind: EdgeAuthorshipKind,
    pub(crate) authorship_owner_memory_id: Option<uuid::Uuid>,
    pub(crate) owner: &'a Owner,
}

/// Write a substrate edge row + the EdgeAppend change_event in one
/// transaction.
///
/// # Errors
///
/// Returns `ConstraintViolation` on FK / check failures; `Internal`
/// on sqlx failure.
pub(crate) async fn append_edge_in_tx(
    tx: &mut sqlx::PgConnection,
    draft: &EdgeDraft<'_>,
) -> Result<(), StorageError> {
    validate_edge_draft(draft, false)?;
    validate_endpoint_required_tags(tx, draft).await?;
    validate_descriptor_owner_policy(tx, draft).await?;

    if !insert_edge_row(tx, draft).await? {
        return Ok(());
    }

    append_edge_change_event(tx, draft).await
}

/// Write a typed edge row + sidecar + the EdgeAppend change_event in one
/// transaction.
///
/// # Errors
///
/// Returns `ConstraintViolation` on shape failures; `Internal` on sqlx or
/// concrete sidecar insertion failure.
pub(crate) async fn append_edge_with_sidecar_in_tx(
    tx: &mut sqlx::PgConnection,
    draft: &EdgeDraft<'_>,
    sidecar: impl for<'t> FnOnce(&'t mut sqlx::PgConnection, EdgeId) -> PgSidecarFuture<'t>,
) -> Result<(), StorageError> {
    validate_edge_draft(draft, true)?;
    validate_endpoint_required_tags(tx, draft).await?;
    validate_descriptor_owner_policy(tx, draft).await?;

    if !insert_edge_row(tx, draft).await? {
        return Ok(());
    }

    sidecar(tx, EdgeId::new(draft.edge_id)).await?;
    append_edge_change_event(tx, draft).await
}

fn validate_edge_draft(draft: &EdgeDraft<'_>, payload_present: bool) -> Result<(), StorageError> {
    let descriptor = draft.relation.descriptor;
    let sidecar_table = draft.relation.payload_sidecar_table;
    match (sidecar_table.is_some(), payload_present) {
        (true, true) | (false, false) => {}
        (true, false) => {
            return Err(StorageError::ConstraintViolation(format!(
                "missing EdgePayload for typed relation {}",
                descriptor.relation
            )));
        }
        (false, true) => {
            return Err(StorageError::ConstraintViolation(format!(
                "payload supplied for substrate relation {}",
                descriptor.relation
            )));
        }
    }
    if !exactly_one_endpoint(
        draft.source_memory_id,
        draft.source_goal_id,
        draft.source_fact_entity_id,
    ) {
        return Err(StorageError::ConstraintViolation(
            "source endpoint columns violate exactly-one invariant".into(),
        ));
    }
    if !exactly_one_endpoint(
        draft.target_memory_id,
        draft.target_goal_id,
        draft.target_fact_entity_id,
    ) {
        return Err(StorageError::ConstraintViolation(
            "target endpoint columns violate exactly-one invariant".into(),
        ));
    }
    descriptor
        .validate_edge_shape(
            draft.source_kind.as_str(),
            endpoint_binding(
                draft.source_memory_id,
                draft.source_goal_id,
                draft.source_fact_entity_id,
            ),
            draft.target_kind.as_str(),
            endpoint_binding(
                draft.target_memory_id,
                draft.target_goal_id,
                draft.target_fact_entity_id,
            ),
            draft.authorship_kind.as_str(),
        )
        .map_err(StorageError::ConstraintViolation)?;
    validate_operator_edge_shape(
        descriptor.class,
        draft.source_kind,
        draft.target_kind,
        draft.authorship_kind,
    )
    .map_err(StorageError::ConstraintViolation)?;
    Ok(())
}

async fn validate_descriptor_owner_policy(
    tx: &mut sqlx::PgConnection,
    draft: &EdgeDraft<'_>,
) -> Result<(), StorageError> {
    if draft.relation.descriptor.owner_policy == RelationOwnerPolicy::SourceOwned {
        return Ok(());
    }
    let source = endpoint_owner(
        tx,
        draft.source_memory_id,
        draft.source_goal_id,
        draft.source_fact_entity_id,
    )
    .await?;
    let target = endpoint_owner(
        tx,
        draft.target_memory_id,
        draft.target_goal_id,
        draft.target_fact_entity_id,
    )
    .await?;
    if source == target {
        Ok(())
    } else {
        Err(StorageError::ConstraintViolation(format!(
            "relation {} requires source and target to have the same owner",
            draft.relation.descriptor.relation
        )))
    }
}

async fn endpoint_owner(
    tx: &mut sqlx::PgConnection,
    memory_id: Option<uuid::Uuid>,
    goal_id: Option<uuid::Uuid>,
    fact_entity_id: Option<uuid::Uuid>,
) -> Result<Owner, StorageError> {
    let row: Option<(OwnerRefKind, Option<uuid::Uuid>)> = match (memory_id, goal_id, fact_entity_id)
    {
        (Some(id), None, None | Some(_)) => sqlx::query_as(
            "SELECT owner_kind, owner_id FROM proxima_core.memories WHERE memory_id = $1",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_err)?,
        (None, Some(id), None) => {
            sqlx::query_as("SELECT owner_kind, owner_id FROM proxima_core.goals WHERE goal_id = $1")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(map_err)?
        }
        (None, None, Some(id)) => sqlx::query_as(
            "SELECT owner_kind, owner_id FROM proxima_core.fact_entities WHERE fact_entity_id = $1",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_err)?,
        _ => None,
    };
    let (kind, id) = row.ok_or(StorageError::NotFound)?;
    kind.with_uuid(id)
        .ok_or_else(|| StorageError::Internal("invalid OwnerRef columns".into()))
}

async fn validate_endpoint_required_tags(
    tx: &mut sqlx::PgConnection,
    draft: &EdgeDraft<'_>,
) -> Result<(), StorageError> {
    let descriptor = draft.relation.descriptor;
    if descriptor.source_required_tags.is_empty() && descriptor.target_required_tags.is_empty() {
        return Ok(());
    }
    if !descriptor.source_required_tags.is_empty() {
        validate_endpoint_side_required_tags(
            tx,
            draft,
            EndpointSide::Source,
            &descriptor.source_required_tags,
        )
        .await?;
    }
    if !descriptor.target_required_tags.is_empty() {
        validate_endpoint_side_required_tags(
            tx,
            draft,
            EndpointSide::Target,
            &descriptor.target_required_tags,
        )
        .await?;
    }
    Ok(())
}

async fn validate_endpoint_side_required_tags(
    tx: &mut sqlx::PgConnection,
    draft: &EdgeDraft<'_>,
    side: EndpointSide,
    required_tags: &std::collections::BTreeSet<CapabilityTag>,
) -> Result<(), StorageError> {
    let endpoint = resolve_endpoint_schema(tx, draft, side).await?;
    let declared = draft.relation.registry().schema_capability_tags(
        &endpoint.schema_id,
        endpoint.schema_version,
        endpoint.kind,
    );
    let missing = required_tags
        .iter()
        .filter(|tag| declared.is_none_or(|tags| !tags.contains(*tag)))
        .map(CapabilityTag::as_str)
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return Ok(());
    }
    Err(StorageError::ConstraintViolation(format!(
        "edge: endpoint missing required capability tag(s) on {}: {}",
        side.as_str(),
        missing.join(", "),
    )))
}

#[derive(Clone, Copy)]
enum EndpointSide {
    Source,
    Target,
}

impl EndpointSide {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Source => "source",
            Self::Target => "target",
        }
    }
}

struct EndpointSchema {
    schema_id: SchemaId,
    schema_version: SchemaVersion,
    kind: PayloadKind,
}

async fn resolve_endpoint_schema(
    tx: &mut sqlx::PgConnection,
    draft: &EdgeDraft<'_>,
    side: EndpointSide,
) -> Result<EndpointSchema, StorageError> {
    match endpoint_columns(draft, side) {
        (Some(memory_id), None, None) => {
            let row: Option<(String, i32, Option<EntityKind>)> = sqlx::query_as(
                "SELECT schema_id, schema_version, kind
                   FROM proxima_core.memories
                  WHERE memory_id = $1",
            )
            .bind(memory_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(map_err)?;
            let (schema_id, schema_version, kind) = row.ok_or_else(|| {
                StorageError::ConstraintViolation(format!(
                    "edge: {} endpoint not found while checking capability tags",
                    side.as_str(),
                ))
            })?;
            Ok(EndpointSchema {
                schema_id: SchemaId::new(schema_id),
                schema_version: schema_version_from_i32(schema_version)?,
                kind: payload_kind_for_entity(kind.unwrap_or(EntityKind::Fact)),
            })
        }
        (None, Some(goal_id), None) => {
            let row: Option<(String, i32)> = sqlx::query_as(
                "SELECT schema_id, schema_version
                   FROM proxima_core.goals
                  WHERE goal_id = $1",
            )
            .bind(goal_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(map_err)?;
            let (schema_id, schema_version) = row.ok_or_else(|| {
                StorageError::ConstraintViolation(format!(
                    "edge: {} endpoint not found while checking capability tags",
                    side.as_str(),
                ))
            })?;
            Ok(EndpointSchema {
                schema_id: SchemaId::new(schema_id),
                schema_version: schema_version_from_i32(schema_version)?,
                kind: PayloadKind::Goal,
            })
        }
        (None, None, Some(fact_entity_id)) => {
            let row: Option<(String, i32)> = sqlx::query_as(
                "SELECT schema_id, schema_version
                   FROM proxima_core.fact_entities
                  WHERE fact_entity_id = $1",
            )
            .bind(fact_entity_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(map_err)?;
            let (schema_id, schema_version) = row.ok_or_else(|| {
                StorageError::ConstraintViolation(format!(
                    "edge: {} endpoint not found while checking capability tags",
                    side.as_str(),
                ))
            })?;
            Ok(EndpointSchema {
                schema_id: SchemaId::new(schema_id),
                schema_version: schema_version_from_i32(schema_version)?,
                kind: PayloadKind::Fact,
            })
        }
        _ => Err(StorageError::ConstraintViolation(format!(
            "edge: {} endpoint columns violate exactly-one invariant",
            side.as_str(),
        ))),
    }
}

fn endpoint_columns(
    draft: &EdgeDraft<'_>,
    side: EndpointSide,
) -> (Option<uuid::Uuid>, Option<uuid::Uuid>, Option<uuid::Uuid>) {
    match side {
        EndpointSide::Source => (
            draft.source_memory_id,
            draft.source_goal_id,
            draft.source_fact_entity_id,
        ),
        EndpointSide::Target => (
            draft.target_memory_id,
            draft.target_goal_id,
            draft.target_fact_entity_id,
        ),
    }
}

fn payload_kind_for_entity(kind: EntityKind) -> PayloadKind {
    match kind {
        EntityKind::Fact => PayloadKind::Fact,
        EntityKind::Abstraction => PayloadKind::Abstraction,
        EntityKind::Perspective => PayloadKind::Perspective,
        EntityKind::Goal => PayloadKind::Goal,
    }
}

fn schema_version_from_i32(value: i32) -> Result<SchemaVersion, StorageError> {
    let version = u32::try_from(value)
        .map_err(|_| StorageError::Internal(format!("schema_version does not fit u32: {value}")))?;
    Ok(SchemaVersion::new(version))
}

async fn insert_edge_row(
    tx: &mut sqlx::PgConnection,
    draft: &EdgeDraft<'_>,
) -> Result<bool, StorageError> {
    let descriptor = draft.relation.descriptor;
    let (owner_kind, owner_id) = draft.owner.columns();
    let inserted: Option<(uuid::Uuid,)> = sqlx::query_as(
        "INSERT INTO proxima_core.edges \
            (edge_id, relation, relation_class, \
             source_kind, source_memory_id, source_goal_id, source_fact_entity_id, \
             target_kind, target_memory_id, target_goal_id, target_fact_entity_id, \
             authorship_kind, authorship_owner_memory_id, owner_kind, owner_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15) \
         ON CONFLICT (edge_id) DO NOTHING \
         RETURNING edge_id",
    )
    .bind(draft.edge_id)
    .bind(descriptor.relation.as_str())
    .bind(descriptor.class)
    .bind(draft.source_kind)
    .bind(draft.source_memory_id)
    .bind(draft.source_goal_id)
    .bind(draft.source_fact_entity_id)
    .bind(draft.target_kind)
    .bind(draft.target_memory_id)
    .bind(draft.target_goal_id)
    .bind(draft.target_fact_entity_id)
    .bind(draft.authorship_kind)
    .bind(draft.authorship_owner_memory_id)
    .bind(owner_kind)
    .bind(owner_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_err)?;
    Ok(inserted.is_some())
}

async fn append_edge_change_event(
    tx: &mut sqlx::PgConnection,
    draft: &EdgeDraft<'_>,
) -> Result<(), StorageError> {
    let (owner_kind, owner_id) = draft.owner.columns();
    let descriptor = draft.relation.descriptor;
    let seq = uuid::Uuid::now_v7();
    sqlx::query(
        "INSERT INTO proxima_core.change_event \
            (seq, owner_kind, owner_id, kind, \
             edge_id, edge_relation, \
             edge_source_memory_id, edge_source_goal_id, edge_source_fact_entity_id, \
             edge_target_memory_id, edge_target_goal_id, edge_target_fact_entity_id) \
         VALUES ($1, $2, $3, 'EdgeAppend', $4, $5, $6, $7, $8, $9, $10, $11)",
    )
    .bind(seq)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(draft.edge_id)
    .bind(descriptor.relation.as_str())
    .bind(draft.source_memory_id)
    .bind(draft.source_goal_id)
    .bind(draft.source_fact_entity_id)
    .bind(draft.target_memory_id)
    .bind(draft.target_goal_id)
    .bind(draft.target_fact_entity_id)
    .execute(&mut *tx)
    .await
    .map_err(map_err)?;

    Ok(())
}

fn exactly_one_endpoint(
    memory: Option<uuid::Uuid>,
    goal: Option<uuid::Uuid>,
    fact_entity: Option<uuid::Uuid>,
) -> bool {
    usize::from(memory.is_some()) + usize::from(goal.is_some()) + usize::from(fact_entity.is_some())
        == 1
}

fn endpoint_binding(
    memory: Option<uuid::Uuid>,
    goal: Option<uuid::Uuid>,
    fact_entity: Option<uuid::Uuid>,
) -> EndpointBinding {
    if fact_entity.is_some() {
        EndpointBinding::FollowHead
    } else {
        debug_assert!(memory.is_some() || goal.is_some());
        EndpointBinding::Pin
    }
}
