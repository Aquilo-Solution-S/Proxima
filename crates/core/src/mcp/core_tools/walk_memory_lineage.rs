//! `core/walk_memory_lineage` — wire-facing memory lineage walk.

use std::collections::{HashMap, HashSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::ancestry::{AncestorEdge, AncestorTarget};
use crate::MemoryId;
use crate::mcp::cursor as wire_cursor;
use crate::mcp::{McpToolCtx, McpToolError};
use crate::verbs::query::{MemoryLineageCursor, MemoryLineageDirection, MemoryLineageRequest};

const MAX_LINEAGE_DEPTH: u32 = 8;
const DEFAULT_LINEAGE_DEPTH: u32 = 3;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WalkMemoryLineageArgs {
    /// Memory UUID.
    pub memory: String,
    /// Walk direction: `ancestors` walks toward what this memory was made
    /// from or is about (default), the edges `core_think` follows:
    /// `origin`, declared subjects as `reference`, and other stores'
    /// memories as leaves; `descendants` follows `origin` edges toward
    /// what was made from it.
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
    /// Hops from the start, which is 0: the level at which the walk first
    /// reached this memory, in either direction and on every page, as on
    /// its edges.
    pub distance: u8,
    /// Set only on a node of another store: its store number.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub store: Option<u16>,
    /// Set only on an other-store node that could not be read: `erased`
    /// (the store erased it) or `unavailable` (no port, the store did not
    /// answer, or the caller may no longer read it). The walk never fails
    /// on such a node.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// Why an `unavailable` origin was not read: `not readable` (the store
    /// answered that the caller cannot read it, or there is no such store),
    /// `store unavailable` (the store failed; retry later), `deadline
    /// exceeded`, or `origin limit exceeded` (the page cap).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<String>,
}

/// One hop of the walk: `origin` (made from) or, toward ancestors,
/// `reference` (a declared subject, e.g. an interpretation's). An edge to
/// another store's memory carries its recorded kind (#480).
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
    let limit = args.limit.min(super::MAX_PAGE_LIMIT);
    if matches!(direction, MemoryLineageDirection::Ancestors) {
        let page = walk_ancestors(&ctx, engine, start, depth, limit, after).await?;
        let Some(page) = page else {
            return Err(McpToolError::NotFound(format!(
                "memory {} not found",
                args.memory
            )));
        };
        let next_cursor = page
            .next_cursor
            .map(|cursor| LINEAGE_CURSOR.encode(&fingerprint, &cursor));
        let has_more = next_cursor.is_some();
        return Ok(WalkMemoryLineageOutput {
            start: args.memory,
            direction: format!("{direction:?}").to_lowercase(),
            nodes: page.nodes,
            edges: page.edges,
            has_more,
            next_cursor,
        });
    }
    let response = engine
        .walk_memory_lineage(
            &ctx.authz,
            &MemoryLineageRequest {
                owner: ctx.owner,
                start_memory_id: start,
                direction,
                depth,
                limit,
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

    let nodes = response
        .nodes
        .into_iter()
        .map(|node| {
            Ok(LineageNodeOutput {
                memory: node.memory_id.into_inner().to_string(),
                kind: node.kind.as_str().to_string(),
                schema_id: node.schema_id.as_str().to_string(),
                snippet: node.snippet,
                distance: node.distance,
                store: None,
                status: None,
                unavailable_reason: None,
            })
        })
        .collect::<Result<Vec<_>, McpToolError>>()?;

    let edges: Vec<LineageEdgeOutput> = response
        .edges
        .into_iter()
        .map(|hop| LineageEdgeOutput {
            source: super::wire_ref::format_endpoint(&ctx, hop.edge.source),
            target: super::wire_ref::format_target_projection(&ctx, hop.edge.target),
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

/// One page of an ancestors walk; `None` when the start is not readable.
struct AncestorPage {
    nodes: Vec<LineageNodeOutput>,
    edges: Vec<LineageEdgeOutput>,
    next_cursor: Option<MemoryLineageCursor>,
}

/// One edge on an ancestors page, `distance` hops from the start.
#[derive(Debug, Clone, Copy)]
struct AncestorHop {
    edge: AncestorEdge,
    distance: u8,
}

/// Where a page resumes: the hop distance and, within it, the keyset
/// `(source, target)` the previous page ended on (edges are listed newest
/// `(source, target)` first). A cursor whose ends are not memories (none is
/// ever issued) resumes at the next distance.
fn resume_point(after: Option<MemoryLineageCursor>) -> (u8, Option<(uuid::Uuid, uuid::Uuid)>) {
    let Some(after) = after else {
        return (1, None);
    };
    match (after.source, after.target) {
        (crate::EntityRef::Memory(src), crate::EntityRef::Memory(tgt)) => (
            after.distance.max(1),
            Some((src.into_inner(), tgt.into_inner())),
        ),
        _ => (after.distance.saturating_add(1).max(1), None),
    }
}

/// The ancestors of `start`, level by level over the ancestor-edge rule
/// `core_think` follows ([`super::ancestry::ancestor_edges`], #480):
/// declared origins, declared payload subjects (`reference`), and the
/// recorded edges to other stores' memories, which are leaves.
///
/// Every edge of a level is listed, newest `(source, target)` first; only
/// unseen local targets are expanded. A page holds at most `limit` edges,
/// other-store ones included, and resumes on the same keyset
/// `(distance, source, target)` the storage walk used, so its cursor shape
/// is unchanged.
async fn walk_ancestors(
    ctx: &McpToolCtx,
    engine: &crate::Engine,
    start: MemoryId,
    depth: u8,
    limit: u32,
    after: Option<MemoryLineageCursor>,
) -> Result<Option<AncestorPage>, McpToolError> {
    let page_len = usize::try_from(limit.min(super::MAX_PAGE_LIMIT)).unwrap_or(usize::MAX);
    if engine.lineage_nodes(&ctx.authz, &[start]).await?.is_empty() {
        return Ok(None);
    }
    let (start_distance, keyset) = resume_point(after);
    let mut hops: Vec<AncestorHop> = Vec::new();
    // Hop distance at which each local memory is first reached; the start
    // is 0. A memory is expanded once, from its first level.
    let mut reached: HashMap<MemoryId, u8> = HashMap::from([(start, 0)]);
    let mut frontier = vec![start];
    'walk: for distance in 1..=depth {
        if frontier.is_empty() {
            break;
        }
        let level = super::ancestry::ancestor_edges(engine, ctx, &frontier).await?;
        let mut next: Vec<MemoryId> = Vec::new();
        for edge in &level {
            if let AncestorTarget::Local(id) = edge.target
                && !reached.contains_key(&id)
            {
                reached.insert(id, distance);
                next.push(id);
            }
        }
        if distance >= start_distance {
            for edge in &level {
                if distance == start_distance
                    && let Some(position) = keyset
                    && (edge.source.into_inner(), edge.target.uuid()) >= position
                {
                    continue;
                }
                hops.push(AncestorHop {
                    edge: *edge,
                    distance,
                });
                if hops.len() > page_len {
                    break 'walk;
                }
            }
        }
        next.sort_by_key(|id| std::cmp::Reverse(*id));
        frontier = next;
    }
    let truncated = hops.len() > page_len;
    hops.truncate(page_len);
    let next_cursor = truncated.then(|| {
        let last = hops.last().expect("truncated page is non-empty");
        MemoryLineageCursor {
            distance: last.distance,
            source: crate::EntityRef::Memory(last.edge.source),
            target: crate::EntityRef::Memory(MemoryId::new(last.edge.target.uuid())),
        }
    });
    let (nodes, edges) = render_ancestor_page(ctx, engine, start, &hops, &reached).await?;
    Ok(Some(AncestorPage {
        nodes,
        edges,
        next_cursor,
    }))
}

/// Nodes and edges of an ancestors page. Local nodes carry the hop
/// distance at which the walk first reached them (`reached`; the start is
/// 0), as edges and other-store nodes do; an unreadable local target is a
/// redacted edge end. Other-store targets are read in one
/// bounded batch and placed at their edge's distance.
async fn render_ancestor_page(
    ctx: &McpToolCtx,
    engine: &crate::Engine,
    start: MemoryId,
    hops: &[AncestorHop],
    reached: &HashMap<MemoryId, u8>,
) -> Result<(Vec<LineageNodeOutput>, Vec<LineageEdgeOutput>), McpToolError> {
    let mut local_ids = vec![start];
    for hop in hops {
        local_ids.push(hop.edge.source);
        if let AncestorTarget::Local(id) = hop.edge.target {
            local_ids.push(id);
        }
    }
    let mut unique = HashSet::new();
    local_ids.retain(|id| unique.insert(*id));
    let mut nodes: Vec<LineageNodeOutput> = engine
        .lineage_nodes(&ctx.authz, &local_ids)
        .await?
        .into_iter()
        .map(|node| LineageNodeOutput {
            memory: node.memory_id.into_inner().to_string(),
            kind: node.kind.as_str().to_string(),
            schema_id: node.schema_id.as_str().to_string(),
            snippet: node.snippet,
            distance: reached.get(&node.memory_id).copied().unwrap_or_default(),
            store: None,
            status: None,
            unavailable_reason: None,
        })
        .collect();
    let visible: HashSet<String> = nodes.iter().map(|node| node.memory.clone()).collect();
    let mut edges = Vec::with_capacity(hops.len());
    let mut external = Vec::new();
    for hop in hops {
        let source = hop.edge.source.into_inner().to_string();
        if !visible.contains(&source) {
            continue;
        }
        match hop.edge.target {
            AncestorTarget::Local(id) => {
                let handle = id.into_inner().to_string();
                let target = if visible.contains(&handle) {
                    handle
                } else {
                    super::wire_ref::format_target_projection(
                        ctx,
                        crate::EdgeTargetProjection::Redacted,
                    )
                };
                edges.push(LineageEdgeOutput {
                    source,
                    target,
                    kind: hop.edge.kind.as_str().to_string(),
                    distance: hop.distance,
                });
            }
            AncestorTarget::Remote(id) => {
                external.push((hop.edge.source, id, hop.edge.kind, hop.distance));
            }
        }
    }
    append_external_edges(ctx, &mut nodes, &mut edges, external).await;
    Ok((nodes, edges))
}

/// Other-store edges of the page's nodes, each target read through the
/// port as the caller. A node of another store is a leaf at its edge's
/// distance, over an edge of the recorded kind (`origin`, or `reference`
/// for an interpretation's subject); erased or unreadable targets render
/// with a `status` instead of failing the walk.
async fn append_external_edges(
    ctx: &McpToolCtx,
    nodes: &mut Vec<LineageNodeOutput>,
    edges: &mut Vec<LineageEdgeOutput>,
    external: Vec<(MemoryId, uuid::Uuid, crate::EdgeKind, u8)>,
) {
    let mut unique = Vec::new();
    let mut seen = HashSet::new();
    for (_, origin, _, _) in &external {
        if seen.insert(*origin) {
            unique.push(*origin);
        }
    }
    let mut projections = super::ancestry::read_remote_targets(ctx, &unique).await;
    for (memory_id, origin, edge_kind, distance) in external {
        let source = memory_id.into_inner().to_string();
        let handle = origin.to_string();
        if let Some(remote) = projections.remove(&origin) {
            nodes.push(LineageNodeOutput {
                memory: handle.clone(),
                kind: remote.kind,
                schema_id: remote.schema_id,
                snippet: remote.snippet,
                distance,
                store: Some(crate::memory_store_number(origin)),
                status: remote.status.map(ToString::to_string),
                unavailable_reason: remote.unavailable_reason.map(ToString::to_string),
            });
        }
        edges.push(LineageEdgeOutput {
            source,
            target: handle,
            kind: edge_kind.as_str().to_string(),
            distance,
        });
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use super::{LINEAGE_CURSOR, LineageNodeOutput, append_external_edges, lineage_fingerprint};
    use crate::mcp::core_tools::ancestry::project_remote_read;
    use crate::mcp::core_tools::memory_spaces::test_ctx::ctx_for;
    use crate::storage_ports::other_stores::RemoteOriginRead;
    use crate::storage_ports::other_stores::{
        OtherStores, OtherStoresError, RemoteMemory, RemoteRead, RemoteSearch, RemoteSearchResult,
        StoreCaller, StoreInfo,
    };
    use crate::verbs::query::{MemoryLineageCursor, MemoryLineageDirection};
    use crate::{MemoryId, UserId};
    use futures::future::BoxFuture;

    /// The cursor binds memory, direction, and depth; any of the three
    /// changing between pages fails closed. `limit` stays out of the
    /// fingerprint so it may vary.
    #[test]
    /// #480: an other-store origin the store says it cannot read is "not
    /// readable"; a store that failed or answered for another memory is
    /// "store unavailable" (retry later). Deadline and cap reasons are kept,
    /// and every one of them keeps the console-safe status `unavailable`.
    fn unreadable_origins_say_why_they_are_unavailable() {
        let reason = |read: Option<&Result<RemoteOriginRead, OtherStoresError>>, capped| {
            let projection = project_remote_read(read, capped);
            assert_eq!(projection.status, Some("unavailable"));
            projection.unavailable_reason
        };
        assert_eq!(
            reason(Some(&Ok(RemoteOriginRead::NotFound)), false),
            Some("not readable")
        );
        assert_eq!(
            reason(Some(&Err(OtherStoresError::UnknownStore(1))), false),
            Some("not readable")
        );
        assert_eq!(reason(None, false), Some("not readable"));
        for failed in [
            OtherStoresError::Unavailable("remote internals".into()),
            OtherStoresError::MismatchedMemory {
                requested: crate::new_memory_uuid(1),
                returned: crate::new_memory_uuid(1),
            },
        ] {
            assert_eq!(reason(Some(&Err(failed)), false), Some("store unavailable"));
        }
        assert_eq!(
            reason(Some(&Err(OtherStoresError::DeadlineExceeded)), false),
            Some("deadline exceeded")
        );
        assert_eq!(reason(None, true), Some("origin limit exceeded"));
    }

    #[test]
    fn lineage_cursor_binds_memory_direction_and_depth() {
        let memory = "018f0000-0000-7000-8000-000000000001";
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
                "018f0000-0000-7000-8000-000000000002",
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

    struct SlowLineageStore {
        reads: Arc<AtomicUsize>,
    }

    impl OtherStores for SlowLineageStore {
        fn stores<'a>(
            &'a self,
            _caller: StoreCaller<'a>,
        ) -> BoxFuture<'a, Result<Vec<StoreInfo>, OtherStoresError>> {
            Box::pin(async { Ok(Vec::new()) })
        }
        fn search<'a>(
            &'a self,
            _caller: StoreCaller<'a>,
            _store: u16,
            _args: &'a RemoteSearch,
        ) -> BoxFuture<'a, Result<RemoteSearchResult, OtherStoresError>> {
            Box::pin(async {
                Ok(RemoteSearchResult {
                    hits: Vec::new(),
                    degraded_to_lexical: false,
                })
            })
        }
        fn read<'a>(
            &'a self,
            _caller: StoreCaller<'a>,
            id: uuid::Uuid,
        ) -> BoxFuture<'a, Result<RemoteRead, OtherStoresError>> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                tokio::time::sleep(std::time::Duration::from_millis(1_900)).await;
                Ok(RemoteRead::Found(RemoteMemory {
                    handle: id,
                    writer: crate::WriterAttribution {
                        by: uuid::Uuid::from_u128(3),
                        via: None,
                    },
                    kind: crate::EntityKind::Fact,
                    schema_id: "test/fact-v1".into(),
                    title: None,
                    body: String::new(),
                    created_at: time::OffsetDateTime::UNIX_EPOCH,
                    tags: Vec::new(),
                }))
            })
        }
    }

    #[tokio::test(start_paused = true)]
    async fn lineage_page_deduplicates_caps_and_deadline_marks_origins() {
        let mut ctx = ctx_for(UserId::new(uuid::Uuid::now_v7()), Vec::new());
        let reads = Arc::new(AtomicUsize::new(0));
        ctx.services = crate::FlavorServices::with(
            crate::storage_ports::other_stores::OtherStoresService::new(Arc::new(
                SlowLineageStore {
                    reads: Arc::clone(&reads),
                },
            )),
        );
        let mut nodes = (0u128..200)
            .map(|index| {
                let id = MemoryId::new(uuid::Uuid::from_u128(index + 1));
                LineageNodeOutput {
                    memory: id.into_inner().to_string(),
                    kind: "fact".into(),
                    schema_id: "test/fact-v1".into(),
                    snippet: String::new(),
                    distance: 0,
                    store: None,
                    status: None,
                    unavailable_reason: None,
                }
            })
            .collect::<Vec<_>>();
        let remote_ids = (0..255)
            .map(|_| crate::new_memory_uuid(1))
            .collect::<Vec<_>>();
        let origins = (0u128..200)
            .flat_map(|node| {
                let id = MemoryId::new(uuid::Uuid::from_u128(node + 1));
                remote_ids
                    .iter()
                    .copied()
                    .map(move |origin| (id, origin, crate::EdgeKind::Origin, 1))
            })
            .collect::<Vec<_>>();
        let mut edges = Vec::new();
        let started = tokio::time::Instant::now();
        let deadline = crate::storage_ports::other_stores::DEFAULT_REMOTE_BATCH_DEADLINE;
        tokio::time::timeout(
            deadline + std::time::Duration::from_millis(1),
            append_external_edges(&ctx, &mut nodes, &mut edges, origins),
        )
        .await
        .expect("lineage page must finish within the call deadline");
        assert!(started.elapsed() <= deadline);
        assert_eq!(reads.load(Ordering::SeqCst), 48);
        assert_eq!(nodes.len(), 200 + 255);
        let remote = nodes
            .iter()
            .filter(|node| node.store.is_some())
            .collect::<Vec<_>>();
        assert_eq!(
            remote.iter().filter(|node| node.status.is_none()).count(),
            40
        );
        assert_eq!(
            remote
                .iter()
                .filter(|node| node.status.as_deref() == Some("unavailable"))
                .count(),
            215
        );
        assert_eq!(
            remote
                .iter()
                .filter(|node| node.unavailable_reason.as_deref() == Some("origin limit exceeded"))
                .count(),
            191
        );
        assert_eq!(
            remote
                .iter()
                .filter(|node| node.unavailable_reason.as_deref() == Some("deadline exceeded"))
                .count(),
            24
        );
        assert_eq!(edges.len(), 200 * 255);
        assert!(
            nodes
                .iter()
                .any(|node| node.unavailable_reason.as_deref() == Some("origin limit exceeded"))
        );
        assert!(
            nodes
                .iter()
                .any(|node| node.unavailable_reason.as_deref() == Some("deadline exceeded"))
        );
    }
}
