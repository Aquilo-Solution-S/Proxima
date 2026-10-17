//! One ancestor-edge rule for `core_think` and `walk_memory_lineage` (#480).
//!
//! A node's ancestors are what it was made from or is about:
//!
//! - its local `origins[]` (`Origin`), when its schema declares
//!   `OriginEdges` or declares nothing (fail-open, see `ancestor_edges`);
//! - the payload references in its declared subject columns
//!   (`Reference`), when its schema declares `PayloadOnly`;
//! - the edges recorded to other stores' memories, with their recorded kind
//!   (`Origin` for a derivation's source, `Reference` for an
//!   interpretation's subject).
//!
//! An other-store target is a leaf: it is read through the port as the
//! caller and never expanded, because this store does not hold its edges.

use std::collections::HashMap;

use uuid::Uuid;

use crate::flavor::Provenance;
use crate::mcp::{McpToolCtx, McpToolError};
use crate::storage_ports::other_stores::{
    MAX_LINEAGE_REMOTE_ORIGINS, OtherStoresError, RemoteOriginRead,
};
use crate::{EdgeKind, GetMemoriesReadRequest, MemoryId};

/// Where an ancestor edge lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum AncestorTarget {
    /// A memory of this store; the walk may expand it.
    Local(MemoryId),
    /// Another store's memory; a leaf.
    Remote(Uuid),
}

impl AncestorTarget {
    pub(crate) fn uuid(self) -> Uuid {
        match self {
            Self::Local(id) => id.into_inner(),
            Self::Remote(id) => id,
        }
    }
}

/// One ancestor edge: `source` was made from, or is about, `target`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AncestorEdge {
    pub(crate) source: MemoryId,
    pub(crate) target: AncestorTarget,
    pub(crate) kind: EdgeKind,
}

/// The ancestor edges of every `frontier` node the caller can read, newest
/// `(source, target)` first.
///
/// The declaration answers "how is this node's provenance REACHABLE":
///
/// - `OriginEdges` — expand `origins[]`.
/// - `PayloadOnly { subject_columns }` — load the node's payload and take
///   the references whose declared FIELD is one of the named columns
///   (`SidecarPayload::references()` carries the field each came from). A
///   `PayloadOnly` node has an EMPTY `origins[]` by construction, so a walk
///   that did not ask would make an interpretation a lineage dead end.
/// - `None` — nothing local. A Fact cannot have origins
///   (`memory_fact_origins_chk`), and otherwise the declaration is the claim
///   that it derives from nothing.
///
/// A schema no contract declares is treated as `OriginEdges`. That is a
/// deliberate fail-OPEN: a registration without a contract must not make
/// its memories invisible to the walk, and showing a pin whose declaration
/// nobody wrote is a smaller wrong than hiding one.
///
/// Every node's recorded other-store edges are added whatever its
/// declaration, with their recorded kind.
///
/// # Errors
///
/// Storage failures of the pin, payload, or external-edge reads.
pub(crate) async fn ancestor_edges(
    engine: &crate::Engine,
    ctx: &McpToolCtx,
    frontier: &[MemoryId],
) -> Result<Vec<AncestorEdge>, McpToolError> {
    if frontier.is_empty() {
        return Ok(Vec::new());
    }
    let pins = engine.pin_nodes(&ctx.authz, frontier).await?;
    let mut edges = Vec::new();
    let mut grounded: Vec<(MemoryId, &'static [&'static str])> = Vec::new();
    let mut visible = Vec::with_capacity(pins.len());
    for node in pins {
        visible.push(node.id);
        match ctx.registry.provenance_of(&node.schema_id, node.kind) {
            None | Some(Provenance::OriginEdges) => {
                edges.extend(node.origins.into_iter().map(|origin| AncestorEdge {
                    source: node.id,
                    target: AncestorTarget::Local(origin),
                    kind: EdgeKind::Origin,
                }));
            }
            Some(Provenance::PayloadOnly { subject_columns }) => {
                grounded.push((node.id, subject_columns));
            }
            Some(Provenance::None) => {}
        }
    }
    edges.extend(payload_subjects(engine, ctx, &grounded).await?);
    edges.extend(
        engine
            .external_edges(&ctx.authz, &visible)
            .await?
            .into_iter()
            .map(|(source, target, kind)| AncestorEdge {
                source,
                target: AncestorTarget::Remote(target),
                kind,
            }),
    );
    edges.sort_by_key(|edge| std::cmp::Reverse((edge.source, edge.target.uuid())));
    edges.dedup_by_key(|edge| (edge.source, edge.target));
    Ok(edges)
}

