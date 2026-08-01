//! The connection index over `proxima_core.edges`.
//!
//! There is no `append_edge` verb here, and there is deliberately no public
//! one anywhere. Every function below is called *by a node write, inside that
//! write's own transaction*, with a kind the write itself determines:
//! [`EdgeKind::Origin`] for a derivation declaration, [`EdgeKind::Reference`]
//! for a schema-declared payload field. Nothing takes an [`EdgeKind`] from a
//! caller (docs/16 §Kernel Invariants, E4).
//!
//! Idempotency is structural. The primary key is the row, so re-running a
//! write re-asserts the same key and the second insert is a no-op; there is
//! no id to mint, no content hash to derive, and no partial unique index to
//! keep honest.

use proxima_core::{
    EdgeEndpoint, EdgeKind, EntityKind, EntityRef, FactEntityId, GoalId, MemoryId, Owner,
    StorageError, validate_edge_layering, validate_not_self_loop,
};

use crate::error::map_err;

/// One end of an edge as Postgres stores it: the entity kind and the address
/// form in a single value.
///
/// `FactEntityHead` is what the old descriptor's `FollowHead` binding became.
/// A binding is not a policy consulted per write — it is what the address
/// *is*, so it cannot disagree with the id beside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, sqlx::Type)]
#[sqlx(type_name = "proxima_core.edge_endpoint_kind")]
pub(crate) enum PgEndpointKind {
    Fact,
    Abstraction,
    Perspective,
    Goal,
    FactEntityHead,
}

/// Split an endpoint into the two columns that address it.
///
/// # Errors
///
/// Returns `ConstraintViolation` for the two shapes the type admits but the
/// model does not: a memory row claiming kind Goal, and a Goal row claiming a
/// memory kind.
pub(crate) fn endpoint_columns(
    endpoint: EdgeEndpoint,
) -> Result<(PgEndpointKind, uuid::Uuid), StorageError> {
    match (endpoint.entity, endpoint.kind) {
        (EntityRef::Memory(memory_id), EntityKind::Fact) => {
            Ok((PgEndpointKind::Fact, memory_id.into_inner()))
        }
        (EntityRef::Memory(memory_id), EntityKind::Abstraction) => {
            Ok((PgEndpointKind::Abstraction, memory_id.into_inner()))
        }
        (EntityRef::Memory(memory_id), EntityKind::Perspective) => {
            Ok((PgEndpointKind::Perspective, memory_id.into_inner()))
        }
        (EntityRef::Goal(goal_id), EntityKind::Goal) => {
            Ok((PgEndpointKind::Goal, goal_id.into_inner()))
        }
        (EntityRef::FactEntity(fact_entity_id), EntityKind::Fact) => {
            Ok((PgEndpointKind::FactEntityHead, fact_entity_id.into_inner()))
        }
        (EntityRef::Memory(_), EntityKind::Goal) => Err(StorageError::ConstraintViolation(
            "edge endpoint: a memory row cannot have kind Goal".into(),
        )),
        (EntityRef::Goal(_) | EntityRef::FactEntity(_), _) => {
            Err(StorageError::ConstraintViolation(format!(
                "edge endpoint: {} address does not admit kind {}",
                match endpoint.entity {
                    EntityRef::Goal(_) => "a Goal",
                    _ => "a Fact-entity head",
                },
                endpoint.kind.as_str(),
            )))
        }
    }
}

/// Rebuild an endpoint from the two columns that address it.
pub(crate) const fn endpoint_from_columns(kind: PgEndpointKind, id: uuid::Uuid) -> EdgeEndpoint {
    match kind {
        PgEndpointKind::Goal => EdgeEndpoint::goal(GoalId::new(id)),
        PgEndpointKind::FactEntityHead => EdgeEndpoint::fact_entity(FactEntityId::new(id)),
        PgEndpointKind::Fact => EdgeEndpoint::memory(EntityKind::Fact, MemoryId::new(id)),
        PgEndpointKind::Abstraction => {
            EdgeEndpoint::memory(EntityKind::Abstraction, MemoryId::new(id))
        }
        PgEndpointKind::Perspective => {
            EdgeEndpoint::memory(EntityKind::Perspective, MemoryId::new(id))
        }
    }
}

