use crate::common::{drop_db, fresh_pg, seed_memory, seed_memory_edge, share_entity};
use proxima_core::access::world;
use proxima_core::{
    CORE_DERIVED_FROM_RELATION, ChangeEventKind, EdgeId, EntityKind, GroupId, MemoryId, OwnerRef,
    RelationClass, Storage, UserId,
};
use uuid::Uuid;

struct EdgeAccessFixture {
    p_read: Vec<OwnerRef>,
    q_read: Vec<OwnerRef>,
    p_without_g1_read: Vec<OwnerRef>,
    world_read: Vec<OwnerRef>,
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

/// H1 regression: `proxima://events` / `list_change_events_after` must apply
/// the same source-owned visibility + public-source guard as `read_edges`,
/// or it leaks a private edge target the redacted edge-read surface hides.
#[tokio::test]
async fn list_change_events_applies_public_source_guard_to_edge_events()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;

    let result = async {
        let gp = OwnerRef::Group(GroupId::new(Uuid::now_v7())); // source (edge) owner
        let private = OwnerRef::Group(GroupId::new(Uuid::now_v7())); // target owner
        let p = OwnerRef::Personal(UserId::new(Uuid::now_v7())); // reads gp, not private
        let world = world();

        let a_public = seed_memory(&pg, &gp, EntityKind::Abstraction, "A public").await?;
        let f_private = seed_memory(&pg, &private, EntityKind::Fact, "private").await?;
        share_entity(&pg, a_public.into_inner(), &world).await?; // publish the source

        let edge = seed_memory_edge(
            &pg,
            &gp,
            (EntityKind::Abstraction, a_public),
            (EntityKind::Fact, f_private),
            CORE_DERIVED_FROM_RELATION,
            RelationClass::Provenance,
        )
        .await?;
        // The edge's change-event is owned by the source/edge owner (gp).
        insert_edge_append_event(&pg, &gp, edge, a_public, f_private).await?;

        // P reaches gp (passes the owner filter + reads the World-published
        // source) but cannot read the `private` target. `read_edges` omits this
        // edge via the public-source guard, so the event surface must too.
        let p_read = vec![p, gp, world];
        let p_events = pg
            .list_change_events_after(&p_read, uuid::Uuid::nil(), 100)
            .await?;
        assert!(
            !p_events.iter().any(|e| matches!(
                &e.event.kind,
                ChangeEventKind::EdgeAppend { edge_id, .. } if *edge_id == edge.into_inner()
            )),
            "public-source/private-target edge event must be omitted (public-source guard)"
        );

        // A reader of BOTH owners sees the event (target readable → guard off).
        let both_read = vec![gp, private, world];
        let both_events = pg
            .list_change_events_after(&both_read, uuid::Uuid::nil(), 100)
            .await?;
        assert!(
            both_events.iter().any(|e| matches!(
                &e.event.kind,
                ChangeEventKind::EdgeAppend { edge_id, .. } if *edge_id == edge.into_inner()
            )),
            "reader of both endpoints sees the edge event"
        );

        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn list_change_events_for_replay_applies_public_source_guard_to_edge_events()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;

    let result = async {
        let gp = OwnerRef::Group(GroupId::new(Uuid::now_v7()));
        let private = OwnerRef::Group(GroupId::new(Uuid::now_v7()));
        let world = world();

        let a_public = seed_memory(&pg, &gp, EntityKind::Abstraction, "A public").await?;
        let f_private = seed_memory(&pg, &private, EntityKind::Fact, "private").await?;
        share_entity(&pg, a_public.into_inner(), &world).await?;

        let edge = seed_memory_edge(
            &pg,
            &gp,
            (EntityKind::Abstraction, a_public),
            (EntityKind::Fact, f_private),
            CORE_DERIVED_FROM_RELATION,
            RelationClass::Provenance,
        )
        .await?;
        insert_edge_append_event(&pg, &gp, edge, a_public, f_private).await?;

        let replay_events = pg
            .list_change_events_for_replay(&gp, uuid::Uuid::nil(), None, 100)
            .await?;
        assert!(
            !replay_events.iter().any(|e| matches!(
                &e.event.kind,
                ChangeEventKind::EdgeAppend { edge_id, .. } if *edge_id == edge.into_inner()
            )),
            "replay must omit public-source/private-target edge event"
        );

        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db_name).await;
    result
}

async fn insert_edge_append_event(
    pg: &proxima_storage_pg::PgStorage,
    owner: &OwnerRef,
    edge_id: EdgeId,
    source_memory_id: MemoryId,
    target_memory_id: MemoryId,
) -> Result<(), sqlx::Error> {
    let (owner_kind, owner_principal_id) = owner.columns();
    sqlx::query(
        "INSERT INTO proxima_core.change_event \
            (seq, owner_principal_kind, owner_principal_id, kind, \
             edge_id, edge_relation, \
             edge_source_memory_id, edge_source_goal_id, edge_source_fact_entity_id, \
             edge_target_memory_id, edge_target_goal_id, edge_target_fact_entity_id) \
         VALUES ($1, $2, $3, 'EdgeAppend', $4, $5, $6, NULL, NULL, $7, NULL, NULL)",
    )
    .bind(uuid::Uuid::now_v7())
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(edge_id.into_inner())
    .bind(CORE_DERIVED_FROM_RELATION)
    .bind(source_memory_id.into_inner())
    .bind(target_memory_id.into_inner())
    .execute(pg.pool())
    .await
    .map(|_| ())
}

async fn seed_edge_access_fixture(
    pg: &proxima_storage_pg::PgStorage,
) -> Result<EdgeAccessFixture, Box<dyn std::error::Error>> {
    let p = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
    let q = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
    let gp = OwnerRef::Group(GroupId::new(Uuid::now_v7()));
    let g1 = OwnerRef::Group(GroupId::new(Uuid::now_v7()));
    let private = OwnerRef::Group(GroupId::new(Uuid::now_v7()));
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
        p_read: vec![p, gp, g1, world],
        q_read: vec![q, g1, world],
        p_without_g1_read: vec![p, gp, world],
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
