//! First-admin bootstrap: single admission, operator authority, and its conflict paths.

use super::{
    admin_member_count, admin_members, assert_bootstrap_conflict, authz_with_role,
    owner_write_permit, system_authz,
};

use crate::common;
use proxima_core::storage_ports::*;
use proxima_core::{
    AuthPath, AuthzContext, Engine, ErrorCode, FlavorRegistry, GroupId, OwnerAccessPort, OwnerRef,
    Relation, Role, UserId,
};
use proxima_storage_pg::PgOwnerAccessResolver;
use std::sync::Arc;
use uuid::Uuid;

#[tokio::test]
async fn group_membership_bootstrap_first_admin_succeeds_once_and_conflicts_second() {
    let (pg, db) = common::fresh_pg().await;
    let group = GroupId::new(Uuid::now_v7());
    let first_admin = UserId::new(Uuid::now_v7());
    let second_admin = UserId::new(Uuid::now_v7());
    let engine = Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests())
        .with_storage_ports(Arc::new(pg.clone()).storage_ports());
    let bootstrap_authz = system_authz();

    engine
        .bootstrap_group_admin(&bootstrap_authz, group, first_admin)
        .await
        .expect("fresh group admits exactly one first admin");
    assert_eq!(admin_members(&pg, group).await, vec![first_admin]);

    let err = engine
        .bootstrap_group_admin(&bootstrap_authz, group, second_admin)
        .await
        .expect_err("second bootstrap must conflict");
    assert_bootstrap_conflict(&err);
    assert_eq!(admin_members(&pg, group).await, vec![first_admin]);

    common::drop_db(&db).await.unwrap();
}

#[tokio::test]
async fn group_membership_bootstrap_requires_operator_authority() {
    let (pg, db) = common::fresh_pg().await;
    let group = GroupId::new(Uuid::now_v7());
    let first_admin = UserId::new(Uuid::now_v7());
    let engine = Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests())
        .with_storage_ports(Arc::new(pg.clone()).storage_ports());
    let unapproved = AuthzContext::for_subject(first_admin, AuthPath::HostBearer);

    let err = engine
        .bootstrap_group_admin(&unapproved, group, first_admin)
        .await
        .expect_err("non-System, non-approved caller must not bootstrap group admin");
    assert_eq!(err.code, ErrorCode::Forbidden);
    assert!(
        admin_members(&pg, group).await.is_empty(),
        "denied bootstrap must not insert a membership row"
    );

    common::drop_db(&db).await.unwrap();
}

#[tokio::test]
async fn group_membership_bootstrap_conflicts_after_admin_added_via_add_member() {
    let (pg, db) = common::fresh_pg().await;
    let group = GroupId::new(Uuid::now_v7());
    let seeded_admin = UserId::new(Uuid::now_v7());
    let added_admin = UserId::new(Uuid::now_v7());
    let bootstrap_candidate = UserId::new(Uuid::now_v7());
    let seeded_admin_owner = OwnerRef::Personal(seeded_admin);
    let engine = Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests())
        .with_storage_ports(Arc::new(pg.clone()).storage_ports());

    let group_owner = OwnerRef::Group(group);
    let permit = owner_write_permit(&group_owner, proxima_core::AccessKind::Goal).await;
    pg.add_group_member(
        &permit,
        group,
        seeded_admin,
        Relation::Admin,
        Uuid::now_v7(),
    )
    .await
    .expect("seed prerequisite admin");
    engine
        .add_member(
            &authz_with_role(&seeded_admin_owner, OwnerRef::Group(group), Role::admin()),
            group,
            added_admin,
            Relation::Admin,
        )
        .await
        .expect("existing admin can add a later admin through normal path");

    let err = engine
        .bootstrap_group_admin(&system_authz(), group, bootstrap_candidate)
        .await
        .expect_err("bootstrap must conflict once any admin exists");
    assert_bootstrap_conflict(&err);
    assert_eq!(admin_member_count(&pg, group).await, 2);

    common::drop_db(&db).await.unwrap();
}

#[tokio::test]
async fn group_membership_bootstrap_concurrent_calls_admit_single_admin() {
    let (pg, db) = common::fresh_pg().await;
    let group = GroupId::new(Uuid::now_v7());
    let first_admin = UserId::new(Uuid::now_v7());
    let second_admin = UserId::new(Uuid::now_v7());
    let engine = Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests())
        .with_storage_ports(Arc::new(pg.clone()).storage_ports());
    let authz = system_authz();

    let first = engine.bootstrap_group_admin(&authz, group, first_admin);
    let second = engine.bootstrap_group_admin(&authz, group, second_admin);
    let (first_result, second_result) = tokio::join!(first, second);
    let results = [first_result, second_result];

    assert_eq!(
        results.iter().filter(|result| result.is_ok()).count(),
        1,
        "exactly one concurrent bootstrap call must succeed"
    );
    let conflicts = results
        .iter()
        .filter_map(|result| result.as_ref().err())
        .collect::<Vec<_>>();
    assert_eq!(
        conflicts.len(),
        1,
        "exactly one concurrent bootstrap call must conflict"
    );
    assert_bootstrap_conflict(conflicts[0]);
    assert_eq!(admin_member_count(&pg, group).await, 1);

    common::drop_db(&db).await.unwrap();
}

#[tokio::test]
async fn group_membership_bootstrap_preserves_later_add_member_authz_path() {
    let (pg, db) = common::fresh_pg().await;
    let group = GroupId::new(Uuid::now_v7());
    let first_admin = UserId::new(Uuid::now_v7());
    let later_viewer = UserId::new(Uuid::now_v7());
    let engine = Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests())
        .with_storage_ports(Arc::new(pg.clone()).storage_ports());

    engine
        .bootstrap_group_admin(&system_authz(), group, first_admin)
        .await
        .expect("fresh group bootstrap succeeds");
    let resolver = PgOwnerAccessResolver::new(pg.pool_for_tests().clone());
    let roles = resolver
        .resolve_roles_for_subject(first_admin)
        .await
        .expect("bootstrapped admin resolves through membership storage");
    let authz = AuthzContext::server_resolved(roles, AuthPath::HostBearer);

    engine
        .add_member(&authz, group, later_viewer, Relation::Viewer)
        .await
        .expect("bootstrapped admin can use normal add_member path afterward");
    assert!(
        pg.list_group_members(group)
            .await
            .expect("list memberships")
            .contains(&(later_viewer, Relation::Viewer))
    );

    common::drop_db(&db).await.unwrap();
}
