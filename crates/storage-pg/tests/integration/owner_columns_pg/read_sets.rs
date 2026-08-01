//! Read-set filtering and owner-role resolution.

use super::{
    fresh_fact_draft, move_home_row, owner_write_permit, read_owners, seed_abstraction_memory,
    seed_edge_between_memories, seed_membership,
};

use crate::common;
use proxima_core::storage_ports::*;
use proxima_core::verbs::query::{
    MemorySearchRequest, QueryRequest, SearchMode, SearchOrder, SupersessionStatus, TagMatch,
};
use proxima_core::{GroupId, OwnerAccessPort, OwnerRef, Relation, UserId};
use proxima_storage_pg::PgOwnerAccessResolver;
use uuid::Uuid;

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn discovery_reads_filter_by_owner_read_set() {
    let (pg, db) = common::fresh_pg().await;
    let p = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));
    let q = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));
    let g1 = GroupId::new(uuid::Uuid::now_v7());

    seed_membership(&pg, g1, &q, Relation::Viewer).await;

    let mut f1_draft = fresh_fact_draft(OwnerRef::Group(g1));
    f1_draft.rendered_text = Some("boundaryneedle group fact".to_string());
    let g1_owner = OwnerRef::Group(g1);
    let fact_permit = owner_write_permit(&g1_owner, proxima_core::AccessKind::Fact).await;
    let f1 = pg
        .ingest_fact_atomic(&fact_permit, &f1_draft, None)
        .await
        .unwrap()
        .memory_id;
    let mut hidden_target_draft = fresh_fact_draft(OwnerRef::Group(g1));
    hidden_target_draft.rendered_text = Some("unreadable edge target".to_string());
    let hidden_target = pg
        .ingest_fact_atomic(&fact_permit, &hidden_target_draft, None)
        .await
        .unwrap()
        .memory_id;
    move_home_row(&pg, hidden_target, &p).await;
    let a = seed_abstraction_memory(
        &pg,
        &p,
        OwnerRef::Group(g1),
        "boundaryneedle personal abstraction",
    )
    .await;
    seed_edge_between_memories(&pg, OwnerRef::Group(g1), f1, hidden_target).await;
    let q_read_owners = read_owners(&pg, &q).await;

    let query = pg
        .query_memories(
            &QueryRequest {
                owner: q,
                read_owners: q_read_owners.clone(),
                entity_kind: None,
                schema_id: None,
                supersession: SupersessionStatus::HeadsOnly,
                tombstones: proxima_core::verbs::query::TombstoneFilter::PresentOnly,
                goal_state: None,
                limit: 50,
                page: proxima_core::verbs::query::QueryPage::default(),
                include_payloads: false,
                memory_ids: Vec::new(),
                goal_ids: Vec::new(),
                stateful_heads: Vec::new(),
            },
            &[],
        )
        .await
        .unwrap();
    let query_ids = query.memories.iter().map(|row| row.id).collect::<Vec<_>>();
    assert!(query_ids.contains(&f1));
    assert!(
        !query_ids.contains(&hidden_target),
        "Q must not query P-owned target Fact"
    );
    assert!(!query_ids.contains(&a), "Q must not query P's singleton A");
    assert!(
        query
            .edges
            .iter()
            .all(|edge| edge.target.endpoint().map(|target| target.entity)
                != Some(proxima_core::EntityRef::Memory(hidden_target))),
        "query_memories must not return an edge whose target is unreadable"
    );

    let search = pg
        .search_memories(
            &MemorySearchRequest {
                owner: q,
                read_owners: q_read_owners,
                query: "boundaryneedle".to_string(),
                mode: SearchMode::Lexical,
                supersession: SupersessionStatus::HeadsOnly,
                limit: 10,
                kind: None,
                schema_id: None,
                tags: Vec::new(),
                tag_match: TagMatch::Any,
                since: None,
                until: None,
                order: SearchOrder::Relevance,
                min_score: None,
                semantic_weight: None,
                after: None,
                query_embedding: None,
                embedding_model_id: None,
            },
            &[],
        )
        .await
        .unwrap();
    let search_ids = search
        .results
        .iter()
        .map(|row| row.memory_id)
        .collect::<Vec<_>>();
    assert!(search_ids.contains(&f1));
    assert!(
        !search_ids.contains(&a),
        "Q must not search P's singleton A"
    );

    common::drop_db(&db).await.unwrap();
}

