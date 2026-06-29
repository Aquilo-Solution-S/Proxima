use crate::common::{drop_db, fresh_pg, seed_memory, seed_memory_edge};
use proxima_core::access::world;
use proxima_core::verbs::query::{
    EdgeExistsRequest, EdgeFilter, EdgeReadRequest, MemoryLineageDirection, MemoryLineageRequest,
};
use proxima_core::{
    CORE_DERIVED_FROM_RELATION, EdgeId, EntityKind, GroupId, MemoryId, OwnerRef, RelationClass,
    Storage, UserId,
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
        assert!(p_read.edges[0].target_readable);
        assert!(!p_read.edges[0].source_world_readable);

        let p_without_g1 =
            read_edge_by_id(&pg, &fixture.p_without_g1_read, &fixture.p, fixture.a_to_f1).await?;
        assert_eq!(p_without_g1.edges.len(), 1);
        assert!(!p_without_g1.edges[0].target_readable);

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
                    principal: fixture.q,
                    start_memory_id: fixture.f1,
                    direction: MemoryLineageDirection::Descendants,
                    depth: 3,
                    limit: 20,
                    reader_personality_instance_id: None,
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
                    principal: fixture.p,
                    start_memory_id: lifecycle,
                    direction: MemoryLineageDirection::Ancestors,
                    depth: 2,
                    limit: 20,
                    reader_personality_instance_id: None,
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
                    principal: fixture.q,
                    start_memory_id: evidence,
                    direction: MemoryLineageDirection::Descendants,
                    depth: 2,
                    limit: 20,
                    reader_personality_instance_id: None,
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

async fn read_edge_by_id(
    pg: &proxima_storage_pg::PgStorage,
    read_owners: &[OwnerRef],
    principal: &OwnerRef,
    edge_id: EdgeId,
) -> Result<proxima_core::verbs::query::EdgeReadResponse, proxima_core::StorageError> {
    pg.read_edges(
        read_owners,
        &EdgeReadRequest {
            principal: *principal,
            edge_ids: vec![edge_id],
            filter: EdgeFilter::default(),
            limit: 10,
        },
    )
    .await
}

async fn edge_exists_by_id(
    pg: &proxima_storage_pg::PgStorage,
    read_owners: &[OwnerRef],
    principal: &OwnerRef,
    edge_id: EdgeId,
) -> Result<proxima_core::verbs::query::EdgeExistsResponse, proxima_core::StorageError> {
    pg.edge_exists(
        read_owners,
        &EdgeExistsRequest {
            principal: *principal,
            edge_ids: vec![edge_id],
            filter: EdgeFilter::default(),
        },
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
