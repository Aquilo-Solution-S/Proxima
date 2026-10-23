//! `core_think` — paged pin walk. Not search.

use crate::PinTargetKind;
use crate::mcp::ToolEffect;
use crate::query::SupersessionStatus;
use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::ancestry::{AncestorTarget, NodeStatus, UnavailableReason, Unread};
use crate::mcp::cursor as wire_cursor;
use crate::mcp::{McpTool, McpToolCtx, McpToolError};
use crate::protocol::tool as protocol_tool;
use crate::{EdgeKind, InboundPinQuery, MemoryId};

const MAX_SEEDS: usize = 8;
const MAX_DEPTH: u32 = 8;
const DEFAULT_DEPTH: u32 = 3;

#[derive(Debug, Default)]
pub struct ThinkTool;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ThinkArgs {
    #[schemars(
        length(min = 1, max = 8),
        description = "Start memories: memory UUIDs. Every direction walks from all seeds."
    )]
    pub seeds: Vec<String>,
    #[serde(default)]
    #[schemars(
        description = "ancestors (default): what the seeds were made from or are about, recursively. descendants: memories made from the seeds, recursively. episode_siblings: other memories that reference what a seed references, e.g. items bound in one core_episode_commit."
    )]
    pub direction: ThinkDirection,
    #[serde(default = "default_depth")]
    #[schemars(
        range(min = 1),
        description = "Hops to walk; values above 8 are clamped. Ignored for episode_siblings."
    )]
    pub depth: u32,
    #[serde(default = "default_limit")]
    #[schemars(description = "Visits per page, at least 1; values above 200 are clamped.")]
    pub limit: u32,
    #[serde(default)]
    #[schemars(
        description = "next_cursor from the previous page; keep seeds, direction and depth unchanged."
    )]
    pub cursor: Option<String>,
}

fn default_depth() -> u32 {
    DEFAULT_DEPTH
}

