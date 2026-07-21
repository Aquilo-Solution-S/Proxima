//! Shared wire formatting for graph endpoints. One place turns an
//! [`EntityRef`] or [`EdgeTargetProjection`] into a prefixed wire handle,
//! so the kind→prefix mapping and the redaction sentinels cannot drift
//! between the edge-bearing read tools (edges, change events, search
//! neighbor edges, lineage).

use crate::mcp::McpToolCtx;
use crate::{EdgeTargetProjection, EntityKind, EntityRef, MemoryId};

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
/// for memory refs; goals and fact entities carry their own shapes.
pub(crate) fn format_entity_ref(
    ctx: &McpToolCtx,
    entity: &EntityRef,
    memory_kind: Option<EntityKind>,
) -> String {
    match entity {
        EntityRef::Memory(memory_id) => format_memory_by_kind(ctx, *memory_id, memory_kind),
        EntityRef::Goal(goal_id) => ctx.format_goal(*goal_id),
        EntityRef::FactEntity(fact_entity_id) => {
            format!("fact_entity:{}", fact_entity_id.into_inner())
        }
    }
}

/// Format a target projection with a caller-supplied memory formatter —
/// lineage resolves memory prefixes through its per-walk class map
/// instead of a single kind.
pub(crate) fn format_target_projection_with(
    ctx: &McpToolCtx,
    target: &EdgeTargetProjection,
    format_memory: impl Fn(MemoryId) -> String,
) -> String {
    match target {
        EdgeTargetProjection::Visible {
            target: EntityRef::Memory(memory_id),
        } => format_memory(*memory_id),
        EdgeTargetProjection::Visible {
            target: EntityRef::Goal(goal_id),
        } => ctx.format_goal(*goal_id),
        EdgeTargetProjection::Visible {
            target: EntityRef::FactEntity(fact_entity_id),
        } => format!("fact_entity:{}", fact_entity_id.into_inner()),
        EdgeTargetProjection::Redacted => REDACTED_TARGET.into(),
        EdgeTargetProjection::Unavailable => UNAVAILABLE_TARGET.into(),
    }
}

/// Format a target projection whose memory kind (when known) picks the
/// handle prefix.
pub(crate) fn format_target_projection(
    ctx: &McpToolCtx,
    target: &EdgeTargetProjection,
    memory_kind: Option<EntityKind>,
) -> String {
    format_target_projection_with(ctx, target, |memory_id| {
        format_memory_by_kind(ctx, memory_id, memory_kind)
    })
}