#[tokio::test]
async fn pg_owner_access_resolver_resolves_group_roles_via_owner_access_port() {
    let (pg, db) = common::fresh_pg().await;
    let subject = UserId::new(Uuid::now_v7());
    let group = GroupId::new(Uuid::now_v7());

    seed_membership(&pg, group, &OwnerRef::Personal(subject), Relation::Editor).await;

    let resolver = PgOwnerAccessResolver::new(pg.pool_for_tests().clone());
    let roles = resolver
        .resolve_roles_for_subject(subject)
        .await
        .expect("resolves group roles");

    assert!(roles.may_write(
        &OwnerRef::Group(group),
        proxima_core::AccessKind::Perspective
    ));
    assert!(!roles.may_manage(&OwnerRef::Group(group)));
    assert!(roles.may_read(&OwnerRef::World, proxima_core::AccessKind::Goal));

    common::drop_db(&db).await.unwrap();
}

#[tokio::test]
async fn pg_owner_access_resolver_resolve_roles_is_empty_for_unknown_subject() {
    let (pg, db) = common::fresh_pg().await;
    let subject = UserId::new(Uuid::now_v7());

    let resolver = PgOwnerAccessResolver::new(pg.pool_for_tests().clone());
    let roles = resolver
        .resolve_roles_for_subject(subject)
        .await
        .expect("resolves with no membership rows");

    assert!(
        roles
            .writable_owners(proxima_core::AccessKind::Perspective)
            .iter()
            .all(|owner| !matches!(owner, OwnerRef::Group(_)))
    );

    common::drop_db(&db).await.unwrap();
}

#[tokio::test]
async fn has_role_for_owner_is_a_positive_and_negative_point_in_time_probe() {
    let (pg, db) = common::fresh_pg().await;
    let admin = UserId::new(Uuid::now_v7());
    let viewer = UserId::new(Uuid::now_v7());
    let group = GroupId::new(Uuid::now_v7());
    let other_group = GroupId::new(Uuid::now_v7());

    seed_membership(&pg, group, &OwnerRef::Personal(admin), Relation::Admin).await;
    seed_membership(&pg, group, &OwnerRef::Personal(viewer), Relation::Viewer).await;

    let resolver = PgOwnerAccessResolver::new(pg.pool_for_tests().clone());

    assert!(
        resolver
            .has_role_for_owner(admin, OwnerRef::Group(group), Relation::Admin)
            .await
            .unwrap()
    );
    assert!(
        !resolver
            .has_role_for_owner(admin, OwnerRef::Group(group), Relation::Editor)
            .await
            .unwrap(),
        "exact-relation probe must not treat Admin as satisfying an Editor probe"
    );
    assert!(
        !resolver
            .has_role_for_owner(viewer, OwnerRef::Group(group), Relation::Admin)
            .await
            .unwrap()
    );
    assert!(
        !resolver
            .has_role_for_owner(admin, OwnerRef::Group(other_group), Relation::Admin)
            .await
            .unwrap(),
        "membership on a different group must not satisfy the probe"
    );
    assert!(
        !resolver
            .has_role_for_owner(admin, OwnerRef::Personal(admin), Relation::Admin)
            .await
            .unwrap(),
        "Personal owners carry no membership row; the probe fails closed"
    );
    assert!(
        !resolver
            .has_role_for_owner(admin, OwnerRef::World, Relation::Viewer)
            .await
            .unwrap(),
        "World carries no membership row; the probe fails closed"
    );

    common::drop_db(&db).await.unwrap();
}
