//! Group membership: visibility, paging, and the admin/editor gates on the membership verbs.

use super::{
    authz_with_role, granted_authz, owner_write_permit, read_owners, seed_membership,
    seed_memory_owned,
};

use crate::common;
use proxima_core::storage_ports::*;
use proxima_core::{Engine, ErrorCode, FlavorRegistry, GroupId, OwnerRef, Relation, Role, UserId};
use std::sync::Arc;
use uuid::Uuid;

#[tokio::test]
async fn visible_to_any_respects_membership() {
    let (pg, db) = common::fresh_pg().await;
    let p = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));
    let q = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));
    let g1 = GroupId::new(uuid::Uuid::now_v7());

    seed_membership(&pg, g1, &p, Relation::Viewer).await;
    seed_membership(&pg, g1, &q, Relation::Viewer).await;
    let f1 = seed_memory_owned(&pg, OwnerRef::Group(g1)).await;
    let a = seed_memory_owned(&pg, p).await;
    let s_p = read_owners(&pg, &p).await;
    let s_q = read_owners(&pg, &q).await;

    assert!(pg.visible_to_any(f1, &s_p).await.unwrap());
    assert!(pg.visible_to_any(a, &s_p).await.unwrap());
    assert!(pg.visible_to_any(f1, &s_q).await.unwrap());
    assert!(
        !pg.visible_to_any(a, &s_q).await.unwrap(),
        "A is personal to P"
    );

    common::drop_db(&db).await.unwrap();
}

/// Keyset pages over `(member_user_id, relation)` are disjoint,
/// exhaustive, and terminate.
#[tokio::test]
async fn list_group_members_page_walks_membership_exactly_once() {
    let (pg, db) = common::fresh_pg().await;
    let group = GroupId::new(uuid::Uuid::now_v7());
    let group_owner = OwnerRef::Group(group);
    let permit = owner_write_permit(&group_owner, proxima_core::AccessKind::Goal).await;

    let mut expected = Vec::new();
    for _ in 0..5 {
        let member = UserId::new(uuid::Uuid::now_v7());
        pg.add_group_member(&permit, group, member, Relation::Viewer, Uuid::now_v7())
            .await
            .unwrap();
        expected.push((member, Relation::Viewer));
    }
    expected.sort_unstable_by_key(|(member, _)| member.into_inner());

    let mut walked = Vec::new();
    let mut after = None;
    let mut pages = 0;
    loop {
        let page = pg.list_group_members_page(group, after, 3).await.unwrap();
        pages += 1;
        assert!(
            pages <= 3,
            "five members at page size two walk in three rounds"
        );
        let full = page.len() == 3;
        walked.extend(page.iter().copied().take(2));
        if full {
            after = Some(page[1]);
        } else {
            break;
        }
    }
    // 5 members walked as 2 + 2 + 1 with an over-fetch row proving
    // continuation each round.
    assert_eq!(pages, 3, "expected three pages of two");
    walked.sort_unstable_by_key(|(member, _)| member.into_inner());
    assert_eq!(walked, expected, "pages cover every member exactly once");

    common::drop_db(&db).await.unwrap();
}

#[tokio::test]
async fn group_membership_verbs_round_trip_and_engine_gates_admin_editor() {
    let (pg, db) = common::fresh_pg().await;
    let admin = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));
    let viewer = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));
    let outsider = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));
    let group = GroupId::new(uuid::Uuid::now_v7());
    let OwnerRef::Personal(admin_id) = admin else {
        unreachable!("admin is user")
    };
    let OwnerRef::Personal(viewer_id) = viewer else {
        unreachable!("viewer is user")
    };
    let OwnerRef::Personal(outsider_id) = outsider else {
        unreachable!("outsider is user")
    };

    let group_owner = OwnerRef::Group(group);
    let permit = owner_write_permit(&group_owner, proxima_core::AccessKind::Goal).await;
    pg.add_group_member(&permit, group, viewer_id, Relation::Viewer, Uuid::now_v7())
        .await
        .unwrap();
    assert_eq!(
        pg.list_group_members(group).await.unwrap(),
        vec![(viewer_id, Relation::Viewer)]
    );

    let group_entity = seed_memory_owned(&pg, OwnerRef::Group(group)).await;
    let viewer_read_owners = read_owners(&pg, &viewer).await;
    assert!(
        pg.visible_to_any(group_entity, &viewer_read_owners)
            .await
            .unwrap(),
        "viewer membership enters S_read and reaches group-owned entity"
    );

    pg.remove_group_member(&permit, group, viewer_id)
        .await
        .unwrap();
    assert!(pg.list_group_members(group).await.unwrap().is_empty());

    pg.add_group_member(&permit, group, admin_id, Relation::Admin, Uuid::now_v7())
        .await
        .unwrap();
    pg.add_group_member(&permit, group, viewer_id, Relation::Viewer, Uuid::now_v7())
        .await
        .unwrap();
    let engine = Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests())
        .with_storage_ports(Arc::new(pg.clone()).storage_ports());
    let outsider_err = engine
        .add_member(&granted_authz(&outsider), group, admin_id, Relation::Viewer)
        .await
        .expect_err("non-admin add_member must be forbidden");
    assert_eq!(outsider_err.code, ErrorCode::Forbidden);

    engine
        .add_member(
            &authz_with_role(&admin, OwnerRef::Group(group), Role::admin()),
            group,
            outsider_id,
            Relation::Viewer,
        )
        .await
        .unwrap();
    assert!(
        engine
            .list_members(
                &authz_with_role(&admin, OwnerRef::Group(group), Role::admin(),),
                group,
                200,
                None,
            )
            .await
            .unwrap()
            .members
            .contains(&(outsider_id, Relation::Viewer))
    );
    engine
        .remove_member(
            &authz_with_role(&admin, OwnerRef::Group(group), Role::admin()),
            group,
            outsider_id,
        )
        .await
        .unwrap();
    assert!(
        !engine
            .list_members(
                &authz_with_role(&admin, OwnerRef::Group(group), Role::admin(),),
                group,
                200,
                None,
            )
            .await
            .unwrap()
            .members
            .iter()
            .any(|(member, _)| *member == outsider_id)
    );

    common::drop_db(&db).await.unwrap();
}