fn default_limit() -> u32 {
    super::DEFAULT_PAGE_LIMIT
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ThinkDirection {
    #[default]
    Ancestors,
    Descendants,
    EpisodeSiblings,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ThinkOutput {
    pub direction: ThinkDirection,
    pub visits: Vec<ThinkVisit>,
    pub has_more: bool,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ThinkVisit {
    pub handle: String,
    pub kind: String,
    pub sketch: String,
    pub depth: u8,
    /// Kind of the edge that first reached this visit: `origin` (made
    /// from) or `reference` (about). Absent on seeds and episode siblings.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edge: Option<EdgeKind>,
    /// Set only on a memory of another store: its store number. Such a
    /// visit is a leaf; the walk does not continue past it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub store: Option<u16>,
    /// Set only on an other-store visit that could not be read: `erased`
    /// or `unavailable`; `kind` and `sketch` are then empty.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<NodeStatus>,
    /// Why an `unavailable` visit was not read, as in
    /// `walk_memory_lineage`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<UnavailableReason>,
}

impl McpTool for ThinkTool {
    const NAME: &'static str = protocol_tool::CORE_THINK;
    const TITLE: &'static str = "Think";
    const DESCRIPTION: &'static str = "Walk provenance links from seed memories and return sketches, page by page. Not a search. Read full bodies via proxima://memory/{id}.";
    const EFFECT: Option<ToolEffect> = Some(ToolEffect::ReadOnly);
    type Args = ThinkArgs;
    type Output = ThinkOutput;

    fn call(
        ctx: McpToolCtx,
        args: ThinkArgs,
    ) -> BoxFuture<'static, Result<ThinkOutput, McpToolError>> {
        Box::pin(async move { think(ctx, args).await })
    }
}

async fn think(ctx: McpToolCtx, args: ThinkArgs) -> Result<ThinkOutput, McpToolError> {
    require_seeds(&args.seeds)?;
    let limit = super::resolve_page_limit(Some(args.limit))?;
    let seeds: Vec<MemoryId> = args
        .seeds
        .iter()
        .map(|raw| ctx.resolve_memory(raw))
        .collect::<Result<Vec<_>, _>>()?;
    let engine = ctx.require_engine()?;
    match args.direction {
        ThinkDirection::Ancestors | ThinkDirection::Descendants => {
            pin_bfs(
                &ctx,
                engine,
                WalkInput {
                    raw_seeds: &args.seeds,
                    seeds: &seeds,
                    direction: args.direction,
                    depth: u8::try_from(args.depth.clamp(1, MAX_DEPTH)).unwrap_or(8),
                    cursor: args.cursor.as_deref(),
                    limit,
                },
            )
            .await
        }
        ThinkDirection::EpisodeSiblings => {
            episode_siblings(
                &ctx,
                engine,
                &args.seeds,
                &seeds,
                args.cursor.as_deref(),
                limit,
            )
            .await
        }
    }
}

const THINK_CURSOR: wire_cursor::FingerprintedCursor = wire_cursor::FingerprintedCursor {
    version: 1,
    source: "core_think page",
    rebind_hint: "repeat the seed, direction, and depth that produced it",
};

#[derive(serde::Serialize, serde::Deserialize)]
struct ThinkPageCursor {
    depth: u8,
    t: MemoryId,
}

const HOP_PAGE: u32 = 200;

struct WalkInput<'a> {
    raw_seeds: &'a [String],
    seeds: &'a [MemoryId],
    direction: ThinkDirection,
    depth: u8,
    cursor: Option<&'a str>,
    limit: u32,
}

/// One visit of a pin walk, before hydration. A remote visit (another
/// store's memory, ancestors only) has no local kind and is a leaf.
#[derive(Debug, Clone, Copy)]
struct Visit {
    id: MemoryId,
    kind: Option<crate::EntityKind>,
    depth: u8,
    edge: Option<EdgeKind>,
}

impl Visit {
    fn is_remote(&self) -> bool {
        self.kind.is_none()
    }
}

async fn pin_bfs(
    ctx: &McpToolCtx,
    engine: &crate::Engine,
    walk: WalkInput<'_>,
) -> Result<ThinkOutput, McpToolError> {
    let fingerprint = think_fingerprint(walk.raw_seeds, walk.direction, Some(walk.depth));
    let after = walk
        .cursor
        .map(|raw| THINK_CURSOR.decode::<ThinkPageCursor>(&fingerprint, raw))
        .transpose()?;
    let seed_pins = engine.pin_nodes(&ctx.authz, walk.seeds).await?;
    if seed_pins.is_empty() {
        return Err(McpToolError::NotFound(format!(
            "memory {} not found",
            walk.raw_seeds[0]
        )));
    }
    let mut ordered: Vec<Visit> = seed_pins
        .iter()
        .map(|node| Visit {
            id: node.id,
            kind: Some(node.kind),
            depth: 0,
            edge: None,
        })
        .collect();
    ordered.sort_by_key(|row| std::cmp::Reverse(row.id));
    let mut seen: std::collections::HashSet<MemoryId> = ordered.iter().map(|row| row.id).collect();
    let mut frontier: Vec<MemoryId> = ordered.iter().map(|row| row.id).collect();
    for hop in 1..=walk.depth {
        if frontier.is_empty() {
            break;
        }
        if should_stop_walk(&ordered, after.as_ref(), walk.limit) {
            break;
        }
        // First edge (in deterministic edge order) to reach a target wins.
        let mut next: Vec<(MemoryId, bool, EdgeKind)> = Vec::new();
        for (target, remote, kind) in next_hop(engine, ctx, walk.direction, &frontier).await? {
            if seen.insert(target) {
                next.push((target, remote, kind));
            }
        }
        next.sort_by_key(|row| std::cmp::Reverse(row.0));
        if next.is_empty() {
            break;
        }
        let local: Vec<MemoryId> = next.iter().filter(|row| !row.1).map(|row| row.0).collect();
        let pins = if local.is_empty() {
            Vec::new()
        } else {
            engine.pin_nodes(&ctx.authz, &local).await?
        };
        let kinds: std::collections::HashMap<MemoryId, crate::EntityKind> =
            pins.into_iter().map(|node| (node.id, node.kind)).collect();
        for (id, remote, edge) in &next {
            let kind = if *remote {
                None
            } else if let Some(kind) = kinds.get(id) {
                Some(*kind)
            } else {
                continue;
            };
            ordered.push(Visit {
                id: *id,
                kind,
                depth: hop,
                edge: Some(*edge),
            });
        }
        frontier = local;
    }
    let start = walk_start(&ordered, after.as_ref())?;
    let rest = &ordered[start.min(ordered.len())..];
    let has_more = rest.len() > walk.limit as usize;
    let page = &rest[..rest.len().min(walk.limit as usize)];
    let next_cursor = has_more.then(|| {
        let last = page.last().expect("has_more implies a last visit");
        THINK_CURSOR.encode(
            &fingerprint,
            &ThinkPageCursor {
                depth: last.depth,
                t: last.id,
            },
        )
    });
    let visits = hydrate_visits(engine, ctx, page).await?;
    Ok(ThinkOutput {
        direction: walk.direction,
        visits,
        has_more,
        next_cursor,
    })
}

/// Render a page: local visits with their sketches, other-store visits
/// read through the port in one bounded batch.
async fn hydrate_visits(
    engine: &crate::Engine,
    ctx: &McpToolCtx,
    page: &[Visit],
) -> Result<Vec<ThinkVisit>, McpToolError> {
    let local_ids: Vec<MemoryId> = page
        .iter()
        .filter(|row| !row.is_remote())
        .map(|row| row.id)
        .collect();
    let remote_ids: Vec<uuid::Uuid> = page
        .iter()
        .filter(|row| row.is_remote())
        .map(|row| row.id.into_inner())
        .collect();
    let sketches = hydrate_sketches(engine, ctx, &local_ids).await?;
    let mut remote = super::ancestry::read_remote_targets(ctx, &remote_ids).await;
    Ok(page
        .iter()
        .map(|visit| {
            let handle = visit.id.into_inner();
            let edge = visit.edge;
            if let Some(kind) = visit.kind {
                ThinkVisit {
                    handle: handle.to_string(),
                    kind: kind.as_str().to_string(),
                    sketch: sketches
                        .iter()
                        .find(|row| row.id == visit.id)
                        .map_or_else(|| kind.as_str().to_string(), |row| row.text.clone()),
                    depth: visit.depth,
                    edge,
                    store: None,
                    status: None,
                    unavailable_reason: None,
                }
            } else {
                let read = remote
                    .remove(&handle)
                    .unwrap_or_else(|| super::ancestry::project_remote_read(None, false));
                ThinkVisit {
                    handle: handle.to_string(),
                    kind: read.kind,
                    sketch: read.snippet,
                    depth: visit.depth,
                    edge,
                    store: Some(crate::memory_store_number(handle)),
                    status: read.unread.map(Unread::status),
                    unavailable_reason: read.unread.and_then(Unread::reason),
                }
            }
        })
        .collect())
}

fn cursor_index(ordered: &[Visit], cursor: &ThinkPageCursor) -> Option<usize> {
    ordered
        .iter()
        .position(|visit| visit.depth == cursor.depth && visit.id == cursor.t)
}

fn walk_start(ordered: &[Visit], after: Option<&ThinkPageCursor>) -> Result<usize, McpToolError> {
    match after {
        None => Ok(0),
        Some(cursor) => cursor_index(ordered, cursor)
            .map(|idx| idx + 1)
            .ok_or_else(|| {
                McpToolError::from(crate::mcp::cursor::cursor_query_mismatch(
                    THINK_CURSOR.rebind_hint,
                ))
            }),
    }
}

fn should_stop_walk(ordered: &[Visit], after: Option<&ThinkPageCursor>, limit: u32) -> bool {
    match after {
        None => page_complete(ordered.len(), 0, limit),
        Some(cursor) => cursor_index(ordered, cursor)
            .is_some_and(|idx| page_complete(ordered.len(), idx + 1, limit)),
    }
}

/// True when `ordered` already has the page plus one extra visit for `has_more`.
fn page_complete(ordered_len: usize, start: usize, limit: u32) -> bool {
    ordered_len > start.saturating_add(usize::try_from(limit).unwrap_or(usize::MAX))
}

/// One hop of the walk: `(target, is another store's memory, edge kind)`.
///
/// `Ancestors` follow the one ancestor-edge rule `walk_memory_lineage`
/// also follows ([`super::ancestry::ancestor_edges`]): declared origins,
/// declared payload subjects, and the edges recorded to other stores'
/// memories, which are leaves.
///
/// `Descendants` is deliberately NOT symmetric. Its inverse question —
/// "which nodes name me in a declared subject column" — is not one `refs[]`
/// answers: an inbound `Reference` pin says a node names me in some
/// declared field, not which one, and picking out the subject columns
/// would load every such node's payload per hop. Nor are there remote
/// descendants: a store cannot see who names its memories elsewhere. The
/// asymmetry is stated here rather than hidden: descendants find what
/// pinned you as an origin, not what named you.
async fn next_hop(
    engine: &crate::Engine,
    ctx: &McpToolCtx,
    direction: ThinkDirection,
    frontier: &[MemoryId],
) -> Result<Vec<(MemoryId, bool, EdgeKind)>, McpToolError> {
    match direction {
        ThinkDirection::Ancestors => Ok(super::ancestry::ancestor_edges(engine, ctx, frontier)
            .await?
            .into_iter()
            .map(|edge| match edge.target {
                AncestorTarget::Local(id) => (id, false, edge.kind),
                AncestorTarget::Remote(id) => (MemoryId::new(id), true, edge.kind),
            })
            .collect()),
        ThinkDirection::Descendants => {
            let inbound = engine
                .inbound_pin_nodes(
                    &ctx.authz,
                    InboundPinQuery {
                        targets: frontier,
                        target_kind: PinTargetKind::Memory,
                        kind: Some(EdgeKind::Origin),
                        supersession: SupersessionStatus::IncludeSuperseded,
                        after: None,
                        limit: HOP_PAGE,
                    },
                )
                .await?;
            Ok(inbound
                .into_iter()
                .map(|node| (node.id, false, EdgeKind::Origin))
                .collect())
        }
        ThinkDirection::EpisodeSiblings => Ok(Vec::new()),
    }
}

async fn hydrate_sketches(
    engine: &crate::Engine,
    ctx: &McpToolCtx,
    ids: &[MemoryId],
) -> Result<Vec<crate::read_models::MemorySketch>, McpToolError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    Ok(engine.load_sketches(&ctx.authz, ids).await?)
}

