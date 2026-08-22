//! Load pin-carrier nodes from storage; project [`Edge`] in memory.

use std::collections::{HashMap, HashSet};

use crate::edge::{PinNode, pin_created_at, project_listed_edge};
use crate::error::ProtocolError;
use crate::storage_ports::{InboundPinQuery, MemoryReadHandle};
use crate::verbs::query::{
    EdgeExistsRequest, EdgeExistsResponse, EdgeReadCursor, EdgeReadRequest, EdgeReadResponse,
};
use crate::{Edge, EdgeKind, EntityKind, EntityRef, MemoryId, OwnerRef};

use super::errors::internal_storage_error;

/// Floor on incoming `read_edges` source pages so a small hop `limit`
/// still amortizes GIN. `exists` (`limit == 1`, no cursor) uses SQL 1.
const INBOUND_SOURCE_PAGE_MIN: u32 = 256;

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

    if req.limit == 0 {
        return Ok(empty_read());
    }

    let sources = if let Some(source) = source_id {
        memory_read
            .load_pin_nodes(read_owners, &[source])
            .await
            .map_err(|err| internal_storage_error("load_pin_nodes", &err))?
    } else if let Some(target) = target_id {
        load_incoming_sources(memory_read, read_owners, target, req).await?
    } else {
        Vec::new()
    };

    let hops = matching_hops(&sources, source_id, target_id, req.filter.kind);

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

fn inbound_source_page_limit(req: &EdgeReadRequest) -> u32 {
    if req.limit == 1 && req.cursor.is_none() {
        1
    } else {
        req.limit.max(INBOUND_SOURCE_PAGE_MIN)
    }
}

fn hops_after_cursor(hops: &[PinHop], cursor: Option<EdgeReadCursor>) -> usize {
    match cursor_key(cursor) {
        None => hops.len(),
        Some(cut) => hops.iter().filter(|hop| hop_key(hop) < cut).count(),
    }
}

fn matching_hops(
    sources: &[PinNode],
    source_id: Option<MemoryId>,
    target_id: Option<MemoryId>,
    kind: Option<EdgeKind>,
) -> Vec<PinHop> {
    expand_hops(sources)
        .into_iter()
        .filter(|hop| source_id.is_none_or(|src| hop.source_id == src))
        .filter(|hop| target_id.is_none_or(|tgt| hop.target_id == tgt))
        .filter(|hop| kind.is_none_or(|want| hop.kind == want))
        .collect()
}

async fn load_incoming_sources(
    memory_read: &MemoryReadHandle,
    read_owners: &[OwnerRef],
    target: MemoryId,
    req: &EdgeReadRequest,
) -> Result<Vec<PinNode>, ProtocolError> {
    let page_limit = inbound_source_page_limit(req);
    let hop_limit = usize::try_from(req.limit).unwrap_or(usize::MAX);
    let mut sources = Vec::new();
    let mut after = None;
    if let Some(cursor) = req.cursor
        && let Some(src) = memory_ref(Some(cursor.source))
    {
        sources.extend(
            memory_read
                .load_pin_nodes(read_owners, &[src])
                .await
                .map_err(|err| internal_storage_error("load_pin_nodes", &err))?,
        );
        after = Some(src);
    }
    let targets = [target];
    loop {
        let page = memory_read
            .load_inbound_pin_nodes(
                read_owners,
                InboundPinQuery {
                    targets: &targets,
                    kind: req.filter.kind,
                    heads_only: false,
                    after,
                    limit: page_limit,
                },
            )
            .await
            .map_err(|err| internal_storage_error("load_inbound_pin_nodes", &err))?;
        let short = page.len() < usize::try_from(page_limit).unwrap_or(usize::MAX);
        if let Some(last) = page.last() {
            after = Some(last.id);
        }
        sources.extend(page);
        let hops = matching_hops(&sources, None, Some(target), req.filter.kind);
        if hops_after_cursor(&hops, req.cursor) >= hop_limit || short {
            break;
        }
    }
    Ok(sources)
}

