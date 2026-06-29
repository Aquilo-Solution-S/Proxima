#![allow(clippy::doc_markdown)]
//! Atomic edge + typed sidecar write.
//!
//! `append_edge_in_tx` inserts one `proxima_core.edges` row plus an
//! optional typed sidecar row (keyed on `edge_id`) plus the
//! `EdgeAppend` change_event row, all in a single transaction.
//!
//! Used by M5.5 typed F-layer edges (e.g. `proxima-code/calls`).

use proxima_core::verbs::schema::PayloadKind;
use proxima_core::{
    CapabilityTag, EdgeAuthorshipKind, EdgeId, EdgePayload, EndpointBinding, EntityKind,
    FactEntityId, GoalId, MemoryId, Owner, RegisteredRelation, SchemaId, SchemaVersion,
    StorageError,
};

use crate::error::map_err;
use crate::sidecars::{PgEdgeSidecar, PgSidecarFuture};

/// Durable edge endpoint with exactly one backing id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Endpoint {
    Memory {
        kind: EntityKind,
        memory_id: MemoryId,
    },
    Goal(GoalId),
    FactEntity(FactEntityId),
}

impl Endpoint {
    #[must_use]
    pub const fn fact(memory_id: MemoryId) -> Self {
        Self::Memory {
            kind: EntityKind::Fact,
            memory_id,
        }
    }

    #[must_use]
    pub const fn abstraction(memory_id: MemoryId) -> Self {
        Self::Memory {
            kind: EntityKind::Abstraction,
            memory_id,
        }
    }

    #[must_use]
    pub const fn perspective(memory_id: MemoryId) -> Self {
        Self::Memory {
            kind: EntityKind::Perspective,
            memory_id,
        }
    }

    #[must_use]
    pub const fn goal(goal_id: GoalId) -> Self {
        Self::Goal(goal_id)
    }

    #[must_use]
    pub const fn fact_entity(fact_entity_id: FactEntityId) -> Self {
        Self::FactEntity(fact_entity_id)
    }

    const fn kind(self) -> EntityKind {
        match self {
            Self::Memory { kind, .. } => kind,
            Self::Goal(_) => EntityKind::Goal,
            Self::FactEntity(_) => EntityKind::Fact,
        }
    }

    const fn memory_id(self) -> Option<uuid::Uuid> {
        match self {
            Self::Memory { memory_id, .. } => Some(memory_id.into_inner()),
            Self::Goal(_) | Self::FactEntity(_) => None,
        }
    }

    const fn goal_id(self) -> Option<uuid::Uuid> {
        match self {
            Self::Goal(goal_id) => Some(goal_id.into_inner()),
            Self::Memory { .. } | Self::FactEntity(_) => None,
        }
    }

    const fn fact_entity_id(self) -> Option<uuid::Uuid> {
        match self {
            Self::FactEntity(fact_entity_id) => Some(fact_entity_id.into_inner()),
            Self::Memory { .. } | Self::Goal(_) => None,
        }
    }
}

/// Draft of an edge to be written.
#[derive(Debug, Clone)]
pub struct EdgeDraft<'a> {
    pub edge_id: uuid::Uuid,
    pub relation: RegisteredRelation<'a>,
    pub source_kind: EntityKind,
    pub source_memory_id: Option<uuid::Uuid>,
    pub source_goal_id: Option<uuid::Uuid>,
    pub source_fact_entity_id: Option<uuid::Uuid>,
    pub target_kind: EntityKind,
    pub target_memory_id: Option<uuid::Uuid>,
    pub target_goal_id: Option<uuid::Uuid>,
    pub target_fact_entity_id: Option<uuid::Uuid>,
    pub authorship_kind: EdgeAuthorshipKind,
    pub authorship_owner_memory_id: Option<uuid::Uuid>,
    pub owner: &'a Owner,
}

impl<'a> EdgeDraft<'a> {
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        edge_id: uuid::Uuid,
        relation: RegisteredRelation<'a>,
        source: Endpoint,
        target: Endpoint,
        authorship_kind: EdgeAuthorshipKind,
        authorship_owner_memory_id: Option<MemoryId>,
        owner: &'a Owner,
    ) -> Self {
        Self {
            edge_id,
            relation,
            source_kind: source.kind(),
            source_memory_id: source.memory_id(),
            source_goal_id: source.goal_id(),
            source_fact_entity_id: source.fact_entity_id(),
            target_kind: target.kind(),
            target_memory_id: target.memory_id(),
            target_goal_id: target.goal_id(),
            target_fact_entity_id: target.fact_entity_id(),
            authorship_kind,
            authorship_owner_memory_id: authorship_owner_memory_id.map(MemoryId::into_inner),
            owner,
        }
    }
}

