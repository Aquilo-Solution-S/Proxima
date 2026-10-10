//! The role probes and the relation change under enforced owner RLS.
//!
//! The probes grant nothing the connection does not already have: a
//! platform-scoped transaction reads every group, an owner scope only the
//! groups its policy lets through. The relation change writes under the same
//! `manage` scope `add_member` does.

use std::sync::Arc;

use proxima_core::{
    Credentials, Engine, FlavorRegistry, GroupId, OwnerRef, OwnerRoles, Relation, Role, UserId,
};
use proxima_storage_pg::{
    PgPlatformScope, PgStorage, begin_owner_transaction, group_role, roles_for_subject,
};
use uuid::Uuid;

/// The runtime role's connection URL: the test database's, with its credentials.
fn role_url(database: &str, role: &str, password: &str) -> String {
    let url = proxima_pg_testkit::db_url(database);
    let (scheme, rest) = url.split_once("://").expect("scheme");
    let (_credentials, host) = rest.rsplit_once('@').expect("credentials");
    format!("{scheme}://{role}:{password}@{host}")
}

async fn seed(admin: &sqlx::PgPool, group: GroupId, member: UserId, relations: &[Relation]) {
    sqlx::query("INSERT INTO proxima_core.owners(owner_id, kind) VALUES ($1, 'group') ON CONFLICT DO NOTHING")
        .bind(group.into_inner())
        .execute(admin)
        .await
        .unwrap();
    for relation in relations {
        sqlx::query(
            "INSERT INTO proxima_core.group_memberships(group_id, member_user_id, relation)
             VALUES ($1, $2, $3)",
        )
        .bind(group.into_inner())
        .bind(member.into_inner())
        .bind(relation)
        .execute(admin)
        .await
        .unwrap();
    }
}

async fn relations(admin: &sqlx::PgPool, group: GroupId, member: UserId) -> Vec<Relation> {
    sqlx::query_scalar(
        "SELECT relation FROM proxima_core.group_memberships
          WHERE group_id = $1 AND member_user_id = $2 ORDER BY relation",
    )
    .bind(group.into_inner())
    .bind(member.into_inner())
    .fetch_all(admin)
    .await
    .unwrap()
}

#[tokio::test]
async fn role_probes_read_what_the_connections_scope_lets_through() {
    let (database, admin, runtime, platform, platform_role, runtime_role, _password) =
        super::setup_full_schema().await;
    let subject = UserId::new(Uuid::now_v7());
    let visible = GroupId::new(Uuid::now_v7());
    let hidden = GroupId::new(Uuid::now_v7());
    seed(&admin, visible, subject, &[Relation::Editor]).await;
    seed(
        &admin,
        hidden,
        subject,
        &[Relation::Viewer, Relation::Ingest],
    )
    .await;

    // Platform scope: every group, the join included.
    let scope = PgPlatformScope::new(platform.clone(), &["proxima_core", "proxima_code"])
        .await
        .expect("platform scope");
    let mut tx = scope.begin().await.unwrap();
    let all = roles_for_subject(&mut tx, subject).await.unwrap();
    assert_eq!(
        all.role_for(&OwnerRef::Group(visible)),
        Some(Role::editor())
    );
    assert_eq!(
        all.role_for(&OwnerRef::Group(hidden)),
        Some(Role::viewer().join(Role::ingest()))
    );
    assert_eq!(
        group_role(&mut tx, subject, hidden).await.unwrap(),
        Some(Role::viewer().join(Role::ingest()))
    );
    tx.rollback().await.unwrap();

    // Owner scope narrowed to one group: the other group's rows are not seen,
    // by either probe.
    let verifier = super::SyntheticVerifier {
        roles: OwnerRoles::for_subject(
            subject,
            [
                (OwnerRef::Group(visible), Role::editor()),
                (OwnerRef::Group(hidden), Role::viewer()),
            ],
        )
        .unwrap(),
    };
    let authz = proxima_core::authenticate(&verifier, &Credentials::Bearer("probe".into()))
        .await
        .unwrap();
    let narrowed = authz.narrowed_to_owner(OwnerRef::Group(visible)).unwrap();
    let mut tx = begin_owner_transaction(&runtime, narrowed.owner_scope().unwrap())
        .await
        .unwrap();
    let seen = roles_for_subject(&mut tx, subject).await.unwrap();
    assert_eq!(
        seen.role_for(&OwnerRef::Group(visible)),
        Some(Role::editor())
    );
    assert_eq!(
        seen.role_for(&OwnerRef::Group(hidden)),
        None,
        "a group the scope cannot read is not seen"
    );
    assert_eq!(group_role(&mut tx, subject, hidden).await.unwrap(), None);
    assert_eq!(
        group_role(&mut tx, subject, visible).await.unwrap(),
        Some(Role::editor())
    );
    tx.rollback().await.unwrap();

    runtime.close().await;
    platform.close().await;
    super::cleanup(&database, admin, &platform_role, &runtime_role).await;
}

#[tokio::test]
async fn relation_change_commits_under_the_managing_owner_scope() {
    let (database, admin, runtime, platform, platform_role, runtime_role, password) =
        super::setup_full_schema().await;
    let manager = UserId::new(Uuid::now_v7());
    let member = UserId::new(Uuid::now_v7());
    let group = GroupId::new(Uuid::now_v7());
    seed(&admin, group, manager, &[Relation::Admin]).await;
    seed(&admin, group, member, &[Relation::Admin, Relation::Ingest]).await;

    let pg = PgStorage::connect(&role_url(&database, &runtime_role, &password))
        .await
        .expect("runtime storage");
    let engine = Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests())
        .with_storage_ports(Arc::new(pg).storage_ports());
    let verifier = super::SyntheticVerifier {
        roles: OwnerRoles::for_subject(manager, [(OwnerRef::Group(group), Role::admin())]).unwrap(),
    };
    let authz = proxima_core::authenticate(&verifier, &Credentials::Bearer("manage".into()))
        .await
        .unwrap();

    engine
        .replace_member_relation(&authz, group, member, Relation::Admin, Relation::Viewer)
        .await
        .expect("replace under the manage scope");
    assert_eq!(
        relations(&admin, group, member).await,
        vec![Relation::Viewer, Relation::Ingest]
    );
    engine
        .remove_member_relation(&authz, group, member, Relation::Ingest)
        .await
        .expect("remove one relation under the manage scope");
    assert_eq!(
        relations(&admin, group, member).await,
        vec![Relation::Viewer]
    );

    // Not holding `from` is a refusal, not an RLS-filtered silent success.
    let err = engine
        .replace_member_relation(&authz, group, member, Relation::Admin, Relation::Editor)
        .await
        .expect_err("the member no longer holds Admin");
    assert_eq!(err.code, proxima_core::ErrorCode::InvalidArgument);
    assert_eq!(
        relations(&admin, group, member).await,
        vec![Relation::Viewer]
    );

    runtime.close().await;
    platform.close().await;
    super::cleanup(&database, admin, &platform_role, &runtime_role).await;
}
