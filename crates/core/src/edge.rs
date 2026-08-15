//! Edges — the connection index over `proxima_core.edges`.
//!
//! See docs/16-edges.md, which supersedes docs/02 §Edges, §Relation
//! Registry and §The Directionality Rule.
//!
//! > Edges are fundamental, non-extensible connection patterns. An edge
//! > carries no information beyond its existence: its endpoints, its
//! > direction, its creation time, and its kind. All content lives in
//! > nodes; meaning arises from the synthesis of the connected nodes.
//!
//! Two consequences shape every type below:
//!
//! - **Kind follows operation.** [`EdgeKind`] is never a parameter a
//!   writer chooses. `Origin` is what a derivation declaration produces;
//!   `Reference` is what a schema-declared reference field produces. No
//!   public API takes an `EdgeKind` from a caller.
//! - **The row is its own identity.** There is no edge id. The primary
//!   key is `(source, target, kind)`, so replaying a write re-asserts the
//!   same row and idempotency is structural rather than derived from a
//!   content hash.

use crate::change_event::EntityRef;
use crate::{EntityKind, FactEntityId, GoalId, MemoryId};

/// Closed substrate vocabulary for what an edge *is*. Two variants, not
/// extensible — not by flavors, not by core features. A feature that
/// seems to need a third kind fails the node-home test (docs/16 §The
/// Thesis) and is missing a node, not a kind.
///
/// Discriminator values match the SQL enum `proxima_core.edge_kind`.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, sqlx::Type,
)]
#[sqlx(type_name = "proxima_core.edge_kind", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum EdgeKind {
    /// A memory declares what it was made from. Written only by a node
    /// write carrying a derivation declaration (`derived_from`), in that
    /// write's own transaction.
    Origin,
    /// A payload field points at another node. Derived at ingest from the
    /// schema-declared reference fields of the node's own payload.
    Reference,
}

impl EdgeKind {
    /// SQL discriminator. Stable contract — must match the labels of
    /// `proxima_core.edge_kind`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Origin => "origin",
            Self::Reference => "reference",
        }
    }
}

/// One end of an edge: where it points ([`EntityRef`] — a memory row, a
/// Goal, or a Fact-entity head, the only three address forms) plus the
/// entity kind stored there.
///
/// The address form *is* the durable binding: a `FactEntity` address
/// follows the head, a `Memory`/`Goal` address pins the row.
///
/// The kind travels with the address because the F/A/P layering rule and
/// every wire projection need it, and re-deriving it per read is what
/// made edge reads a second query.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct EdgeEndpoint {
    pub kind: EntityKind,
    pub entity: EntityRef,
}

impl EdgeEndpoint {
    /// A pinned memory row.
    #[must_use]
    pub const fn memory(kind: EntityKind, memory_id: MemoryId) -> Self {
        Self {
            kind,
            entity: EntityRef::Memory(memory_id),
        }
    }

    /// A Goal row. Goal endpoints sit outside the F/A/P layer comparison.
    #[must_use]
    pub const fn goal(goal_id: GoalId) -> Self {
        Self {
            kind: EntityKind::Goal,
            entity: EntityRef::Goal(goal_id),
        }
    }

    /// A Fact-entity head — the address that follows the head pointer
    /// instead of pinning one observation.
    #[must_use]
    pub const fn fact_entity(fact_entity_id: FactEntityId) -> Self {
        Self {
            kind: EntityKind::Fact,
            entity: EntityRef::FactEntity(fact_entity_id),
        }
    }

    /// The memory row this endpoint pins, if it pins one.
    #[must_use]
    pub const fn memory_id(self) -> Option<MemoryId> {
        match self.entity {
            EntityRef::Memory(memory_id) => Some(memory_id),
            EntityRef::Goal(_) | EntityRef::FactEntity(_) => None,
        }
    }

    /// F/A/P layer index, or `None` for Goal endpoints, which are a
    /// separate entity axis (docs/06) and never enter the comparison.
    #[must_use]
    pub const fn layer(self) -> Option<u8> {
        match self.kind {
            EntityKind::Fact => Some(0),
            EntityKind::Abstraction => Some(1),
            EntityKind::Perspective => Some(2),
            EntityKind::Goal => None,
        }
    }
}

/// Target side of an edge as projected to one reader. A readable edge
/// may still have an endpoint the reader may not see; the row is
/// returned with the endpoint withheld rather than suppressed, so the
/// existence of a connection is not itself leaked or hidden by accident.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum EdgeTargetProjection {
    Visible { target: EdgeEndpoint },
    Redacted,
    Unavailable,
}

impl EdgeTargetProjection {
    #[must_use]
    pub const fn visible(target: EdgeEndpoint) -> Self {
        Self::Visible { target }
    }

    #[must_use]
    pub const fn endpoint(self) -> Option<EdgeEndpoint> {
        match self {
            Self::Visible { target } => Some(target),
            Self::Redacted | Self::Unavailable => None,
        }
    }
}