/// Write an untyped/substrate edge row and the EdgeAppend change_event.
///
/// # Errors
///
/// Returns storage errors from edge validation, row insertion, or change
/// event insertion.
#[allow(clippy::too_many_arguments)]
pub async fn append_edge(
    tx: &mut sqlx::PgConnection,
    edge_id: uuid::Uuid,
    relation: RegisteredRelation<'_>,
    source: Endpoint,
    target: Endpoint,
    authorship_kind: EdgeAuthorshipKind,
    authorship_owner_memory_id: Option<MemoryId>,
    owner: &Owner,
) -> Result<uuid::Uuid, StorageError> {
    let draft = EdgeDraft::new(
        edge_id,
        relation,
        source,
        target,
        authorship_kind,
        authorship_owner_memory_id,
        owner,
    );
    append_edge_in_tx(tx, &draft).await?;
    Ok(edge_id)
}

/// Write a typed edge row, its `EdgePayload` sidecar, and the
/// EdgeAppend change_event.
///
/// # Errors
///
/// Returns storage errors from schema mismatch, edge validation, row
/// insertion, sidecar insertion, or change event insertion.
#[allow(clippy::too_many_arguments)]
pub async fn append_typed_edge<E: EdgePayload + PgEdgeSidecar + Clone>(
    tx: &mut sqlx::PgConnection,
    edge_id: uuid::Uuid,
    relation: RegisteredRelation<'_>,
    source: Endpoint,
    target: Endpoint,
    authorship_kind: EdgeAuthorshipKind,
    authorship_owner_memory_id: Option<MemoryId>,
    owner: &Owner,
    payload: &E,
) -> Result<uuid::Uuid, StorageError> {
    let payload_schema = relation.descriptor.payload_schema.as_ref().ok_or_else(|| {
        StorageError::ConstraintViolation(format!(
            "typed EdgePayload supplied for substrate relation {}",
            relation.descriptor.relation,
        ))
    })?;
    if payload_schema.schema_id != E::schema_id()
        || payload_schema.schema_version.into_inner() != E::SCHEMA_VERSION
    {
        return Err(StorageError::ConstraintViolation(format!(
            "EdgePayload {} v{} does not match relation {} payload schema {} v{}",
            E::SCHEMA_ID,
            E::SCHEMA_VERSION,
            relation.descriptor.relation,
            payload_schema.schema_id.as_str(),
            payload_schema.schema_version.into_inner(),
        )));
    }
    let draft = EdgeDraft::new(
        edge_id,
        relation,
        source,
        target,
        authorship_kind,
        authorship_owner_memory_id,
        owner,
    );
    let sidecar_payload = payload.clone();
    append_edge_with_sidecar_in_tx(tx, &draft, move |tx, edge_id| {
        Box::pin(async move { sidecar_payload.insert_edge_sidecar(tx, edge_id).await })
    })
    .await?;
    Ok(edge_id)
}

/// Write a substrate edge row + the EdgeAppend change_event in one
/// transaction.
///
/// # Errors
///
/// Returns `ConstraintViolation` on FK / check failures; `Internal`
/// on sqlx failure.
pub async fn append_edge_in_tx(
    tx: &mut sqlx::PgConnection,
    draft: &EdgeDraft<'_>,
) -> Result<(), StorageError> {
    validate_edge_draft(draft, false)?;
    validate_endpoint_required_tags(tx, draft).await?;

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
pub async fn append_edge_with_sidecar_in_tx(
    tx: &mut sqlx::PgConnection,
    draft: &EdgeDraft<'_>,
    sidecar: impl for<'t> FnOnce(&'t mut sqlx::PgConnection, EdgeId) -> PgSidecarFuture<'t>,
) -> Result<(), StorageError> {
    validate_edge_draft(draft, true)?;
    validate_endpoint_required_tags(tx, draft).await?;

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
    Ok(())
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
