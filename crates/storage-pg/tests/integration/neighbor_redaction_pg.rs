use crate::common::{drop_db, fresh_pg, seed_memory, seed_memory_edge};
use proxima_core::access::world;
use proxima_core::verbs::query::EdgeTargetProjection;
use proxima_core::{
    CORE_DERIVED_FROM_RELATION, ChangeEventKind, EdgeId, EntityKind, EntityRef, GroupId, MemoryId,
    OwnerRef, RelationClass, UserId,
};
use proxima_core::{ChangeEventPort, MemoryReadPort};
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
        assert!(matches!(
            a_to_f1.target,
            EdgeTargetProjection::Visible { target } if target == EntityRef::Memory(fixture.f1)
        ));
        assert!(matches!(
            a_to_f2.target,
            EdgeTargetProjection::Visible { target } if target == EntityRef::Memory(fixture.f2)
        ));

        let p_without_g1_from_a = pg
            .load_neighbor_memory_edges(&fixture.p_without_g1_read, &[fixture.a], 100)
            .await?;
        let redacted = p_without_g1_from_a
            .iter()
            .find(|edge| edge.edge_id == fixture.a_to_f1)
            .expect("readable source keeps target handle");
        assert_eq!(redacted.target, EdgeTargetProjection::Redacted);
        assert!(
            p_without_g1_from_a.iter().any(|edge| matches!(
                edge.target,
                EdgeTargetProjection::Visible { target }
                    if edge.edge_id == fixture.a_to_f2
                        && target == EntityRef::Memory(fixture.f2)
            )),
            "same-source readable target remains full"
        );

        let world_from_group_source = pg
            .load_neighbor_memory_edges(&fixture.world_read, &[fixture.a_public], 100)
            .await?;
        assert!(
            world_from_group_source
                .iter()
                .all(|edge| edge.edge_id != fixture.public_to_private),
            "World-only reader cannot read a group-owned source edge"
        );

        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db_name).await;
    result
}

/// H1 regression: `proxima://change-events` / `list_change_events_after` must apply
/// the same source-owned visibility + source-owned visibility as `read_edges`,
/// or it leaks a private edge target the redacted edge-read surface hides.
#[tokio::test]
async fn list_change_events_surfaces_readable_non_world_source_edge_events()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;

    let result = async {
        let gp = OwnerRef::Group(GroupId::new(Uuid::now_v7())); // source (edge) owner
        let private = OwnerRef::Group(GroupId::new(Uuid::now_v7())); // target owner
        let p = OwnerRef::Personal(UserId::new(Uuid::now_v7())); // reads gp, not private
        let a_public = seed_memory(&pg, &gp, EntityKind::Abstraction, "A public").await?;
        let f_private = seed_memory(&pg, &private, EntityKind::Fact, "private").await?;

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

        // P reaches gp (passes the owner filter + reads the source) but cannot
        // read the `private` target. Non-world source-owned edge events remain
        // visible; target readability is enforced by the reader's owner set.
        let p_read = vec![p, gp];
        let p_events = pg
            .list_change_events_after(&p_read, uuid::Uuid::nil(), 100)
            .await?;
        assert!(
            p_events.iter().any(|e| matches!(
                &e.event.kind,
                ChangeEventKind::EdgeAppend { edge_id, .. } if edge_id == &edge.into_inner()
            )),
            "source-readable non-world edge event must be visible"
        );

        // A reader of BOTH owners sees the event too.
        let both_read = vec![gp, private];
        let both_events = pg
            .list_change_events_after(&both_read, uuid::Uuid::nil(), 100)
            .await?;
        assert!(
            both_events.iter().any(|e| matches!(
                &e.event.kind,
                ChangeEventKind::EdgeAppend { edge_id, .. } if edge_id == &edge.into_inner()
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
async fn list_change_events_for_replay_surfaces_source_owned_edge_events()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;

    let result = async {
        let gp = OwnerRef::Group(GroupId::new(Uuid::now_v7()));
        let private = OwnerRef::Group(GroupId::new(Uuid::now_v7()));
        let a_public = seed_memory(&pg, &gp, EntityKind::Abstraction, "A public").await?;
        let f_private = seed_memory(&pg, &private, EntityKind::Fact, "private").await?;

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
            replay_events.iter().any(|e| matches!(
                &e.event.kind,
                ChangeEventKind::EdgeAppend { edge_id, .. } if edge_id == &edge.into_inner()
            )),
            "replay for the source owner sees non-world edge events"
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
    let (owner_kind, owner_id) = proxima_storage_pg::access::owner_columns::owner_binds(owner);
    sqlx::query(
        "INSERT INTO proxima_core.change_event \
            (seq, owner_kind, owner_id, kind, \
             edge_id, edge_relation, \
             edge_source_memory_id, edge_source_goal_id, edge_source_fact_entity_id, \
             edge_target_memory_id, edge_target_goal_id, edge_target_fact_entity_id) \
         VALUES ($1, $2, $3, 'EdgeAppend', $4, $5, $6, NULL, NULL, $7, NULL, NULL)",
    )
    .bind(uuid::Uuid::now_v7())
    .bind(owner_kind)
    .bind(owner_id)
    .bind(edge_id.into_inner())
    .bind(CORE_DERIVED_FROM_RELATION)
    .bind(source_memory_id.into_inner())
    .bind(target_memory_id.into_inner())
    .execute(pg.pool_for_tests())
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
