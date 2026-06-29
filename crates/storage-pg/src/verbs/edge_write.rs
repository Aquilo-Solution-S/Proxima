//! Owner-checked edge write helpers for flavor ingestion.
//!
//! This is the public storage-pg edge write surface. Raw `EdgeDraft` /
//! endpoint-column assembly stays crate-private in `edge_append`.

use proxima_core::{
    EdgeAuthorshipKind, EdgeId, EdgePayload, EntityKind, FactEntityId, GoalId, MemoryId, Owner,
    OwnerRefKind, RegisteredRelation, RelationOwnerPolicy, RelationTargetAccessPolicy,
    StorageError,
};

use crate::error::map_err;
use crate::sidecars::PgEdgeSidecar;

use super::edge_append::{EdgeDraft, append_edge_in_tx, append_edge_with_sidecar_in_tx};

/// Memory endpoint for an owner-checked edge write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryEndpoint {
    pub kind: EntityKind,
    pub memory_id: MemoryId,
}

impl MemoryEndpoint {
    #[must_use]
    pub const fn fact(memory_id: MemoryId) -> Self {
        Self {
            kind: EntityKind::Fact,
            memory_id,
        }
    }

    #[must_use]
    pub const fn abstraction(memory_id: MemoryId) -> Self {
        Self {
            kind: EntityKind::Abstraction,
            memory_id,
        }
    }

    #[must_use]
    pub const fn perspective(memory_id: MemoryId) -> Self {
        Self {
            kind: EntityKind::Perspective,
            memory_id,
        }
    }
}

/// Endpoint for owner-checked storage edge writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckedEdgeEndpoint {
    Memory(MemoryEndpoint),
    Goal(GoalId),
    FactEntity(FactEntityId),
}

impl CheckedEdgeEndpoint {
    #[must_use]
    pub const fn fact(memory_id: MemoryId) -> Self {
        Self::Memory(MemoryEndpoint::fact(memory_id))
    }

    #[must_use]
    pub const fn abstraction(memory_id: MemoryId) -> Self {
        Self::Memory(MemoryEndpoint::abstraction(memory_id))
    }

    #[must_use]
    pub const fn perspective(memory_id: MemoryId) -> Self {
        Self::Memory(MemoryEndpoint::perspective(memory_id))
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
            Self::Memory(endpoint) => endpoint.kind,
            Self::Goal(_) => EntityKind::Goal,
            Self::FactEntity(_) => EntityKind::Fact,
        }
    }

    const fn memory_id(self) -> Option<uuid::Uuid> {
        match self {
            Self::Memory(endpoint) => Some(endpoint.memory_id.into_inner()),
            Self::Goal(_) | Self::FactEntity(_) => None,
        }
    }

    const fn goal_id(self) -> Option<uuid::Uuid> {
        match self {
            Self::Goal(goal_id) => Some(goal_id.into_inner()),
            Self::Memory(_) | Self::FactEntity(_) => None,
        }
    }

    const fn fact_entity_id(self) -> Option<uuid::Uuid> {
        match self {
            Self::FactEntity(fact_entity_id) => Some(fact_entity_id.into_inner()),
            Self::Memory(_) | Self::Goal(_) => None,
        }
    }
}

impl From<MemoryEndpoint> for CheckedEdgeEndpoint {
    fn from(value: MemoryEndpoint) -> Self {
        Self::Memory(value)
    }
}

/// Append an untyped memory-to-memory edge after endpoint owner checks.
///
/// # Errors
///
/// Returns a storage error when endpoints are absent, endpoint ownership does
/// not prove the descriptor's target gate for this trusted-owner path, relation
/// shape validation fails, or the database write fails.
#[allow(clippy::too_many_arguments)]
pub async fn append_owner_checked_memory_edge(
    tx: &mut sqlx::PgConnection,
    owner: &Owner,
    edge_id: EdgeId,
    relation: RegisteredRelation<'_>,
    source: MemoryEndpoint,
    target: MemoryEndpoint,
    authorship_kind: EdgeAuthorshipKind,
    authorship_owner_memory_id: Option<MemoryId>,
) -> Result<EdgeId, StorageError> {
    append_owner_checked_edge(
        tx,
        owner,
        edge_id,
        relation,
        source.into(),
        target.into(),
        authorship_kind,
        authorship_owner_memory_id,
    )
    .await
}

/// Append an untyped edge after endpoint owner checks.
///
/// # Errors
///
/// Returns a storage error when endpoint ownership does not prove this trusted
/// owner path's descriptor target gate, relation shape validation fails, or the
/// database write fails.
#[allow(clippy::too_many_arguments)]
pub async fn append_owner_checked_edge(
    tx: &mut sqlx::PgConnection,
    owner: &Owner,
    edge_id: EdgeId,
    relation: RegisteredRelation<'_>,
    source: CheckedEdgeEndpoint,
    target: CheckedEdgeEndpoint,
    authorship_kind: EdgeAuthorshipKind,
    authorship_owner_memory_id: Option<MemoryId>,
) -> Result<EdgeId, StorageError> {
    validate_owner_checked_edge(tx, owner, relation, source, target).await?;
    let draft = edge_draft(
        owner,
        edge_id,
        relation,
        source,
        target,
        authorship_kind,
        authorship_owner_memory_id,
    );
    append_edge_in_tx(tx, &draft).await?;
    Ok(edge_id)
}

