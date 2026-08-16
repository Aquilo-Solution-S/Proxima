//! Load pin-carrier nodes from storage; project [`Edge`] in memory.

use std::collections::{HashMap, HashSet};

use crate::edge::{PinNode, pin_created_at, project_listed_edge};
use crate::error::ProtocolError;
use crate::storage_ports::MemoryReadHandle;
use crate::verbs::query::{
    EdgeExistsRequest, EdgeExistsResponse, EdgeReadCursor, EdgeReadRequest, EdgeReadResponse,
};
use crate::{Edge, EdgeKind, EntityKind, EntityRef, MemoryId, OwnerRef};

use super::errors::internal_storage_error;

type HopKey = (time::OffsetDateTime, uuid::Uuid, uuid::Uuid, &'static str);

struct PinHop {
    source_id: MemoryId,
    source_kind: EntityKind,
    target_id: MemoryId,
    kind: EdgeKind,
}

fn empty_read() -> EdgeReadResponse {
    EdgeReadResponse {
        edges: Vec::new(),
        next_cursor: None,
    }
}

fn memory_ref(entity: Option<EntityRef>) -> Option<MemoryId> {
    match entity? {
        EntityRef::Memory(id) => Some(id),
        EntityRef::Goal(_) => None,
    }
}

fn expand_hops<'a>(nodes: impl IntoIterator<Item = &'a PinNode>) -> Vec<PinHop> {
    let mut hops = Vec::new();
    for node in nodes {
        hops.extend(node.pins().map(|(target_id, kind)| PinHop {
            source_id: node.id,
            source_kind: node.kind,
            target_id,
            kind,
        }));
    }
    hops
}

fn hop_key(hop: &PinHop) -> HopKey {
    (
        pin_created_at(hop.source_id),
        hop.source_id.into_inner(),
        hop.target_id.into_inner(),
        hop.kind.as_str(),
    )
}

fn cursor_key(cursor: Option<EdgeReadCursor>) -> Option<HopKey> {
    let cursor = cursor?;
    Some((
        cursor.created_at,
        memory_ref(Some(cursor.source))?.into_inner(),
        memory_ref(Some(cursor.target))?.into_inner(),
        cursor.kind.as_str(),
    ))
}

fn page_hops(
    mut hops: Vec<PinHop>,
    cursor: Option<EdgeReadCursor>,
    limit: u32,
    visible: &HashMap<MemoryId, EntityKind>,
) -> EdgeReadResponse {
    hops.sort_unstable_by(|left, right| hop_key(right).cmp(&hop_key(left)));
    if let Some(cursor) = cursor_key(cursor) {
        hops.retain(|hop| hop_key(hop) < cursor);
    }
    let page_len = usize::try_from(limit).unwrap_or(usize::MAX);
    let truncated = hops.len() > page_len;
    hops.truncate(page_len);
    let edges: Vec<Edge> = hops
        .iter()
        .map(|hop| {
            project_listed_edge(
                hop.source_kind,
                hop.source_id,
                hop.target_id,
                hop.kind,
                visible,
            )
        })
        .collect();
    let next_cursor = truncated.then(|| {
        let last = hops.last().expect("truncated page is non-empty");
        EdgeReadCursor {
            created_at: pin_created_at(last.source_id),
            source: EntityRef::Memory(last.source_id),
            target: EntityRef::Memory(last.target_id),
            kind: last.kind,
        }
    });
    EdgeReadResponse { edges, next_cursor }
}

async fn load_visible(
    memory_read: &MemoryReadHandle,
    read_owners: &[OwnerRef],
    ids: &[MemoryId],
) -> Result<HashMap<MemoryId, EntityKind>, ProtocolError> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let nodes = memory_read
        .load_pin_nodes(read_owners, ids)
        .await
        .map_err(|err| internal_storage_error("load_pin_nodes", &err))?;
    Ok(nodes.into_iter().map(|node| (node.id, node.kind)).collect())
}

/// Source and/or inbound pin nodes, then project a newest-first page.
pub(in crate::engine) async fn read_edges_from_nodes(
    memory_read: &MemoryReadHandle,
    read_owners: &[OwnerRef],
    req: &EdgeReadRequest,
) -> Result<EdgeReadResponse, ProtocolError> {
    if matches!(req.filter.source, Some(EntityRef::Goal(_)))
        || matches!(req.filter.target, Some(EntityRef::Goal(_)))
    {
        return Ok(empty_read());
    }
    let source_id = memory_ref(req.filter.source);
    let target_id = memory_ref(req.filter.target);
    if source_id.is_none() && target_id.is_none() {
        return Ok(empty_read());
    }

    let sources = if let Some(source) = source_id {
        memory_read
            .load_pin_nodes(read_owners, &[source])
            .await
            .map_err(|err| internal_storage_error("load_pin_nodes", &err))?
    } else if let Some(target) = target_id {
        memory_read
            .load_inbound_pin_nodes(read_owners, &[target])
            .await
            .map_err(|err| internal_storage_error("load_inbound_pin_nodes", &err))?
    } else {
        Vec::new()
    };

    let hops: Vec<PinHop> = expand_hops(&sources)
        .into_iter()
        .filter(|hop| source_id.is_none_or(|src| hop.source_id == src))
        .filter(|hop| target_id.is_none_or(|tgt| hop.target_id == tgt))
        .filter(|hop| req.filter.kind.is_none_or(|kind| hop.kind == kind))
        .collect();

    let mut want: Vec<MemoryId> = hops.iter().map(|hop| hop.target_id).collect();
    want.sort_unstable();
    want.dedup();
    let visible = load_visible(memory_read, read_owners, &want).await?;
    Ok(page_hops(hops, req.cursor, req.limit, &visible))
}

