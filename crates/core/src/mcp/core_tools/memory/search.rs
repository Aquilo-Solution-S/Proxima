use crate::mcp::{McpToolCtx, McpToolError};
use crate::{MemoryId, Owner};
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct NeighborEdge {
    pub handle: String,
    pub relation: String,
    pub source: Option<String>,
    pub target: Option<String>,
}

/// # Errors
///
/// Returns storage errors from the owner-filtered edge query.
pub async fn neighbor_edges(
    ctx: &McpToolCtx,
    owner: &Owner,
    memory_ids: &[uuid::Uuid],
) -> Result<Vec<NeighborEdge>, McpToolError> {
    if memory_ids.is_empty() {
        return Ok(Vec::new());
    }
    let storage = ctx
        .storage()
        .ok_or_else(|| McpToolError::Other("engine storage unavailable".into()))?;
    let ids = memory_ids
        .iter()
        .copied()
        .map(MemoryId::new)
        .collect::<Vec<_>>();
    let rows = storage.load_neighbor_memory_edges(owner, &ids, 200).await?;

    Ok(neighbor_edges_from_rows(ctx, rows))
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
            target: row
                .target_memory_id
                .map(|id| format_memory_by_kind(ctx, id, row.target_kind)),
        })
        .collect()
}

fn format_memory_by_kind(ctx: &McpToolCtx, memory_id: MemoryId, kind: crate::EntityKind) -> String {
    match kind {
        crate::EntityKind::Abstraction => ctx.format_abstraction_memory(memory_id),
        crate::EntityKind::Perspective => ctx.format_perspective_memory(memory_id),
        _ => ctx.format_fact_memory(memory_id),
    }
}
