use crate::common::{drop_db, fresh_pg, seed_memory, seed_memory_edge, share_entity};
use proxima_core::access::world;
use proxima_core::{
    CORE_DERIVED_FROM_RELATION, EdgeId, EntityKind, GroupId, MemoryId, Principal, RelationClass,
    Storage, UserId,
};
use uuid::Uuid;

struct EdgeAccessFixture {
    p_read: Vec<Principal>,
    q_read: Vec<Principal>,
    p_without_g1_read: Vec<Principal>,
    world_read: Vec<Principal>,
    a: MemoryId,
    f1: MemoryId,
    f2: MemoryId,
    a_public: MemoryId,
    a_to_f1: EdgeId,
    a_to_f2: EdgeId,
    public_to_private: EdgeId,
}

#[tokio::test]
async fn neighbor_edges_are_source_owned_and_targets_redact()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;

    let result = async {
        let fixture = seed_edge_access_fixture(&pg).await?;

        let q_from_f1 = pg
            .load_neighbor_memory_edges(&fixture.q_read, &[fixture.f1], 100)
            .await?;
        assert!(
            q_from_f1.iter().all(|edge| edge.edge_id != fixture.a_to_f1),
            "readers of F1 must not discover unreadable source A"
        );

        let p_from_a = pg
            .load_neighbor_memory_edges(&fixture.p_read, &[fixture.a], 100)
            .await?;
        let a_to_f1 = p_from_a
            .iter()
            .find(|edge| edge.edge_id == fixture.a_to_f1)
            .expect("P sees A to F1");
        let a_to_f2 = p_from_a
            .iter()
            .find(|edge| edge.edge_id == fixture.a_to_f2)
            .expect("P sees A to F2");
        assert!(a_to_f1.target_readable);
        assert!(a_to_f2.target_readable);
        assert_eq!(a_to_f1.target_memory_id, Some(fixture.f1));
        assert_eq!(a_to_f2.target_memory_id, Some(fixture.f2));

        let p_without_g1_from_a = pg
            .load_neighbor_memory_edges(&fixture.p_without_g1_read, &[fixture.a], 100)
            .await?;
        let redacted = p_without_g1_from_a
            .iter()
            .find(|edge| edge.edge_id == fixture.a_to_f1)
            .expect("readable source keeps target handle");
        assert!(!redacted.target_readable);
        assert_eq!(redacted.target_memory_id, Some(fixture.f1));
        assert!(!redacted.source_world_readable);
        assert!(
            p_without_g1_from_a
                .iter()
                .any(|edge| edge.edge_id == fixture.a_to_f2 && edge.target_readable),
            "same-source readable target remains full"
        );

        let world_from_public = pg
            .load_neighbor_memory_edges(&fixture.world_read, &[fixture.a_public], 100)
            .await?;
        assert!(
            world_from_public
                .iter()
                .all(|edge| edge.edge_id != fixture.public_to_private),
            "World-readable source with private target is omitted"
        );

        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db_name).await;
    result
}

async fn seed_edge_access_fixture(
    pg: &proxima_storage_pg::PgStorage,
) -> Result<EdgeAccessFixture, Box<dyn std::error::Error>> {
    let p = Principal::User(UserId::new(Uuid::now_v7()));
    let q = Principal::User(UserId::new(Uuid::now_v7()));
    let gp = Principal::Group(GroupId::new(Uuid::now_v7()));
    let g1 = Principal::Group(GroupId::new(Uuid::now_v7()));
    let private = Principal::Group(GroupId::new(Uuid::now_v7()));
    let world = world();

    let f1 = seed_memory(pg, &g1, EntityKind::Fact, "F1").await?;
    let f2 = seed_memory(pg, &gp, EntityKind::Fact, "F2").await?;
    let a = seed_memory(pg, &gp, EntityKind::Abstraction, "A").await?;
    let a_public = seed_memory(pg, &gp, EntityKind::Abstraction, "A public").await?;
    let f_private = seed_memory(pg, &private, EntityKind::Fact, "private").await?;
    share_entity(pg, a_public.into_inner(), &world).await?;

    let a_to_f1 = seed_memory_edge(
        pg,
        &gp,
        (EntityKind::Abstraction, a),
        (EntityKind::Fact, f1),
        CORE_DERIVED_FROM_RELATION,
        RelationClass::Provenance,
    )
    .await?;
    let a_to_f2 = seed_memory_edge(
        pg,
        &gp,
        (EntityKind::Abstraction, a),
        (EntityKind::Fact, f2),
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

    Ok(EdgeAccessFixture {
        p_read: vec![p.clone(), gp.clone(), g1.clone(), world.clone()],
        q_read: vec![q, g1, world.clone()],
        p_without_g1_read: vec![p, gp, world.clone()],
        world_read: vec![world],
        a,
        f1,
        f2,
        a_public,
        a_to_f1,
        a_to_f2,
        public_to_private,
    })
}
