//! `core/walk_memory_lineage` — wire-facing memory lineage walk.

use std::collections::HashMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::mcp::cursor as wire_cursor;
use crate::mcp::{McpToolCtx, McpToolError};
use crate::verbs::query::{MemoryLineageCursor, MemoryLineageDirection, MemoryLineageRequest};
use crate::{MemoryHandleClass, MemoryId};

use super::get_memory::memory_class;

const MAX_LINEAGE_DEPTH: u32 = 8;
const DEFAULT_LINEAGE_DEPTH: u32 = 3;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WalkMemoryLineageArgs {
    /// `F:<uuid>`, `A:<uuid>`, or `P:<uuid>` memory id.
    pub memory: String,
    /// Walk direction: `ancestors` follows `origin` edges toward what
    /// this memory was made from (default); `descendants` follows them
    /// toward what was made from it.
    #[serde(default = "default_direction")]
    pub direction: WalkMemoryLineageDirectionArg,
    /// Maximum hop distance from the start memory; clamped to 1..=8,
    /// default 3.
    #[serde(default = "default_depth")]
    pub depth: u32,
    /// Max lineage edges per page; values above 200 are clamped, 0 is
    /// rejected, default 50.
    #[serde(default = "default_limit")]
    pub limit: u32,
    /// Opaque pagination cursor from a previous response's `next_cursor`.
    /// The memory, direction, and depth must stay unchanged between
    /// pages; `limit` may vary.
    #[serde(default)]
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum WalkMemoryLineageDirectionArg {
    /// Toward what this memory was derived from.
    Ancestors,
    /// Toward what was derived from this memory.
    Descendants,
}

impl From<WalkMemoryLineageDirectionArg> for MemoryLineageDirection {
    fn from(value: WalkMemoryLineageDirectionArg) -> Self {
        match value {
            WalkMemoryLineageDirectionArg::Ancestors => Self::Ancestors,
            WalkMemoryLineageDirectionArg::Descendants => Self::Descendants,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct WalkMemoryLineageOutput {
    pub start: String,
    pub direction: String,
    pub nodes: Vec<LineageNodeOutput>,
    pub edges: Vec<LineageEdgeOutput>,
    /// More lineage edges exist beyond this page; `true` iff
    /// `next_cursor` is present. Named `has_more` to match every other
    /// paginated surface.
    pub has_more: bool,
    /// Opaque cursor for the next page of lineage edges; absent on the
    /// last page. Pass back as `cursor` with the same memory, direction,
    /// and depth.
    pub next_cursor: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct LineageNodeOutput {
    pub memory: String,
    pub kind: String,
    pub schema_id: String,
    pub snippet: String,
    pub distance: u8,
}

/// One hop of the walk. Lineage traverses `origin` and nothing else, so
/// every edge here carries that kind; it is reported anyway so the shape
/// matches every other edge-bearing output.
#[derive(Debug, Serialize)]
pub struct LineageEdgeOutput {
    pub source: String,
    pub target: String,
    pub kind: String,
    pub distance: u8,
}

fn default_direction() -> WalkMemoryLineageDirectionArg {
    WalkMemoryLineageDirectionArg::Ancestors
}

fn default_depth() -> u32 {
    DEFAULT_LINEAGE_DEPTH
}

fn default_limit() -> u32 {
    super::DEFAULT_PAGE_LIMIT
}

/// Opaque cursor codec: the shared `{v, fp, c}` envelope with the
/// storage keyset position under `c`. The fingerprint binds the memory,
/// direction, and effective depth; `limit` stays out so it may vary
/// between pages.
const LINEAGE_CURSOR: wire_cursor::FingerprintedCursor = wire_cursor::FingerprintedCursor {
    version: 1,
    source: "proxima://memory/{id}/lineage page",
    rebind_hint: "repeat the memory, direction, and depth that produced it",
};

fn lineage_fingerprint(memory: &str, direction: MemoryLineageDirection, depth: u8) -> String {
    let canon = serde_json::to_string(&(memory, format!("{direction:?}"), depth))
        .expect("fingerprint canon serializes");
    wire_cursor::fingerprint(&canon)
}

/// # Errors
///
/// Returns invalid memory references, storage, or projection failures.
pub async fn walk_memory_lineage(
    ctx: McpToolCtx,
    args: WalkMemoryLineageArgs,
) -> Result<WalkMemoryLineageOutput, McpToolError> {
    let start = ctx.resolve_memory(&args.memory)?;
    crate::reject_zero_limit(Some(args.limit))?;
    let direction = MemoryLineageDirection::from(args.direction);
    let depth = u8::try_from(args.depth.clamp(1, MAX_LINEAGE_DEPTH)).unwrap_or(8);
    let fingerprint = lineage_fingerprint(&args.memory, direction, depth);
    let after: Option<MemoryLineageCursor> = args
        .cursor
        .as_deref()
        .map(|raw| LINEAGE_CURSOR.decode(&fingerprint, raw))
        .transpose()?;
    let engine = ctx.require_engine()?;
    let response = engine
        .walk_memory_lineage(
            &ctx.authz,
            &MemoryLineageRequest {
                owner: ctx.owner,
                start_memory_id: start,
                direction,
                depth,
                limit: args.limit.min(super::MAX_PAGE_LIMIT),
                after,
            },
        )
        .await?;

    // A visible start memory always projects itself as a distance-0 node,
    // even with no lineage edges; an empty node set therefore means the
    // start does not exist or is not visible (deliberately
    // indistinguishable) — surface that instead of an empty walk.
    if response.nodes.is_empty() {
        return Err(McpToolError::NotFound(format!(
            "memory {} not found",
            args.memory
        )));
    }

    let mut classes = HashMap::new();
    let nodes = response
        .nodes
        .into_iter()
        .map(|node| {
            let class = memory_class(node.kind)?;
            classes.insert(node.memory_id, class);
            Ok(LineageNodeOutput {
                memory: ctx.format_memory_with_class(node.memory_id, class),
                kind: node.kind.as_str().to_string(),
                schema_id: node.schema_id.as_str().to_string(),
                snippet: node.snippet,
                distance: node.distance,
            })
        })
        .collect::<Result<Vec<_>, McpToolError>>()?;

    let edges = response
        .edges
        .into_iter()
        .map(|hop| LineageEdgeOutput {
            source: format_lineage_endpoint(&ctx, &classes, hop.edge.source),
            target: format_lineage_target(&ctx, &classes, hop.edge.target),
            kind: hop.edge.kind.as_str().to_string(),
            distance: hop.distance,
        })
        .collect();

    let next_cursor = response
        .next_cursor
        .map(|cursor| LINEAGE_CURSOR.encode(&fingerprint, &cursor));
    // Derived from the cursor, not the verb's separate `truncated` flag,
    // so the documented "`has_more` iff `next_cursor`" invariant holds by
    // construction — the same derivation every other paginated surface
    // uses.
    let has_more = next_cursor.is_some();
    Ok(WalkMemoryLineageOutput {
        start: args.memory,
        direction: format!("{direction:?}").to_lowercase(),
        nodes,
        edges,
        has_more,
        next_cursor,
    })
}

fn format_lineage_target(
    ctx: &McpToolCtx,
    classes: &HashMap<MemoryId, MemoryHandleClass>,
    target: crate::EdgeTargetProjection,
) -> String {
    // Memory prefixes come from the per-walk class map (built off the
    // node projection) rather than a single kind, so this goes through
    // the closure variant of the shared formatter.
    super::wire_ref::format_target_projection_with(ctx, target, |memory_id| {
        let class = classes
            .get(&memory_id)
            .copied()
            .unwrap_or(MemoryHandleClass::Fact);
        ctx.format_memory_with_class(memory_id, class)
    })
}

fn format_lineage_endpoint(
    ctx: &McpToolCtx,
    classes: &HashMap<MemoryId, MemoryHandleClass>,
    endpoint: crate::EdgeEndpoint,
) -> String {
    let Some(memory_id) = endpoint.memory_id() else {
        return super::wire_ref::format_endpoint(ctx, endpoint);
    };
    let class = classes
        .get(&memory_id)
        .copied()
        .unwrap_or_else(|| memory_class(endpoint.kind).unwrap_or(MemoryHandleClass::Fact));
    ctx.format_memory_with_class(memory_id, class)
}

#[cfg(test)]
mod tests {
    use super::{LINEAGE_CURSOR, lineage_fingerprint};
    use crate::verbs::query::{MemoryLineageCursor, MemoryLineageDirection};

    /// The cursor binds memory, direction, and depth; any of the three
    /// changing between pages fails closed. `limit` stays out of the
    /// fingerprint so it may vary.
    #[test]
    fn lineage_cursor_binds_memory_direction_and_depth() {
        let memory = "F:018f0000-0000-7000-8000-000000000001";
        let fingerprint = lineage_fingerprint(memory, MemoryLineageDirection::Ancestors, 3);
        let cursor = MemoryLineageCursor {
            distance: 2,
            source: crate::EntityRef::Memory(crate::MemoryId::new(uuid::Uuid::now_v7())),
            target: crate::EntityRef::Memory(crate::MemoryId::new(uuid::Uuid::now_v7())),
        };
        let token = LINEAGE_CURSOR.encode(&fingerprint, &cursor);
        let decoded: MemoryLineageCursor = LINEAGE_CURSOR
            .decode(&fingerprint, &token)
            .expect("round trip");
        assert_eq!(decoded, cursor);

        for other in [
            lineage_fingerprint(memory, MemoryLineageDirection::Descendants, 3),
            lineage_fingerprint(memory, MemoryLineageDirection::Ancestors, 4),
            lineage_fingerprint(
                "F:018f0000-0000-7000-8000-000000000002",
                MemoryLineageDirection::Ancestors,
                3,
            ),
        ] {
            let err = LINEAGE_CURSOR
                .decode::<MemoryLineageCursor>(&other, &token)
                .expect_err("changed walk shape must fail closed");
            assert!(err.into_message().starts_with("cursor does not match"));
        }
    }
}
