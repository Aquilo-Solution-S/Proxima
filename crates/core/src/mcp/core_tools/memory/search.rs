use crate::change_event::{EdgeTargetProjection, EntityRef};
use crate::mcp::McpToolCtx;
use crate::{EntityKind, MemoryId};
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct NeighborEdge {
    pub handle: String,
    pub relation: String,
    pub source: Option<String>,
    pub target: Option<String>,
}

pub(crate) fn neighbor_edges_from_rows(
    ctx: &McpToolCtx,
    rows: Vec<crate::storage::NeighborEdgeRow>,
) -> Vec<NeighborEdge> {
    rows.into_iter()
        .map(|row| NeighborEdge {
            handle: ctx.format_edge(row.edge_id),
            relation: row.relation,
            source: row
                .source_memory_id
                .map(|id| format_memory_by_kind(ctx, id, row.source_kind)),
            target: Some(format_target_projection(
                ctx,
                row.target,
                row.target_memory_kind,
            )),
        })
        .collect()
}

fn format_target_projection(
    ctx: &McpToolCtx,
    target: EdgeTargetProjection,
    target_memory_kind: Option<EntityKind>,
) -> String {
    match target {
        EdgeTargetProjection::Visible {
            target: EntityRef::Memory(memory_id),
        } => format_memory_by_kind(
            ctx,
            memory_id,
            target_memory_kind.unwrap_or(EntityKind::Fact),
        ),
        EdgeTargetProjection::Visible {
            target: EntityRef::Goal(goal_id),
        } => ctx.format_goal(goal_id),
        EdgeTargetProjection::Visible {
            target: EntityRef::FactEntity(fact_entity_id),
        } => format!("fact_entity:{}", fact_entity_id.into_inner()),
        EdgeTargetProjection::Redacted => "redacted target".into(),
        EdgeTargetProjection::Unavailable => "unavailable target".into(),
    }
}

fn format_memory_by_kind(ctx: &McpToolCtx, memory_id: MemoryId, kind: EntityKind) -> String {
    match kind {
        EntityKind::Abstraction => ctx.format_abstraction_memory(memory_id),
        EntityKind::Perspective => ctx.format_perspective_memory(memory_id),
        _ => ctx.format_fact_memory(memory_id),
    }
}