/// A whole edge, as read. Four fields is the entire model — there is no
/// id, no relation, no namespace, no authorship column, no payload, no
/// citation and no status. A connection that needs to say more than
/// "these two, this way, since then" is a node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Edge {
    pub source: EdgeEndpoint,
    pub target: EdgeTargetProjection,
    pub kind: EdgeKind,
    pub created_at: time::OffsetDateTime,
}

/// Enforce the layering rule for a proposed edge: `ℓ(source) ≥
/// ℓ(target)` for memory endpoints, with Goal endpoints outside the
/// comparison (docs/16 §Direction and layering).
///
/// This is what keeps a Fact from being an interpretation *source*: a
/// Fact asserts no judgment, and every interpretation is a Perspective
/// referring downward.
///
/// # Errors
///
/// Returns a message when both endpoints are memories and the source
/// sits below the target in the F/A/P order.
pub fn validate_edge_layering(source: EdgeEndpoint, target: EdgeEndpoint) -> Result<(), String> {
    let (Some(source_layer), Some(target_layer)) = (source.layer(), target.layer()) else {
        return Ok(());
    };
    if source_layer < target_layer {
        return Err(format!(
            "layering violation: a {} cannot point at a {} (source layer must be at least the target layer)",
            source.kind.as_str(),
            target.kind.as_str(),
        ));
    }
    Ok(())
}

/// A self-loop is never a connection between two things — it is a row
/// asserting that a node relates to itself, which no node write can mean.
///
/// # Errors
///
/// Returns a message when both endpoints address the same entity.
pub fn validate_not_self_loop(source: EdgeEndpoint, target: EdgeEndpoint) -> Result<(), String> {
    if source.entity == target.entity {
        return Err("an edge cannot point at its own source".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        EdgeEndpoint, EdgeKind, EdgeTargetProjection, validate_edge_layering,
        validate_not_self_loop,
    };
    use crate::{EntityKind, GoalId, MemoryId};

    fn memory(kind: EntityKind) -> EdgeEndpoint {
        EdgeEndpoint::memory(kind, MemoryId::new(uuid::Uuid::now_v7()))
    }

    #[test]
    fn kind_discriminators_are_the_sql_labels() {
        assert_eq!(EdgeKind::Origin.as_str(), "origin");
        assert_eq!(EdgeKind::Reference.as_str(), "reference");
    }

    /// The vocabulary is closed at two. A third variant is a design
    /// change (docs/16 §Kinds are closed), not a patch, and this is the
    /// test that says so out loud.
    #[test]
    fn the_vocabulary_is_exactly_two_kinds() {
        let all = [EdgeKind::Origin, EdgeKind::Reference];
        assert_eq!(all.len(), 2);
        for kind in all {
            match kind {
                EdgeKind::Origin | EdgeKind::Reference => {}
            }
        }
    }

    #[test]
    fn layering_admits_downward_and_level_memory_edges() {
        for (source, target) in [
            (EntityKind::Perspective, EntityKind::Abstraction),
            (EntityKind::Perspective, EntityKind::Fact),
            (EntityKind::Abstraction, EntityKind::Fact),
            (EntityKind::Fact, EntityKind::Fact),
            (EntityKind::Perspective, EntityKind::Perspective),
        ] {
            validate_edge_layering(memory(source), memory(target))
                .expect("source layer at least target layer is admitted");
        }
    }

    #[test]
    fn layering_rejects_upward_memory_edges() {
        for (source, target) in [
            (EntityKind::Fact, EntityKind::Abstraction),
            (EntityKind::Fact, EntityKind::Perspective),
            (EntityKind::Abstraction, EntityKind::Perspective),
        ] {
            validate_edge_layering(memory(source), memory(target))
                .expect_err("an upward memory edge is a layering violation");
        }
    }

    #[test]
    fn goal_endpoints_sit_outside_the_layer_comparison() {
        let goal = EdgeEndpoint::goal(GoalId::new(uuid::Uuid::now_v7()));
        validate_edge_layering(goal, memory(EntityKind::Perspective))
            .expect("a Goal endpoint is not compared by layer");
        validate_edge_layering(memory(EntityKind::Fact), goal)
            .expect("a Goal endpoint is not compared by layer");
    }

    #[test]
    fn a_self_loop_is_rejected() {
        let endpoint = memory(EntityKind::Perspective);
        validate_not_self_loop(endpoint, endpoint).expect_err("self-loop");
        validate_not_self_loop(endpoint, memory(EntityKind::Fact)).expect("distinct endpoints");
    }

    #[test]
    fn a_redacted_target_yields_no_endpoint() {
        let endpoint = memory(EntityKind::Fact);
        assert_eq!(
            EdgeTargetProjection::visible(endpoint).endpoint(),
            Some(endpoint)
        );
        assert_eq!(EdgeTargetProjection::Redacted.endpoint(), None);
        assert_eq!(EdgeTargetProjection::Unavailable.endpoint(), None);
    }
}