pub(in crate::engine) async fn edge_exists_from_nodes(
    memory_read: &MemoryReadHandle,
    read_owners: &[OwnerRef],
    req: &EdgeExistsRequest,
) -> Result<EdgeExistsResponse, ProtocolError> {
    let read = EdgeReadRequest {
        owner: req.owner,
        filter: req.filter.clone(),
        limit: 1,
        cursor: None,
    };
    let page = read_edges_from_nodes(memory_read, read_owners, &read).await?;
    Ok(EdgeExistsResponse {
        exists: !page.edges.is_empty(),
    })
}

/// Requested rows + inbound rows + authorized pin targets, then project.
pub(in crate::engine) async fn neighbor_edges_from_nodes(
    memory_read: &MemoryReadHandle,
    read_owners: &[OwnerRef],
    memory_ids: &[MemoryId],
    limit: usize,
) -> Result<Vec<Edge>, ProtocolError> {
    if memory_ids.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }
    let requested = memory_read
        .load_pin_nodes(read_owners, memory_ids)
        .await
        .map_err(|err| internal_storage_error("load_pin_nodes", &err))?;
    let inbound = memory_read
        .load_inbound_pin_nodes(read_owners, memory_ids)
        .await
        .map_err(|err| internal_storage_error("load_inbound_pin_nodes", &err))?;

    let mut by_id: HashMap<MemoryId, PinNode> = HashMap::new();
    for node in requested.into_iter().chain(inbound) {
        by_id.entry(node.id).or_insert(node);
    }
    let requested_ids: HashSet<MemoryId> = memory_ids.iter().copied().collect();
    let mut hops: Vec<PinHop> = expand_hops(by_id.values())
        .into_iter()
        .filter(|hop| {
            requested_ids.contains(&hop.source_id) || requested_ids.contains(&hop.target_id)
        })
        .collect();

    let missing: Vec<MemoryId> = {
        let mut ids: Vec<MemoryId> = hops
            .iter()
            .map(|hop| hop.target_id)
            .filter(|id| !by_id.contains_key(id))
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    };
    let extra = load_visible(memory_read, read_owners, &missing).await?;
    let mut visible: HashMap<MemoryId, EntityKind> =
        by_id.iter().map(|(id, node)| (*id, node.kind)).collect();
    visible.extend(extra);

    hops.sort_unstable_by(|left, right| hop_key(right).cmp(&hop_key(left)));
    hops.truncate(limit);
    Ok(hops
        .iter()
        .map(|hop| {
            project_listed_edge(
                hop.source_kind,
                hop.source_id,
                hop.target_id,
                hop.kind,
                &visible,
            )
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::{PinHop, empty_read, page_hops};
    use crate::{EdgeKind, EdgeTargetProjection, EntityKind, MemoryId};
    use std::collections::HashMap;

    fn hop(source: MemoryId, target: MemoryId, kind: EdgeKind) -> PinHop {
        PinHop {
            source_id: source,
            source_kind: EntityKind::Abstraction,
            target_id: target,
            kind,
        }
    }

    #[test]
    fn no_source_and_no_target_is_empty() {
        let page = empty_read();
        assert!(page.edges.is_empty());
        assert!(page.next_cursor.is_none());
    }

    #[test]
    fn page_hops_is_newest_first_and_resumes() {
        let older = MemoryId::new(uuid::Uuid::now_v7());
        let newer = MemoryId::new(uuid::Uuid::now_v7());
        let a = MemoryId::new(uuid::Uuid::now_v7());
        let b = MemoryId::new(uuid::Uuid::now_v7());
        let visible = HashMap::from([
            (a, EntityKind::Fact),
            (b, EntityKind::Fact),
            (older, EntityKind::Abstraction),
            (newer, EntityKind::Abstraction),
        ]);
        let hops = vec![
            hop(older, a, EdgeKind::Origin),
            hop(newer, b, EdgeKind::Origin),
        ];
        let first = page_hops(hops, None, 1, &visible);
        assert_eq!(first.edges.len(), 1);
        assert_eq!(first.edges[0].source.memory_id(), Some(newer));
        let cursor = first.next_cursor.expect("more hops");
        let second = page_hops(
            vec![
                hop(older, a, EdgeKind::Origin),
                hop(newer, b, EdgeKind::Origin),
            ],
            Some(cursor),
            1,
            &visible,
        );
        assert_eq!(second.edges.len(), 1);
        assert_eq!(second.edges[0].source.memory_id(), Some(older));
        assert!(second.next_cursor.is_none());
    }

    #[test]
    fn page_hops_redacts_targets_absent_from_visible() {
        let source = MemoryId::new(uuid::Uuid::now_v7());
        let missing = MemoryId::new(uuid::Uuid::now_v7());
        let page = page_hops(
            vec![hop(source, missing, EdgeKind::Reference)],
            None,
            10,
            &HashMap::new(),
        );
        assert!(matches!(
            page.edges[0].target,
            EdgeTargetProjection::Redacted
        ));
    }
}