/// Assert the index rows one node write implies, in that write's transaction.
///
/// `kind` is supplied by the call site, never by a caller of the call site:
/// the `origins` arm passes [`EdgeKind::Origin`] because a derivation
/// declaration is what it read, the `references` arm passes
/// [`EdgeKind::Reference`] because a schema-declared field is what it read.
///
/// Returns how many distinct rows the write asserted. Assert, not insert: a
/// replay re-asserts the same primary keys and reports the same count while
/// adding nothing, which is what structural idempotency means.
///
/// # Errors
///
/// Returns `ConstraintViolation` for a self-loop, a layering violation, an
/// absent endpoint, or an owner that is not the source owner; `Internal` for
/// storage faults.
pub(crate) async fn assert_index_rows_in_tx(
    tx: &mut sqlx::PgConnection,
    owner: &Owner,
    source: EdgeEndpoint,
    kind: EdgeKind,
    targets: &[EdgeEndpoint],
) -> Result<usize, StorageError> {
    let mut asserted = 0_usize;
    let mut seen = std::collections::BTreeSet::new();
    for target in targets {
        validate_not_self_loop(source, *target).map_err(StorageError::ConstraintViolation)?;
        validate_edge_layering(source, *target).map_err(StorageError::ConstraintViolation)?;
        let (source_kind, source_id) = endpoint_columns(source)?;
        let (target_kind, target_id) = endpoint_columns(*target)?;
        if !seen.insert((source_kind, source_id, target_kind, target_id)) {
            continue;
        }
        asserted += 1;
        let (owner_kind, owner_id) = owner.columns();
        let inserted: Option<(uuid::Uuid,)> = sqlx::query_as(
            "INSERT INTO proxima_core.edges
                (source_kind, source_id, target_kind, target_id, kind, owner_kind, owner_id)
             VALUES ($1, $2, $3, $4, $5, $6, $7)
             ON CONFLICT (source_kind, source_id, target_kind, target_id, kind) DO NOTHING
             RETURNING source_id",
        )
        .bind(source_kind)
        .bind(source_id)
        .bind(target_kind)
        .bind(target_id)
        .bind(kind)
        .bind(owner_kind)
        .bind(owner_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_err)?;
        if inserted.is_none() {
            // The row was already there. Nothing changed, so nothing is
            // announced — a change event per replay would report a change
            // that did not happen.
            continue;
        }
        append_edge_change_event(
            tx,
            owner,
            (source_kind, source_id),
            (target_kind, target_id),
            kind,
        )
        .await?;
    }
    Ok(asserted)
}

async fn append_edge_change_event(
    tx: &mut sqlx::PgConnection,
    owner: &Owner,
    source: (PgEndpointKind, uuid::Uuid),
    target: (PgEndpointKind, uuid::Uuid),
    kind: EdgeKind,
) -> Result<(), StorageError> {
    let (owner_kind, owner_id) = owner.columns();
    sqlx::query(
        "INSERT INTO proxima_core.change_event
            (seq, owner_kind, owner_id, kind,
             edge_kind, edge_source_kind, edge_source_id,
             edge_target_kind, edge_target_id)
         VALUES ($1, $2, $3, 'EdgeAppend', $4, $5, $6, $7, $8)",
    )
    .bind(uuid::Uuid::now_v7())
    .bind(owner_kind)
    .bind(owner_id)
    .bind(kind)
    .bind(source.0)
    .bind(source.1)
    .bind(target.0)
    .bind(target.1)
    .execute(&mut *tx)
    .await
    .map_err(map_err)?;
    Ok(())
}

/// The index rows a node write declared, read back for replay comparison.
///
/// Rebuildability (E7) is the master invariant: the edge set is a function of
/// node content. A replay that declares a different set is therefore a
/// different write wearing the same idempotency key, and the comparison below
/// is what says so.
pub(crate) async fn stored_index_rows_in_tx(
    tx: &mut sqlx::PgConnection,
    source: EdgeEndpoint,
) -> Result<std::collections::BTreeSet<(String, String, uuid::Uuid)>, StorageError> {
    let (source_kind, source_id) = endpoint_columns(source)?;
    let rows: Vec<(EdgeKind, PgEndpointKind, uuid::Uuid)> = sqlx::query_as(
        "SELECT kind, target_kind, target_id
           FROM proxima_core.edges
          WHERE source_kind = $1 AND source_id = $2",
    )
    .bind(source_kind)
    .bind(source_id)
    .fetch_all(&mut *tx)
    .await
    .map_err(map_err)?;
    Ok(rows
        .into_iter()
        .map(|(kind, target_kind, target_id)| {
            (
                kind.as_str().to_string(),
                format!("{target_kind:?}"),
                target_id,
            )
        })
        .collect())
}

/// The same set, computed from what a write declares rather than from what
/// storage holds.
pub(crate) fn declared_index_rows(
    origins: &[EdgeEndpoint],
    references: &[EdgeEndpoint],
) -> Result<std::collections::BTreeSet<(String, String, uuid::Uuid)>, StorageError> {
    let mut out = std::collections::BTreeSet::new();
    for (kind, targets) in [
        (EdgeKind::Origin, origins),
        (EdgeKind::Reference, references),
    ] {
        for target in targets {
            let (target_kind, target_id) = endpoint_columns(*target)?;
            out.insert((
                kind.as_str().to_string(),
                format!("{target_kind:?}"),
                target_id,
            ));
        }
    }
    Ok(out)
}
