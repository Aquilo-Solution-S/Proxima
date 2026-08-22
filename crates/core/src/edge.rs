//! Pins — `memory.origins` / `memory.refs`. There is no edge table.
//!
//! docs/16-edges.md is the reference.
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
//! - **The pin is its own identity.** There is no edge row and no edge
//!   id: a pin is a `MemoryId` in the source row's `origins` or `refs`
//!   column, so replaying a write re-asserts the same column values and
//!   idempotency is structural rather than derived from a content hash.
//!   The [`Edge`] / [`EdgeKind`] types below are the read-side
//!   projection of those columns, not a table.

use crate::change_event::EntityRef;
use crate::{EntityKind, GoalId, MemoryId, SchemaId};

/// Closed substrate vocabulary for what an edge *is*. Two variants, not
/// extensible — not by flavors, not by core features. A feature that
/// seems to need a third kind fails the node-home test (docs/16 §The
/// Thesis) and is missing a node, not a kind.
///
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
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

/// One end of an edge: where it points ([`EntityRef`] — a memory row or
/// a Goal) plus the entity kind stored there.
///
/// A `Memory`/`Goal` address pins the row. There is no follow-head address.
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

    /// The memory row this endpoint pins, if it pins one.
    #[must_use]
    pub const fn memory_id(self) -> Option<MemoryId> {
        match self.entity {
            EntityRef::Memory(memory_id) => Some(memory_id),
            EntityRef::Goal(_) => None,
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

/// A memory row as a pin carrier. Storage returns these; [`Edge`] is a
/// view of `origins` / `refs` built in the engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinNode {
    pub id: MemoryId,
    pub kind: EntityKind,
    /// Which schema wrote this row, and therefore which `Provenance` the
    /// lineage walk must consult before treating `origins` as lineage.
    ///
    /// Carried on the node rather than looked up by the walk because the
    /// walk has no owner-scoped read of its own: the node is the only thing
    /// that crossed the authorization boundary.
    pub schema_id: SchemaId,
    pub origins: Vec<MemoryId>,
    pub refs: Vec<MemoryId>,
}

impl PinNode {
    /// Origin pins first, then reference pins.
    pub fn pins(&self) -> impl Iterator<Item = (MemoryId, EdgeKind)> + '_ {
        self.origins
            .iter()
            .copied()
            .map(|id| (id, EdgeKind::Origin))
            .chain(
                self.refs
                    .iter()
                    .copied()
                    .map(|id| (id, EdgeKind::Reference)),
            )
    }
}

/// `created_at` of a pin is the source row's uuidv7 timestamp.
#[must_use]
pub fn pin_created_at(source: MemoryId) -> time::OffsetDateTime {
    source
        .into_inner()
        .get_timestamp()
        .and_then(|ts| {
            let (secs, nanos) = ts.to_unix();
            time::OffsetDateTime::from_unix_timestamp(i64::try_from(secs).ok()?)
                .ok()?
                .replace_nanosecond(nanos)
                .ok()
        })
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH)
}

/// Query-window edges: both endpoints must be in `nodes`. Other pins
/// are omitted, not redacted.
#[must_use]
pub fn project_window_edges(nodes: &[PinNode], cap: usize) -> Vec<Edge> {
    let visible: std::collections::HashMap<MemoryId, EntityKind> =
        nodes.iter().map(|node| (node.id, node.kind)).collect();
    let mut edges = Vec::new();
    for source in nodes {
        for (target, kind) in source.pins() {
            let Some(target_kind) = visible.get(&target).copied() else {
                continue;
            };
            edges.push(Edge {
                source: EdgeEndpoint::memory(source.kind, source.id),
                target: EdgeTargetProjection::visible(EdgeEndpoint::memory(target_kind, target)),
                kind,
                created_at: pin_created_at(source.id),
            });
            if edges.len() >= cap {
                return edges;
            }
        }
    }
    edges
}

/// Listing projection: a pin whose target is not in `visible` is
/// [`EdgeTargetProjection::Redacted`].
#[must_use]
pub fn project_listed_edge<S: std::hash::BuildHasher>(
    source_kind: EntityKind,
    source: MemoryId,
    target: MemoryId,
    kind: EdgeKind,
    visible: &std::collections::HashMap<MemoryId, EntityKind, S>,
) -> Edge {
    let target = match visible.get(&target).copied() {
        Some(target_kind) => {
            EdgeTargetProjection::visible(EdgeEndpoint::memory(target_kind, target))
        }
        None => EdgeTargetProjection::Redacted,
    };
    Edge {
        source: EdgeEndpoint::memory(source_kind, source),
        target,
        kind,
        created_at: pin_created_at(source),
    }
}

/// Enforce the layering rule for a proposed edge: `ℓ(source) ≥
/// ℓ(target)` for memory endpoints, with Goal endpoints outside the
/// comparison (docs/16 §The Model).
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
        EdgeEndpoint, EdgeKind, EdgeTargetProjection, PinNode, project_listed_edge,
        project_window_edges, validate_edge_layering, validate_not_self_loop,
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
    /// change (docs/16 §The Model), not a patch, and this is the
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
            (EntityKind::Abstraction, EntityKind::Abstraction),
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

    #[test]
    fn window_projection_keeps_only_pins_inside_the_node_set() {
        let leaf = MemoryId::new(uuid::Uuid::now_v7());
        let hub = MemoryId::new(uuid::Uuid::now_v7());
        let outside = MemoryId::new(uuid::Uuid::now_v7());
        let nodes = [
            PinNode {
                id: leaf,
                kind: EntityKind::Fact,
                schema_id: crate::SchemaId::new("test/pin-v1".into()),
                origins: Vec::new(),
                refs: Vec::new(),
            },
            PinNode {
                id: hub,
                kind: EntityKind::Abstraction,
                schema_id: crate::SchemaId::new("test/pin-v1".into()),
                origins: vec![leaf],
                refs: vec![outside],
            },
        ];
        let edges = project_window_edges(&nodes, 50);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].kind, EdgeKind::Origin);
        assert_eq!(edges[0].source.memory_id(), Some(hub));
        assert_eq!(
            edges[0].target.endpoint().and_then(EdgeEndpoint::memory_id),
            Some(leaf)
        );
    }

    #[test]
    fn listed_projection_redacts_targets_absent_from_the_node_set() {
        let hub = MemoryId::new(uuid::Uuid::now_v7());
        let missing = MemoryId::new(uuid::Uuid::now_v7());
        let visible = std::collections::HashMap::new();
        let edge = project_listed_edge(
            EntityKind::Abstraction,
            hub,
            missing,
            EdgeKind::Reference,
            &visible,
        );
        assert!(matches!(edge.target, EdgeTargetProjection::Redacted));
    }
}
