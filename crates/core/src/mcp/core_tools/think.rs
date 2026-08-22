//! `core_think` — paged pin walk. Not search.

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::flavor::Provenance;
use crate::mcp::cursor as wire_cursor;
use crate::mcp::{McpTool, McpToolCtx, McpToolError};
use crate::protocol::tool as protocol_tool;
use crate::{EdgeKind, GetMemoriesReadRequest, InboundPinQuery, MemoryHandleClass, MemoryId};

const MAX_SEEDS: usize = 8;
const MAX_DEPTH: u32 = 8;
const DEFAULT_DEPTH: u32 = 3;

#[derive(Debug, Default)]
pub struct ThinkTool;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ThinkArgs {
    #[schemars(
        length(min = 1, max = 8),
        description = "Seed handles (`F:`/`A:`/`P:`). Ancestors/descendants walk the first seed; siblings use all seeds."
    )]
    pub seeds: Vec<String>,
    #[serde(default)]
    pub direction: ThinkDirection,
    #[serde(default = "default_depth")]
    #[schemars(range(min = 1), description = "Hop depth 1..=8, default 3.")]
    pub depth: u32,
    #[serde(default = "default_limit")]
    pub limit: u32,
    #[serde(default)]
    pub cursor: Option<String>,
}

fn default_depth() -> u32 {
    DEFAULT_DEPTH
}

fn default_limit() -> u32 {
    super::DEFAULT_PAGE_LIMIT
}

#[derive(Debug, Clone, Copy, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ThinkDirection {
    #[default]
    Ancestors,
    Descendants,
    EpisodeSiblings,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ThinkOutput {
    pub direction: String,
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
}

impl McpTool for ThinkTool {
    const NAME: &'static str = protocol_tool::CORE_THINK;
    const DESCRIPTION: &'static str = "Paged graph walk from seeds. Directions: ancestors, descendants, episode_siblings. No ANN. Hydrate bodies separately via proxima://memory/{id}. Cursor pages, not a live stream.";
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
    crate::reject_zero_limit(Some(args.limit))?;
    let seeds: Vec<MemoryId> = args
        .seeds
        .iter()
        .map(|raw| ctx.resolve_memory(raw))
        .collect::<Result<Vec<_>, _>>()?;
    let engine = ctx.require_engine()?;
    let limit = args.limit.min(super::MAX_PAGE_LIMIT);
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
    let mut ordered: Vec<(MemoryId, crate::EntityKind, u8)> = seed_pins
        .iter()
        .map(|node| (node.id, node.kind, 0))
        .collect();
    ordered.sort_by_key(|row| std::cmp::Reverse(row.0));
    let mut seen: std::collections::HashSet<MemoryId> = ordered.iter().map(|row| row.0).collect();
    let mut frontier: Vec<MemoryId> = ordered.iter().map(|row| row.0).collect();
    for hop in 1..=walk.depth {
        if frontier.is_empty() {
            break;
        }
        if should_stop_walk(&ordered, after.as_ref(), walk.limit) {
            break;
        }
        let next = next_hop(engine, ctx, walk.direction, &frontier).await?;
        let mut next: Vec<MemoryId> = next.into_iter().filter(|id| seen.insert(*id)).collect();
        next.sort_by_key(|id| std::cmp::Reverse(*id));
        if next.is_empty() {
            break;
        }
        let pins = engine.pin_nodes(&ctx.authz, &next).await?;
        let kinds: std::collections::HashMap<MemoryId, crate::EntityKind> =
            pins.into_iter().map(|node| (node.id, node.kind)).collect();
        for id in &next {
            if let Some(kind) = kinds.get(id) {
                ordered.push((*id, *kind, hop));
            }
        }
        frontier = next;
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
                depth: last.2,
                t: last.0,
            },
        )
    });
    let ids: Vec<MemoryId> = page.iter().map(|row| row.0).collect();
    let sketches = hydrate_sketches(engine, ctx, &ids).await?;
    let visits = page
        .iter()
        .map(|(id, kind, hop)| {
            let class = super::get_memory::memory_class(*kind).unwrap_or(MemoryHandleClass::Fact);
            let sketch = sketches
                .iter()
                .find(|row| row.id == *id)
                .map_or_else(|| kind.as_str().to_string(), |row| row.text.clone());
            ThinkVisit {
                handle: ctx.format_memory_with_class(*id, class),
                kind: kind.as_str().to_string(),
                sketch,
                depth: *hop,
            }
        })
        .collect();
    Ok(ThinkOutput {
        direction: direction_name(walk.direction),
        visits,
        has_more,
        next_cursor,
    })
}

