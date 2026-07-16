use crate::common::{drop_db, fresh_pg, seed_memory, seed_memory_edge};
use proxima_core::access::world;
use proxima_core::storage_ports::*;
use proxima_core::verbs::query::{
    EdgeExistsRequest, EdgeFilter, EdgePayloadSpec, EdgeReadRequest, EdgeTargetProjection,
    MemoryLineageDirection, MemoryLineageRequest,
};
use proxima_core::{
    AGENT_LINK_RELATION, AgentLinkV1, CORE_DERIVED_FROM_RELATION, EdgeId, EntityKind, EntityRef,
    GroupId, MemoryId, OwnerRef, RelationClass, SchemaId, SchemaVersion, UserId,
};
use uuid::Uuid;

struct EdgeReadFixture {
    p: OwnerRef,
    q: OwnerRef,
    p_read: Vec<OwnerRef>,
    q_read: Vec<OwnerRef>,
    p_without_g1_read: Vec<OwnerRef>,
    world_read: Vec<OwnerRef>,
    a: MemoryId,
    f1: MemoryId,
    a_to_f1: EdgeId,
    public_to_private: EdgeId,
}

#[tokio::test]
async fn direct_edge_reads_are_source_owned_with_redaction_and_guard()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;

    let result = async {
        let fixture = seed_edge_read_fixture(&pg).await?;

        let q_read = read_edge_by_id(&pg, &fixture.q_read, &fixture.q, fixture.a_to_f1).await?;
        assert!(q_read.edges.is_empty(), "unreadable source hides edge");
        let q_exists = edge_exists_by_id(&pg, &fixture.q_read, &fixture.q, fixture.a_to_f1).await?;
        assert!(!q_exists.exists);

        let p_read = read_edge_by_id(&pg, &fixture.p_read, &fixture.p, fixture.a_to_f1).await?;
        assert_eq!(p_read.edges.len(), 1);
        assert!(matches!(
            p_read.edges[0].target,
            EdgeTargetProjection::Visible { target } if target == EntityRef::Memory(fixture.f1)
        ));
        assert_eq!(p_read.edges[0].source_kind, EntityKind::Abstraction);
        assert_eq!(p_read.edges[0].target_kind, Some(EntityKind::Fact));

        let p_without_g1 =
            read_edge_by_id(&pg, &fixture.p_without_g1_read, &fixture.p, fixture.a_to_f1).await?;
        assert_eq!(p_without_g1.edges.len(), 1);
        assert_eq!(p_without_g1.edges[0].target, EdgeTargetProjection::Redacted);
        assert_eq!(
            p_without_g1.edges[0].target_kind, None,
            "redacted target must not leak its kind"
        );

        let p_target_probe = read_edge_by_target_filter(
            &pg,
            &fixture.p_without_g1_read,
            &fixture.p,
            fixture.a_to_f1,
            EntityRef::Memory(fixture.f1),
        )
        .await?;
        assert!(
            p_target_probe.edges.is_empty(),
            "target-id filters must not confirm an unreadable redacted target"
        );

        let world_read = read_edge_by_id(
            &pg,
            &fixture.world_read,
            &world(),
            fixture.public_to_private,
        )
        .await?;
        assert!(
            world_read.edges.is_empty(),
            "public-source guard omits private target"
        );
        let world_exists = edge_exists_by_id(
            &pg,
            &fixture.world_read,
            &world(),
            fixture.public_to_private,
        )
        .await?;
        assert!(!world_exists.exists);

        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn lineage_hides_unreadable_sources_and_stops_at_redacted_fact_targets()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;

    let result = async {
        let fixture = seed_edge_read_fixture(&pg).await?;
        let gp = fixture.p_without_g1_read[1];
        let g1 = fixture.q_read[1];
        let lifecycle = seed_memory(&pg, &gp, EntityKind::Fact, "lifecycle").await?;
        let evidence = seed_memory(&pg, &g1, EntityKind::Fact, "evidence").await?;
        let lifecycle_edge = seed_memory_edge(
            &pg,
            &gp,
            (EntityKind::Fact, lifecycle),
            (EntityKind::Fact, evidence),
            CORE_DERIVED_FROM_RELATION,
            RelationClass::Provenance,
        )
        .await?;

        let q_from_f1 = pg
            .walk_memory_lineage(
                &fixture.q_read,
                &MemoryLineageRequest {
                    owner: fixture.q,
                    start_memory_id: fixture.f1,
                    direction: MemoryLineageDirection::Descendants,
                    depth: 3,
                    limit: 20,
                },
            )
            .await?;
        assert!(
            q_from_f1
                .edges
                .iter()
                .all(|edge| edge.edge_id != fixture.a_to_f1.into_inner()),
            "lineage from F1 must not surface unreadable source A"
        );
        assert!(
            q_from_f1
                .nodes
                .iter()
                .all(|node| node.memory_id != fixture.a)
        );

        let p_from_lifecycle = pg
            .walk_memory_lineage(
                &fixture.p_without_g1_read,
                &MemoryLineageRequest {
                    owner: fixture.p,
                    start_memory_id: lifecycle,
                    direction: MemoryLineageDirection::Ancestors,
                    depth: 2,
                    limit: 20,
                },
            )
            .await?;
        assert!(
            p_from_lifecycle
                .edges
                .iter()
                .any(|edge| edge.edge_id == lifecycle_edge.into_inner()),
            "readable Fact source exposes its provenance edge"
        );
        assert!(
            p_from_lifecycle
                .nodes
                .iter()
                .all(|node| node.memory_id != evidence),
            "unreadable evidence target is a handle-only frontier, not a node"
        );

        let q_from_evidence = pg
            .walk_memory_lineage(
                &fixture.q_read,
                &MemoryLineageRequest {
                    owner: fixture.q,
                    start_memory_id: evidence,
                    direction: MemoryLineageDirection::Descendants,
                    depth: 2,
                    limit: 20,
                },
            )
            .await?;
        assert!(
            q_from_evidence
                .edges
                .iter()
                .all(|edge| edge.edge_id != lifecycle_edge.into_inner()),
            "Fact-source edge is hidden when the lifecycle source is unreadable"
        );

        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db_name).await;
    result
}

