//! Endpoint helpers. Pins live on `memory.origins` / `memory.refs`.

use proxima_core::{EdgeEndpoint, EntityKind, FactEntityId, GoalId, MemoryId};

/// One end of an edge as Postgres stores it: the entity kind and the address
/// form in a single value.
///
/// `FactEntityHead` follows the current Fact-entity head. A binding is
/// not a policy consulted per write — it is what the address *is*, so it
/// cannot disagree with the id beside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, sqlx::Type)]
#[sqlx(type_name = "proxima_core.edge_endpoint_kind")]
pub(crate) enum PgEndpointKind {
    Fact,
    Abstraction,
    Perspective,
    Goal,
    FactEntityHead,
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