/// Requested rows plus a newest-first **current-head** inbound sample.
/// Not a complete star — `read_edges` incoming is the complete path.
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
    let inbound_limit = u32::try_from(limit).unwrap_or(u32::MAX);
    let inbound = memory_read
        .load_inbound_pin_nodes(
            read_owners,
            InboundPinQuery {
                targets: memory_ids,
                kind: None,
                heads_only: true,
                after: None,
                limit: inbound_limit,
            },
        )
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
    use super::{PinHop, empty_read, neighbor_edges_from_nodes, page_hops, read_edges_from_nodes};
    use crate::edge::PinNode;
    use crate::storage_ports::{InboundPinQuery, MemoryReadPort};
    use crate::verbs::query::{EdgeFilter, EdgeReadRequest};
    use crate::{
        EdgeKind, EdgeTargetProjection, EntityKind, EntityRef, MemoryId, OwnerRef, StorageError,
    };
    use std::collections::HashMap;
    use std::sync::Arc;

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

    struct FakePins {
        nodes: Vec<PinNode>,
    }

    impl FakePins {
        fn inbound(&self, query: InboundPinQuery<'_>) -> Vec<PinNode> {
            let mut rows: Vec<PinNode> = self
                .nodes
                .iter()
                .filter(|node| {
                    query.targets.iter().any(|t| match query.kind {
                        Some(EdgeKind::Origin) => node.origins.contains(t),
                        Some(EdgeKind::Reference) => node.refs.contains(t),
                        None => node.origins.contains(t) || node.refs.contains(t),
                    })
                })
                .cloned()
                .collect();
            rows.sort_by_key(|node| std::cmp::Reverse(node.id.into_inner()));
            if let Some(after) = query.after {
                rows.retain(|n| n.id.into_inner() < after.into_inner());
            }
            rows.truncate(usize::try_from(query.limit).unwrap_or(usize::MAX));
            rows
        }
    }

    #[async_trait::async_trait]
    impl MemoryReadPort for FakePins {
        async fn load_fact_text(
            &self,
            _owner: &crate::Owner,
            _memory_id: MemoryId,
        ) -> Result<Option<String>, StorageError> {
            Ok(None)
        }

        async fn load_memory_graph_payloads(
            &self,
            _identities: &[crate::storage::MemoryGraphIdentity],
            _include_body: bool,
        ) -> Result<Vec<crate::storage::MemoryGraphPayloadRow>, StorageError> {
            Ok(Vec::new())
        }

        async fn load_sketches(
            &self,
            _read_owners: &[OwnerRef],
            _memory_ids: &[MemoryId],
        ) -> Result<Vec<crate::read_models::MemorySketch>, StorageError> {
            Ok(Vec::new())
        }

        async fn load_pin_nodes(
            &self,
            _read_owners: &[OwnerRef],
            memory_ids: &[MemoryId],
        ) -> Result<Vec<PinNode>, StorageError> {
            Ok(self
                .nodes
                .iter()
                .filter(|n| memory_ids.contains(&n.id))
                .cloned()
                .collect())
        }

        async fn load_inbound_pin_nodes(
            &self,
            _read_owners: &[OwnerRef],
            query: InboundPinQuery<'_>,
        ) -> Result<Vec<PinNode>, StorageError> {
            Ok(self.inbound(query))
        }

        async fn query_memories(
            &self,
            _req: &crate::verbs::query::QueryRequest,
            _schemas: &[crate::verbs::schema::SchemaInfo],
        ) -> Result<crate::verbs::query::QueryResponse, StorageError> {
            Err(StorageError::Internal("unused".into()))
        }

        async fn search_memories(
            &self,
            _req: &crate::verbs::query::MemorySearchRequest,
            _projections: &[crate::verbs::schema::MemorySearchProjection],
        ) -> Result<crate::verbs::query::MemorySearchPage, StorageError> {
            Err(StorageError::Internal("unused".into()))
        }

        async fn walk_memory_lineage(
            &self,
            _read_owners: &[OwnerRef],
            _req: &crate::verbs::query::MemoryLineageRequest,
        ) -> Result<crate::verbs::query::MemoryLineageResponse, StorageError> {
            Err(StorageError::Internal("unused".into()))
        }

        async fn owned_series_handle(
            &self,
            _owner: crate::Owner,
            _schema_id: &crate::SchemaId,
            _sidecar_table: &str,
            _columns: &[(&str, crate::verbs::query::SidecarAtom)],
        ) -> Result<Option<uuid::Uuid>, StorageError> {
            Ok(None)
        }
    }

    fn hub_fixture(n: usize) -> (OwnerRef, MemoryId, Arc<FakePins>) {
        let owner = OwnerRef::Personal(crate::UserId::new(uuid::Uuid::now_v7()));
        let hub = MemoryId::new(uuid::Uuid::now_v7());
        let mut nodes = vec![PinNode {
            id: hub,
            kind: EntityKind::Fact,
            schema_id: crate::SchemaId::new("test/pin-v1".into()),
            origins: Vec::new(),
            refs: Vec::new(),
        }];
        for _ in 0..n {
            nodes.push(PinNode {
                id: MemoryId::new(uuid::Uuid::now_v7()),
                kind: EntityKind::Abstraction,
                schema_id: crate::SchemaId::new("test/pin-v1".into()),
                origins: vec![hub],
                refs: Vec::new(),
            });
        }
        (owner, hub, Arc::new(FakePins { nodes }))
    }

    #[tokio::test]
    async fn incoming_read_edges_pages_all_sources() {
        let (owner, hub, fake) = hub_fixture(300);
        let handle: crate::storage_ports::MemoryReadHandle = fake;
        let mut cursor = None;
        let mut seen = std::collections::HashSet::new();
        loop {
            let page = read_edges_from_nodes(
                &handle,
                &[owner],
                &EdgeReadRequest {
                    owner,
                    filter: EdgeFilter {
                        kind: Some(EdgeKind::Origin),
                        source: None,
                        target: Some(EntityRef::Memory(hub)),
                    },
                    limit: 50,
                    cursor,
                },
            )
            .await
            .expect("page");
            for edge in &page.edges {
                assert!(seen.insert(edge.source.memory_id().expect("source")));
            }
            match page.next_cursor {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }
        assert_eq!(seen.len(), 300);
    }

    #[tokio::test]
    async fn neighbor_sample_keeps_newest_heads() {
        let (owner, hub, fake) = hub_fixture(300);
        let handle: crate::storage_ports::MemoryReadHandle = fake.clone();
        let edges = neighbor_edges_from_nodes(&handle, &[owner], &[hub], 200)
            .await
            .expect("neighbors");
        assert_eq!(edges.len(), 200);
        let newest: std::collections::HashSet<_> = fake
            .nodes
            .iter()
            .rev()
            .filter(|n| n.id != hub)
            .take(200)
            .map(|n| n.id)
            .collect();
        for edge in &edges {
            assert!(newest.contains(&edge.source.memory_id().expect("src")));
        }
    }
}