/// The subjects `PayloadOnly` nodes ground in, read through the payload
/// fields their schemas declare, as `Reference` edges.
///
/// `get_memories` is the same owner-scoped read `core_get_memories` uses,
/// so a subject the caller may not see is not returned and the walk simply
/// does not reach it — the ordinary shape of an absent node, not a redaction.
async fn payload_subjects(
    engine: &crate::Engine,
    ctx: &McpToolCtx,
    grounded: &[(MemoryId, &'static [&'static str])],
) -> Result<Vec<AncestorEdge>, McpToolError> {
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
    let declared: HashMap<MemoryId, &'static [&'static str]> = grounded.iter().copied().collect();
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
                subjects.push(AncestorEdge {
                    source: snapshot.memory_id,
                    target: AncestorTarget::Local(id),
                    kind: EdgeKind::Reference,
                });
            }
        }
    }
    Ok(subjects)
}

/// How an other-store memory renders on a walk: from the read, or empty
/// with a `status` (and why) when it could not be read. A walk never fails
/// on such a node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoteProjection {
    pub(crate) kind: String,
    pub(crate) schema_id: String,
    pub(crate) snippet: String,
    pub(crate) status: Option<&'static str>,
    pub(crate) unavailable_reason: Option<&'static str>,
}

/// Read `handles` (distinct, in walk order) through the port as the caller,
/// at most [`MAX_LINEAGE_REMOTE_ORIGINS`] of them under one call-wide
/// deadline; the rest render as `origin limit exceeded`.
pub(crate) async fn read_remote_targets(
    ctx: &McpToolCtx,
    handles: &[Uuid],
) -> HashMap<Uuid, RemoteProjection> {
    let service = super::other_stores::service(ctx);
    let has_service = service.is_some();
    let readable = handles.len().min(MAX_LINEAGE_REMOTE_ORIGINS);
    let reads = match service {
        Some(service) if readable > 0 => Some(
            service
                .read_origins_bounded(super::other_stores::caller(ctx), &handles[..readable])
                .await,
        ),
        _ => None,
    };
    // No port wired: every target is on a store this deployment does not know.
    let reads = reads.unwrap_or_else(|| {
        handles[..readable]
            .iter()
            .map(|handle| {
                Err(OtherStoresError::UnknownStore(crate::memory_store_number(
                    *handle,
                )))
            })
            .collect()
    });
    let mut read_by_handle: HashMap<Uuid, _> =
        handles.iter().take(readable).copied().zip(reads).collect();
    handles
        .iter()
        .map(|handle| {
            let read = read_by_handle.remove(handle);
            let capped = has_service && read.is_none() && handles.len() > readable;
            (*handle, project_remote_read(read.as_ref(), capped))
        })
        .collect()
}

pub(crate) fn project_remote_read(
    read: Option<&Result<RemoteOriginRead, OtherStoresError>>,
    capped: bool,
) -> RemoteProjection {
    const SNIPPET_CHARS: usize = 200;
    let unread = |status, reason| RemoteProjection {
        kind: String::new(),
        schema_id: String::new(),
        snippet: String::new(),
        status: Some(status),
        unavailable_reason: reason,
    };
    match read {
        Some(Ok(RemoteOriginRead::Found(_, memory))) => RemoteProjection {
            kind: memory.kind.as_str().to_string(),
            schema_id: memory.schema_id.clone(),
            snippet: memory.body.chars().take(SNIPPET_CHARS).collect(),
            status: None,
            unavailable_reason: None,
        },
        Some(Ok(RemoteOriginRead::Erased)) => unread("erased", None),
        Some(Err(OtherStoresError::DeadlineExceeded)) => {
            unread("unavailable", Some("deadline exceeded"))
        }
        _ if capped => unread("unavailable", Some("origin limit exceeded")),
        // The store answered that the caller cannot read it, or there is no
        // such store (also: no other store wired).
        Some(Ok(RemoteOriginRead::NotFound) | Err(OtherStoresError::UnknownStore(_))) | None => {
            unread("unavailable", Some("not readable"))
        }
        // The store failed or answered for another memory: retry later.
        Some(Err(_)) => unread("unavailable", Some("store unavailable")),
    }
}
