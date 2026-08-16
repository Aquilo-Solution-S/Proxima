//! Shared wire formatting for graph endpoints. One place turns an
//! [`EntityRef`] or [`EdgeTargetProjection`] into a prefixed wire handle,
//! so the kind→prefix mapping and the redaction sentinels cannot drift
//! between the edge-bearing read tools (edges, change events, search
//! neighbor edges, lineage).

use crate::mcp::McpToolCtx;
use crate::{EdgeEndpoint, EdgeTargetProjection, EntityKind, EntityRef, MemoryId};

/// Placeholder for an edge target the caller may not see. Part of the
/// wire contract: every edge-bearing output uses this exact string.
pub(crate) const REDACTED_TARGET: &str = "redacted target";
/// Placeholder for an edge target that could not be projected.
pub(crate) const UNAVAILABLE_TARGET: &str = "unavailable target";

/// Format a memory endpoint by its (possibly unknown) kind. An unknown
/// or non-memory kind falls back to the `F:` prefix — endpoints always
/// render as some handle rather than failing the whole page.
pub(crate) fn format_memory_by_kind(
    ctx: &McpToolCtx,
    memory_id: MemoryId,
    kind: Option<EntityKind>,
) -> String {
    match kind {
        Some(EntityKind::Abstraction) => ctx.format_abstraction_memory(memory_id),
        Some(EntityKind::Perspective) => ctx.format_perspective_memory(memory_id),
        Some(EntityKind::Fact | EntityKind::Goal) | None => ctx.format_fact_memory(memory_id),
    }
}

/// Format any graph endpoint reference. `memory_kind` selects the prefix
/// for memory refs; goals carry their own shape.
pub(crate) fn format_entity_ref(
    ctx: &McpToolCtx,
    entity: &EntityRef,
    memory_kind: Option<EntityKind>,
) -> String {
    match entity {
        EntityRef::Memory(memory_id) => format_memory_by_kind(ctx, *memory_id, memory_kind),
        EntityRef::Goal(goal_id) => ctx.format_goal(*goal_id),
    }
}

/// Format an edge endpoint. The endpoint carries its own kind, so there
/// is nothing to look up and nothing to guess.
pub(crate) fn format_endpoint(ctx: &McpToolCtx, endpoint: EdgeEndpoint) -> String {
    format_entity_ref(ctx, &endpoint.entity, Some(endpoint.kind))
}

/// Format a target projection with a caller-supplied memory formatter —
/// lineage resolves memory prefixes through its per-walk class map
/// instead of the endpoint's own kind.
pub(crate) fn format_target_projection_with(
    ctx: &McpToolCtx,
    target: EdgeTargetProjection,
    format_memory: impl Fn(MemoryId) -> String,
) -> String {
    match target {
        EdgeTargetProjection::Visible { target } => match target.entity {
            EntityRef::Memory(memory_id) => format_memory(memory_id),
            EntityRef::Goal(goal_id) => ctx.format_goal(goal_id),
        },
        EdgeTargetProjection::Redacted => REDACTED_TARGET.into(),
        EdgeTargetProjection::Unavailable => UNAVAILABLE_TARGET.into(),
    }
}

/// Format a target projection using the endpoint's own kind.
pub(crate) fn format_target_projection(ctx: &McpToolCtx, target: EdgeTargetProjection) -> String {
    let memory_kind = target.endpoint().map(|endpoint| endpoint.kind);
    format_target_projection_with(ctx, target, |memory_id| {
        format_memory_by_kind(ctx, memory_id, memory_kind)
    })
}
