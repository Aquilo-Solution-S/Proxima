use crate::mcp::McpToolCtx;
use crate::mcp::core_tools::wire_ref;
use schemars::JsonSchema;
use serde::Serialize;

/// One edge touching the memory being read: the same four fields every
/// other edge-bearing output carries, minus `created_at`, which a
/// neighbor listing has no use for.
#[derive(Debug, Serialize, JsonSchema)]
pub struct NeighborEdge {
    pub source: String,
    pub target: String,
    pub kind: String,
}

pub(crate) fn neighbor_edges_from_rows(
    ctx: &McpToolCtx,
    rows: Vec<crate::Edge>,
) -> Vec<NeighborEdge> {
    rows.into_iter()
        .map(|edge| NeighborEdge {
            source: wire_ref::format_endpoint(ctx, edge.source),
            target: wire_ref::format_target_projection(ctx, edge.target),
            kind: edge.kind.as_str().to_string(),
        })
        .collect()
}