/// Append a typed memory-to-memory edge and its typed sidecar after endpoint
/// owner checks.
///
/// # Errors
///
/// Returns a storage error when endpoint owner checks, relation shape, sidecar
/// validation, or database writes fail.
#[allow(clippy::too_many_arguments)]
pub async fn append_owner_checked_typed_memory_edge<E: EdgePayload + PgEdgeSidecar + Clone>(
    tx: &mut sqlx::PgConnection,
    owner: &Owner,
    edge_id: EdgeId,
    relation: RegisteredRelation<'_>,
    source: MemoryEndpoint,
    target: MemoryEndpoint,
    authorship_kind: EdgeAuthorshipKind,
    authorship_owner_memory_id: Option<MemoryId>,
    payload: &E,
) -> Result<EdgeId, StorageError> {
    append_owner_checked_typed_edge(
        tx,
        owner,
        edge_id,
        relation,
        source.into(),
        target.into(),
        authorship_kind,
        authorship_owner_memory_id,
        payload,
    )
    .await
}

/// Append a typed edge and sidecar after endpoint owner checks.
///
/// # Errors
///
/// Returns a storage error when endpoint owner checks, sidecar schema
/// validation, relation shape validation, or database writes fail.
#[allow(clippy::too_many_arguments)]
pub async fn append_owner_checked_typed_edge<E: EdgePayload + PgEdgeSidecar + Clone>(
    tx: &mut sqlx::PgConnection,
    owner: &Owner,
    edge_id: EdgeId,
    relation: RegisteredRelation<'_>,
    source: CheckedEdgeEndpoint,
    target: CheckedEdgeEndpoint,
    authorship_kind: EdgeAuthorshipKind,
    authorship_owner_memory_id: Option<MemoryId>,
    payload: &E,
) -> Result<EdgeId, StorageError> {
    validate_owner_checked_edge(tx, owner, relation, source, target).await?;
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
    let draft = edge_draft(
        owner,
        edge_id,
        relation,
        source,
        target,
        authorship_kind,
        authorship_owner_memory_id,
    );
    let sidecar_payload = payload.clone();
    append_edge_with_sidecar_in_tx(tx, &draft, move |tx, edge_id| {
        Box::pin(async move { sidecar_payload.insert_edge_sidecar(tx, edge_id).await })
    })
    .await?;
    Ok(edge_id)
}

fn edge_draft<'a>(
    owner: &'a Owner,
    edge_id: EdgeId,
    relation: RegisteredRelation<'a>,
    source: CheckedEdgeEndpoint,
    target: CheckedEdgeEndpoint,
    authorship_kind: EdgeAuthorshipKind,
    authorship_owner_memory_id: Option<MemoryId>,
) -> EdgeDraft<'a> {
    EdgeDraft {
        edge_id: edge_id.into_inner(),
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

async fn validate_owner_checked_edge(
    tx: &mut sqlx::PgConnection,
    owner: &Owner,
    relation: RegisteredRelation<'_>,
    source: CheckedEdgeEndpoint,
    target: CheckedEdgeEndpoint,
) -> Result<(), StorageError> {
    let source_owner = endpoint_owner(tx, source).await?;
    if &source_owner != owner {
        return Err(StorageError::ConstraintViolation(
            "edge source owner does not match authorized owner".into(),
        ));
    }
    let target_owner = endpoint_owner(tx, target).await?;
    if relation.descriptor.owner_policy == RelationOwnerPolicy::SameOwner
        && target_owner != source_owner
    {
        return Err(StorageError::ConstraintViolation(format!(
            "relation {} requires source and target to have the same owner",
            relation.descriptor.relation
        )));
    }
    if matches!(
        relation.descriptor.target_access_policy,
        RelationTargetAccessPolicy::Read | RelationTargetAccessPolicy::Write
    ) && target_owner != source_owner
    {
        return Err(StorageError::ConstraintViolation(format!(
            "relation {} target access is not proven by owner-checked storage write",
            relation.descriptor.relation
        )));
    }
    Ok(())
}

async fn endpoint_owner(
    tx: &mut sqlx::PgConnection,
    endpoint: CheckedEdgeEndpoint,
) -> Result<Owner, StorageError> {
    let row: Option<(OwnerRefKind, Option<uuid::Uuid>)> = match endpoint {
        CheckedEdgeEndpoint::Memory(endpoint) => sqlx::query_as(
            "SELECT owner_kind, owner_id FROM proxima_core.memories WHERE memory_id = $1",
        )
        .bind(endpoint.memory_id.into_inner())
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_err)?,
        CheckedEdgeEndpoint::Goal(goal_id) => {
            sqlx::query_as("SELECT owner_kind, owner_id FROM proxima_core.goals WHERE goal_id = $1")
                .bind(goal_id.into_inner())
                .fetch_optional(&mut *tx)
                .await
                .map_err(map_err)?
        }
        CheckedEdgeEndpoint::FactEntity(fact_entity_id) => sqlx::query_as(
            "SELECT owner_kind, owner_id FROM proxima_core.fact_entities WHERE fact_entity_id = $1",
        )
        .bind(fact_entity_id.into_inner())
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_err)?,
    };
    let (kind, id) = row.ok_or(StorageError::NotFound)?;
    kind.with_uuid(id)
        .ok_or_else(|| StorageError::Internal("invalid OwnerRef columns".into()))
}
