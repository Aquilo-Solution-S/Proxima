//! Endpoint helpers. Pins live on `memory.origins` / `memory.refs`.

use proxima_core::{EdgeEndpoint, EntityKind, GoalId, MemoryId};

/// One end of an edge as Postgres stores it: the entity kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum PgEndpointKind {
    Fact,
    Abstraction,
    Perspective,
    Goal,
}

/// Rebuild an endpoint from the two columns that address it.
pub(crate) const fn endpoint_from_columns(kind: PgEndpointKind, id: uuid::Uuid) -> EdgeEndpoint {
    match kind {
        PgEndpointKind::Goal => EdgeEndpoint::goal(GoalId::new(id)),
        PgEndpointKind::Fact => EdgeEndpoint::memory(EntityKind::Fact, MemoryId::new(id)),
        PgEndpointKind::Abstraction => {
            EdgeEndpoint::memory(EntityKind::Abstraction, MemoryId::new(id))
        }
        PgEndpointKind::Perspective => {
            EdgeEndpoint::memory(EntityKind::Perspective, MemoryId::new(id))
        }
    }
}