struct AgentLinkFixture {
    owner: OwnerRef,
    seeded: Vec<EdgeId>,
    source_filter: EdgeFilter,
}

/// Seed one Abstraction hub agent-linked to five Facts, each with a
/// `agent_link_v1` sidecar row (`link {i}` / confidence `40+i`).
async fn seed_agent_link_fixture(
    pg: &proxima_storage_pg::PgStorage,
) -> Result<AgentLinkFixture, Box<dyn std::error::Error>> {
    let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
    let hub = seed_memory(pg, &owner, EntityKind::Abstraction, "hub").await?;
    let mut seeded = Vec::new();
    for index in 0..5u8 {
        let fact = seed_memory(pg, &owner, EntityKind::Fact, &format!("fact {index}")).await?;
        let edge = seed_memory_edge(
            pg,
            &owner,
            (EntityKind::Abstraction, hub),
            (EntityKind::Fact, fact),
            AGENT_LINK_RELATION,
            RelationClass::Interpretive,
        )
        .await?;
        sqlx::query(
            "INSERT INTO proxima_core.agent_link_v1 (edge_id, reason, confidence)
             VALUES ($1, $2, $3)",
        )
        .bind(edge.into_inner())
        .bind(format!("link {index}"))
        .bind(i16::from(40 + index))
        .execute(pg.pool_for_tests())
        .await?;
        seeded.push(edge);
    }
    Ok(AgentLinkFixture {
        owner,
        seeded,
        source_filter: EdgeFilter {
            relation: Some(AGENT_LINK_RELATION.to_string()),
            source: Some(EntityRef::Memory(hub)),
            target: None,
        },
    })
}

fn filtered_request(
    fixture: &AgentLinkFixture,
    limit: u32,
    include_payloads: bool,
) -> EdgeReadRequest {
    EdgeReadRequest {
        owner: fixture.owner,
        edge_ids: Vec::new(),
        filter: fixture.source_filter.clone(),
        limit,
        cursor: None,
        include_payloads,
    }
}

#[tokio::test]
async fn filtered_edge_reads_hydrate_agent_link_payloads_on_opt_in()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;

    let result = async {
        let fixture = seed_agent_link_fixture(&pg).await?;
        let read_owners = vec![fixture.owner];
        let specs = vec![EdgePayloadSpec {
            relation: AGENT_LINK_RELATION.to_string(),
            schema_id: SchemaId::new("core/agent-link-v1".into()),
            schema_version: SchemaVersion::new(1),
        }];

        // Full page: endpoint kinds surface, payloads hydrate, no next cursor.
        let full = pg
            .read_edges(&read_owners, &filtered_request(&fixture, 10, true), &specs)
            .await?;
        assert_eq!(full.edges.len(), 5);
        assert!(full.next_cursor.is_none());
        for edge in &full.edges {
            assert_eq!(edge.source_kind, EntityKind::Abstraction);
            assert_eq!(edge.target_kind, Some(EntityKind::Fact));
            let payload = edge.payload.as_ref().expect("agent-link payload hydrates");
            let link = payload
                .downcast_ref::<AgentLinkV1>()
                .expect("payload downcasts to AgentLinkV1");
            assert!(link.reason.starts_with("link "));
            assert!((40..45).contains(&link.confidence));
        }

        // Payload hydration is opt-in: same read without the flag stays lean.
        let lean = pg
            .read_edges(&read_owners, &filtered_request(&fixture, 10, false), &specs)
            .await?;
        assert!(lean.edges.iter().all(|edge| edge.payload.is_none()));

        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn filtered_edge_reads_paginate_by_keyset_cursor() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;

    let result = async {
        let fixture = seed_agent_link_fixture(&pg).await?;
        let read_owners = vec![fixture.owner];

        // Keyset pagination: pages of 2 chain by cursor, cover all edges
        // exactly once, and follow (created_at, edge_id) descending order.
        let mut collected = Vec::new();
        let mut cursor = None;
        let mut pages = 0;
        loop {
            let mut request = filtered_request(&fixture, 2, false);
            request.cursor = cursor;
            let page = pg.read_edges(&read_owners, &request, &[]).await?;
            pages += 1;
            collected.extend(page.edges);
            cursor = page.next_cursor;
            if cursor.is_none() {
                break;
            }
            assert!(pages < 10, "cursor chain must terminate");
        }
        assert_eq!(pages, 3, "5 edges at limit 2 paginate as 2+2+1");
        assert_eq!(collected.len(), 5);
        let mut unique = collected.iter().map(|edge| edge.id).collect::<Vec<_>>();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), 5, "pages must not overlap");
        assert!(
            collected
                .windows(2)
                .all(|pair| (pair[0].created_at, pair[0].id) > (pair[1].created_at, pair[1].id)),
            "pages follow (created_at, edge_id) descending keyset order"
        );
        let mut expected = fixture
            .seeded
            .iter()
            .map(|edge| edge.into_inner())
            .collect::<Vec<_>>();
        expected.sort_unstable();
        assert_eq!(
            unique, expected,
            "pagination covers exactly the seeded edges"
        );

        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db_name).await;
    result
}