fn cursor_index(
    ordered: &[(MemoryId, crate::EntityKind, u8)],
    cursor: &ThinkPageCursor,
) -> Option<usize> {
    ordered
        .iter()
        .position(|(id, _, hop)| *hop == cursor.depth && *id == cursor.t)
}

fn walk_start(
    ordered: &[(MemoryId, crate::EntityKind, u8)],
    after: Option<&ThinkPageCursor>,
) -> Result<usize, McpToolError> {
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

fn should_stop_walk(
    ordered: &[(MemoryId, crate::EntityKind, u8)],
    after: Option<&ThinkPageCursor>,
    limit: u32,
) -> bool {
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

/// One hop of the lineage walk, and the place `Provenance` is spent.
///
/// The declaration answers "how is this node's provenance REACHABLE", and
/// until Phase 4 the walk did not ask: `Ancestors` expanded `origins[]` for
/// every node, which is right for the two thirds of the vocabulary that
/// write origins and wrong for the third that does not. A
/// `PayloadOnly` node has an EMPTY `origins[]` by construction — its
/// subjects live in payload columns the array never sees — so an
/// interpretation was a lineage dead end, which is exactly the defect plan
/// checkpoint 9 named: *"`core_think`'s edge-walk never traverses
/// interpretation→subject"*.
///
/// The three arms, and what each costs:
///
/// - `OriginEdges` — expand `origins[]`. Today's behaviour, unchanged.
/// - `PayloadOnly { subject_columns }` — load the node's payload and take
///   the references whose declared FIELD is one of the named columns. No
///   new statement and no new port: `SidecarPayload::references()` already
///   carries the field name each reference came from, and
///   `subject_columns` is what picks the grounding ones out of the rest.
/// - `None` — nothing. The row cannot have origins anyway when it is a
///   Fact (`memory_fact_origins_chk`), and when it is not, the declaration
///   is the claim that it derives from nothing.
///
/// A schema no contract declares is treated as `OriginEdges`. That is a
/// deliberate fail-OPEN: a registration without a contract must not make
/// its memories invisible to the walk, and showing a pin whose declaration
/// nobody wrote is a smaller wrong than hiding one.
///
/// `Descendants` is deliberately NOT symmetric under the gate. Its inverse
/// question — "which nodes name me in a declared subject column" — has no
/// index to answer it: `_references` is discarded at ingest
/// (`storage-pg/src/verbs/fact_ingest.rs`), so there is no reference table,
/// and `interpretation_v1.subject_memory_ids` carries no GIN index. It
/// would be a sequential scan per hop. The asymmetry is stated here rather
/// than hidden: descendants find what pinned you, not what named you.
async fn next_hop(
    engine: &crate::Engine,
    ctx: &McpToolCtx,
    direction: ThinkDirection,
    frontier: &[MemoryId],
) -> Result<Vec<MemoryId>, McpToolError> {
    match direction {
        ThinkDirection::Ancestors => {
            let pins = engine.pin_nodes(&ctx.authz, frontier).await?;
            let mut ancestors = Vec::new();
            let mut grounded: Vec<(MemoryId, &'static [&'static str])> = Vec::new();
            for node in pins {
                match ctx.registry.provenance_of(&node.schema_id, node.kind) {
                    None | Some(Provenance::OriginEdges) => ancestors.extend(node.origins),
                    Some(Provenance::PayloadOnly { subject_columns }) => {
                        grounded.push((node.id, subject_columns));
                    }
                    Some(Provenance::None) => {}
                }
            }
            ancestors.extend(payload_subjects(engine, ctx, &grounded).await?);
            Ok(ancestors)
        }
        ThinkDirection::Descendants => {
            let inbound = engine
                .inbound_pin_nodes(
                    &ctx.authz,
                    InboundPinQuery {
                        targets: frontier,
                        kind: Some(EdgeKind::Origin),
                        heads_only: false,
                        after: None,
                        limit: HOP_PAGE,
                    },
                )
                .await?;
            Ok(inbound.into_iter().map(|node| node.id).collect())
        }
        ThinkDirection::EpisodeSiblings => Ok(Vec::new()),
    }
}

/// The subjects a `PayloadOnly` node grounds in, read through the payload
/// fields its schema declares.
///
/// `get_memories` is the same owner-scoped read `core_get_memories` uses,
/// so a subject the caller may not see is not returned and the walk simply
/// does not reach it — the ordinary shape of an absent node, not a redaction.
async fn payload_subjects(
    engine: &crate::Engine,
    ctx: &McpToolCtx,
    grounded: &[(MemoryId, &'static [&'static str])],
) -> Result<Vec<MemoryId>, McpToolError> {
    if grounded.is_empty() {
        return Ok(Vec::new());
    }
    let response = engine
        .get_memories(
            &ctx.authz,
            &GetMemoriesReadRequest {
                memory_ids: grounded.iter().map(|(id, _)| *id).collect(),
            },
        )
        .await?;
    let declared: std::collections::HashMap<MemoryId, &'static [&'static str]> =
        grounded.iter().copied().collect();
    let mut subjects = Vec::new();
    for snapshot in response.memories {
        let (Some(columns), Some(payload)) =
            (declared.get(&snapshot.memory_id), snapshot.payload.as_ref())
        else {
            continue;
        };
        for reference in payload.references() {
            if columns.contains(&reference.field)
                && let Some(id) = reference.target.memory_id()
            {
                subjects.push(id);
            }
        }
    }
    Ok(subjects)
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
            direction: direction_name(ThinkDirection::EpisodeSiblings),
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
                kind: Some(crate::EdgeKind::Reference),
                heads_only: false,
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
            let class =
                super::get_memory::memory_class(node.kind).unwrap_or(MemoryHandleClass::Fact);
            let sketch = sketches
                .iter()
                .find(|row| row.id == node.id)
                .map_or_else(|| node.kind.as_str().to_string(), |row| row.text.clone());
            ThinkVisit {
                handle: ctx.format_memory_with_class(node.id, class),
                kind: node.kind.as_str().to_string(),
                sketch,
                depth: 1,
            }
        })
        .collect();
    Ok(ThinkOutput {
        direction: direction_name(ThinkDirection::EpisodeSiblings),
        visits,
        has_more,
        next_cursor,
    })
}

fn think_fingerprint(seeds: &[String], direction: ThinkDirection, depth: Option<u8>) -> String {
    let canon = serde_json::to_string(&(seeds, direction_name(direction), depth))
        .expect("fingerprint canon serializes");
    wire_cursor::fingerprint(&canon)
}

fn direction_name(direction: ThinkDirection) -> String {
    match direction {
        ThinkDirection::Ancestors => "ancestors".into(),
        ThinkDirection::Descendants => "descendants".into(),
        ThinkDirection::EpisodeSiblings => "episode_siblings".into(),
    }
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
    fn direction_names_are_stable() {
        assert_eq!(direction_name(ThinkDirection::Ancestors), "ancestors");
        assert_eq!(
            direction_name(ThinkDirection::EpisodeSiblings),
            "episode_siblings"
        );
    }

    #[test]
    fn page_complete_stops_after_limit_plus_one() {
        assert!(!page_complete(1, 0, 1));
        assert!(page_complete(2, 0, 1));
        assert!(!page_complete(8, 0, 8));
        assert!(page_complete(9, 0, 8));
        assert!(!page_complete(5, 4, 1));
        assert!(page_complete(6, 4, 1));
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