async fn episode_siblings(
    ctx: &McpToolCtx,
    engine: &crate::Engine,
    raw_seeds: &[String],
    seeds: &[MemoryId],
    cursor: Option<&str>,
    limit: u32,
) -> Result<ThinkOutput, McpToolError> {
    let fingerprint = think_fingerprint(raw_seeds, ThinkDirection::EpisodeSiblings, None);
    let after = cursor
        .map(|raw| THINK_CURSOR.decode::<MemoryId>(&fingerprint, raw))
        .transpose()?;
    let pins = engine.pin_nodes(&ctx.authz, seeds).await?;
    let mut targets: Vec<MemoryId> = Vec::new();
    for node in &pins {
        targets.extend(node.refs.iter().copied());
    }
    targets.sort();
    targets.dedup();
    if targets.is_empty() {
        return Ok(ThinkOutput {
            direction: ThinkDirection::EpisodeSiblings,
            visits: Vec::new(),
            has_more: false,
            next_cursor: None,
        });
    }
    let inbound = engine
        .inbound_pin_nodes(
            &ctx.authz,
            InboundPinQuery {
                targets: &targets,
                target_kind: PinTargetKind::Memory,
                kind: Some(crate::EdgeKind::Reference),
                supersession: SupersessionStatus::IncludeSuperseded,
                after,
                limit: limit.saturating_add(u32::try_from(seeds.len()).unwrap_or(u32::MAX) + 1),
            },
        )
        .await?;
    let seed_set: std::collections::HashSet<MemoryId> = seeds.iter().copied().collect();
    let page: Vec<_> = inbound
        .into_iter()
        .filter(|node| !seed_set.contains(&node.id))
        .collect();
    let has_more = page.len() > limit as usize;
    let page: Vec<_> = page.into_iter().take(limit as usize).collect();
    let next_cursor = has_more.then(|| {
        let last = page.last().expect("has_more implies a last visit");
        THINK_CURSOR.encode(&fingerprint, &last.id)
    });
    let ids: Vec<MemoryId> = page.iter().map(|node| node.id).collect();
    let sketches = hydrate_sketches(engine, ctx, &ids).await?;
    let visits = page
        .into_iter()
        .map(|node| {
            let sketch = sketches
                .iter()
                .find(|row| row.id == node.id)
                .map_or_else(|| node.kind.as_str().to_string(), |row| row.text.clone());
            ThinkVisit {
                handle: node.id.into_inner().to_string(),
                kind: node.kind.as_str().to_string(),
                sketch,
                depth: 1,
                edge: None,
                store: None,
                status: None,
                unavailable_reason: None,
            }
        })
        .collect();
    Ok(ThinkOutput {
        direction: ThinkDirection::EpisodeSiblings,
        visits,
        has_more,
        next_cursor,
    })
}

fn think_fingerprint(seeds: &[String], direction: ThinkDirection, depth: Option<u8>) -> String {
    let canon =
        serde_json::to_string(&(seeds, direction, depth)).expect("fingerprint canon serializes");
    wire_cursor::fingerprint(&canon)
}

fn require_seeds(seeds: &[String]) -> Result<(), McpToolError> {
    if seeds.is_empty() {
        return Err(McpToolError::InvalidInput(
            "think requires at least one seed".into(),
        ));
    }
    if seeds.len() > MAX_SEEDS {
        return Err(McpToolError::InvalidInput(format!(
            "at most {MAX_SEEDS} seeds; got {}",
            seeds.len()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_seeds_rejected() {
        let err = require_seeds(&[]).expect_err("empty");
        assert!(err.to_string().contains("at least one seed"));
    }

    #[test]
    fn missing_think_cursor_fails_closed() {
        let missing = ThinkPageCursor {
            depth: 1,
            t: MemoryId::new(uuid::Uuid::nil()),
        };
        let err = walk_start(&[], Some(&missing)).expect_err("vanished cursor");
        assert!(err.to_string().contains("does not match"));
        assert!(!should_stop_walk(&[], Some(&missing), 1));
    }
}