async fn read_edge_by_id(
    pg: &proxima_storage_pg::PgStorage,
    read_owners: &[OwnerRef],
    owner: &OwnerRef,
    edge_id: EdgeId,
) -> Result<proxima_core::verbs::query::EdgeReadResponse, proxima_core::StorageError> {
    pg.read_edges(
        read_owners,
        &EdgeReadRequest {
            owner: *owner,
            edge_ids: vec![edge_id],
            filter: EdgeFilter::default(),
            limit: 10,
            cursor: None,
            include_payloads: false,
        },
        &[],
    )
    .await
}

async fn edge_exists_by_id(
    pg: &proxima_storage_pg::PgStorage,
    read_owners: &[OwnerRef],
    owner: &OwnerRef,
    edge_id: EdgeId,
) -> Result<proxima_core::verbs::query::EdgeExistsResponse, proxima_core::StorageError> {
    pg.edge_exists(
        read_owners,
        &EdgeExistsRequest {
            owner: *owner,
            edge_ids: vec![edge_id],
            filter: EdgeFilter::default(),
        },
    )
    .await
}

async fn read_edge_by_target_filter(
    pg: &proxima_storage_pg::PgStorage,
    read_owners: &[OwnerRef],
    owner: &OwnerRef,
    edge_id: EdgeId,
    target: EntityRef,
) -> Result<proxima_core::verbs::query::EdgeReadResponse, proxima_core::StorageError> {
    pg.read_edges(
        read_owners,
        &EdgeReadRequest {
            owner: *owner,
            edge_ids: vec![edge_id],
            filter: EdgeFilter {
                relation: None,
                source: None,
                target: Some(target),
            },
            limit: 10,
            cursor: None,
            include_payloads: false,
        },
        &[],
    )
    .await
}

async fn seed_edge_read_fixture(
    pg: &proxima_storage_pg::PgStorage,
) -> Result<EdgeReadFixture, Box<dyn std::error::Error>> {
    let p = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
    let q = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
    let gp = OwnerRef::Group(GroupId::new(Uuid::now_v7()));
    let g1 = OwnerRef::Group(GroupId::new(Uuid::now_v7()));
    let private = OwnerRef::Group(GroupId::new(Uuid::now_v7()));
    let world = world();

    let f1 = seed_memory(pg, &g1, EntityKind::Fact, "F1").await?;
    let a = seed_memory(pg, &gp, EntityKind::Abstraction, "A").await?;
    let a_public = seed_memory(pg, &gp, EntityKind::Abstraction, "A public").await?;
    let f_private = seed_memory(pg, &private, EntityKind::Fact, "private").await?;

    let a_to_f1 = seed_memory_edge(
        pg,
        &gp,
        (EntityKind::Abstraction, a),
        (EntityKind::Fact, f1),
        CORE_DERIVED_FROM_RELATION,
        RelationClass::Provenance,
    )
    .await?;
    let public_to_private = seed_memory_edge(
        pg,
        &gp,
        (EntityKind::Abstraction, a_public),
        (EntityKind::Fact, f_private),
        CORE_DERIVED_FROM_RELATION,
        RelationClass::Provenance,
    )
    .await?;

    Ok(EdgeReadFixture {
        p,
        q,
        p_read: vec![p, gp, g1, world],
        q_read: vec![q, g1, world],
        p_without_g1_read: vec![p, gp, world],
        world_read: vec![world],
        a,
        f1,
        a_to_f1,
        public_to_private,
    })
}
