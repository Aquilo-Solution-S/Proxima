use crate::mcp::McpToolCtx;
use crate::mcp::core_tools::wire_ref;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct NeighborEdge {
    /// `E:<uuid>` edge reference. Named `edge` to match every other
    /// edge-bearing output (`core_read_edges`, lineage, change events);
    /// this field was `handle` before v0.0.7.
    pub edge: String,
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
            edge: ctx.format_edge(row.edge_id),
            relation: row.relation,
            source: row
                .source_memory_id
                .map(|id| wire_ref::format_memory_by_kind(ctx, id, Some(row.source_kind))),
            target: Some(wire_ref::format_target_projection(
                ctx,
                &row.target,
                row.target_memory_kind,
            )),
        })
        .collect()
}
