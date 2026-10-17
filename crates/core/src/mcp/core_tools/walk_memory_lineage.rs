//! `core/walk_memory_lineage` — wire-facing memory lineage walk.

use std::collections::{HashMap, HashSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::MemoryId;
use crate::mcp::cursor as wire_cursor;
use crate::mcp::{McpToolCtx, McpToolError};
use crate::storage_ports::other_stores::{MAX_LINEAGE_REMOTE_ORIGINS, RemoteOriginRead};
use crate::verbs::query::{MemoryLineageCursor, MemoryLineageDirection, MemoryLineageRequest};

const MAX_LINEAGE_DEPTH: u32 = 8;
const DEFAULT_LINEAGE_DEPTH: u32 = 3;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WalkMemoryLineageArgs {
    /// Memory UUID.
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
    /// Set only on a node of another store: its store number.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub store: Option<u16>,
    /// Set only on an other-store node that could not be read: `erased`
    /// (the store erased it) or `unavailable` (no port, the store did not
    /// answer, or the caller may no longer read it). The walk never fails
    /// on such a node.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// Why an unavailable origin was not read when a page cap or deadline
    /// prevented the read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<String>,
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

    let mut edges: Vec<LineageEdgeOutput> = response
        .edges
        .into_iter()
        .map(|hop| LineageEdgeOutput {
            source: super::wire_ref::format_endpoint(&ctx, hop.edge.source),
            target: super::wire_ref::format_target_projection(&ctx, hop.edge.target),
            kind: hop.edge.kind.as_str().to_string(),
            distance: hop.distance,
        })
        .collect();

    let mut nodes = nodes;
    if matches!(direction, MemoryLineageDirection::Ancestors) {
        append_external_origins(&ctx, &mut nodes, &mut edges, response.external_origins).await;
    }

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

/// Other-store origins of the page's nodes, each read through the port as
/// the caller. A node of another store is one hop beyond the node it is an
/// origin of; erased or unreadable origins render with a `status` instead
/// of failing the walk.
async fn append_external_origins(
    ctx: &McpToolCtx,
    nodes: &mut Vec<LineageNodeOutput>,
    edges: &mut Vec<LineageEdgeOutput>,
    external: Vec<(MemoryId, uuid::Uuid)>,
) {
    let service = super::other_stores::service(ctx);
    let has_service = service.is_some();
    let mut unique = Vec::new();
    let mut seen = HashSet::new();
    for (_, origin) in &external {
        if seen.insert(*origin) {
            unique.push(*origin);
        }
    }
    let readable = unique.len().min(MAX_LINEAGE_REMOTE_ORIGINS);
    let reads = match service {
        Some(service) if readable > 0 => Some(
            service
                .read_origins_bounded(super::other_stores::caller(ctx), &unique[..readable])
                .await,
        ),
        _ => None,
    };
    let reads = reads.unwrap_or_else(|| {
        (0..readable)
            .map(|_| {
                Err(
                    crate::storage_ports::other_stores::OtherStoresError::Unavailable(
                        "origin unavailable".into(),
                    ),
                )
            })
            .collect()
    });
    let read_by_handle: HashMap<_, _> = unique.iter().take(readable).copied().zip(reads).collect();
    for (memory_id, origin) in external {
        let Some(source) = nodes
            .iter()
            .find(|node| {
                node.store.is_none() && ctx.resolve_memory(&node.memory).ok() == Some(memory_id)
            })
            .map(|node| (node.memory.clone(), node.distance))
        else {
            continue;
        };
        let distance = source.1.saturating_add(1);
        let capped = has_service
            && unique.len() > MAX_LINEAGE_REMOTE_ORIGINS
            && !read_by_handle.contains_key(&origin);
        let read = read_by_handle.get(&origin);
        let handle = origin.to_string();
        let (kind, schema_id, snippet, status, unavailable_reason) =
            project_remote_read(read, capped);
        if !nodes.iter().any(|node| node.memory == handle) {
            nodes.push(LineageNodeOutput {
                memory: handle.clone(),
                kind,
                schema_id,
                snippet,
                distance,
                store: Some(crate::memory_store_number(origin)),
                status: status.map(ToString::to_string),
                unavailable_reason: unavailable_reason.map(ToString::to_string),
            });
        }
        edges.push(LineageEdgeOutput {
            source: source.0,
            target: handle,
            kind: crate::EdgeKind::Origin.as_str().to_string(),
            distance,
        });
    }
}

fn project_remote_read(
    read: Option<&Result<RemoteOriginRead, crate::storage_ports::other_stores::OtherStoresError>>,
    capped: bool,
) -> (
    String,
    String,
    String,
    Option<&'static str>,
    Option<&'static str>,
) {
    const SNIPPET_CHARS: usize = 200;
    match read {
        Some(Ok(RemoteOriginRead::Found(_, memory))) => (
            memory.kind.as_str().to_string(),
            memory.schema_id.clone(),
            memory.body.chars().take(SNIPPET_CHARS).collect(),
            None,
            None,
        ),
        Some(Ok(RemoteOriginRead::Erased)) => (
            String::new(),
            String::new(),
            String::new(),
            Some("erased"),
            None,
        ),
        Some(Err(crate::storage_ports::other_stores::OtherStoresError::DeadlineExceeded)) => (
            String::new(),
            String::new(),
            String::new(),
            Some("unavailable"),
            Some("deadline exceeded"),
        ),
        Some(Ok(RemoteOriginRead::NotFound)) => (
            String::new(),
            String::new(),
            String::new(),
            Some("unavailable"),
            None,
        ),
        Some(Err(_)) | None if !capped => (
            String::new(),
            String::new(),
            String::new(),
            Some("unavailable"),
            None,
        ),
        Some(Err(_)) | None => (
            String::new(),
            String::new(),
            String::new(),
            Some("unavailable"),
            Some("origin limit exceeded"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use super::{LINEAGE_CURSOR, LineageNodeOutput, append_external_origins, lineage_fingerprint};
    use crate::mcp::core_tools::memory_spaces::test_ctx::ctx_for;
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
                remote_ids.iter().copied().map(move |origin| (id, origin))
            })
            .collect::<Vec<_>>();
        let mut edges = Vec::new();
        let started = tokio::time::Instant::now();
        let deadline = crate::storage_ports::other_stores::DEFAULT_REMOTE_BATCH_DEADLINE;
        tokio::time::timeout(
            deadline + std::time::Duration::from_millis(1),
            append_external_origins(&ctx, &mut nodes, &mut edges, origins),
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
