//! `list_group_members` against the shipped table (no `created_at`).

use proxima_core::storage_ports::{MembershipWritePermit, OwnerMembershipAdminPort};
use proxima_core::test_fixtures::owner_scope_for_tests;
use proxima_core::{GroupId, OwnerId, Relation, UserId};
use proxima_pg_testkit::{db_url, drop_db};
use proxima_storage_pg::PgStorage;
use proxima_storage_pg::test_fixtures::create_core_db;
use uuid::Uuid;

#[tokio::test]
async fn list_group_members_runs_against_v008_schema() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        let group = GroupId::new(Uuid::now_v7());
        let admin = UserId::new(Uuid::now_v7());
        let editor = UserId::new(Uuid::now_v7());
        pg.bootstrap_group_admin(group, admin, admin.into_inner())
            .await?;

        let permit = proxima_core::storage_ports::OwnerWritePermit::new_for_tests(
            OwnerId::from(group),
            proxima_core::AccessKind::Fact,
        );
        pg.add_group_member(
            &MembershipWritePermit::new_for_tests(&permit, None, None, None),
            group,
            OwnerId::from(editor),
            Relation::Editor,
            Uuid::now_v7(),
        )
        .await?;

        let owner_scope = owner_scope_for_tests(&[group.into()]);
        let members = pg.list_group_members(Some(&owner_scope), group).await?;
        assert_eq!(members.len(), 2, "admin + editor");
        assert!(members.contains(&(OwnerId::from(admin), Relation::Admin)));
        assert!(members.contains(&(OwnerId::from(editor), Relation::Editor)));

        let page = pg
            .list_group_members_page(Some(&owner_scope), group, None, 10)
            .await?;
        assert_eq!(page, members, "full list matches first page");
        Ok(())
    }
    .await;

    drop_db(&db_name).await.ok();
    result.expect("list_group_members must run against the shipped group_memberships");
}

/// Two managers remove each other. Both requests passed the engine's manage
/// check before either reached storage; the storage write runs them one
/// after the other under the group-tree lock. The second must re-read its
/// caller's role there and be refused, and the group must keep a manager.
#[tokio::test]
async fn managers_removing_each_other_one_refused() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        let group = GroupId::new(Uuid::now_v7());
        let m1 = UserId::new(Uuid::now_v7());
        let m2 = UserId::new(Uuid::now_v7());
        pg.bootstrap_group_admin(group, m1, m1.into_inner()).await?;
        let permit = proxima_core::storage_ports::OwnerWritePermit::new_for_tests(
            OwnerId::from(group),
            proxima_core::AccessKind::Fact,
        );
        pg.add_group_member(
            &MembershipWritePermit::new_for_tests(&permit, None, None, None),
            group,
            OwnerId::from(m2),
            Relation::Admin,
            Uuid::now_v7(),
        )
        .await?;

        let first = pg
            .remove_group_member(
                &MembershipWritePermit::new_for_tests(&permit, None, None, None).with_caller(m1),
                group,
                OwnerId::from(m2),
            )
            .await;
        let second = pg
            .remove_group_member(
                &MembershipWritePermit::new_for_tests(&permit, None, None, None).with_caller(m2),
                group,
                OwnerId::from(m1),
            )
            .await;
        assert!(first.is_ok(), "the first removal stands: {first:?}");
        assert!(
            second.is_err(),
            "the second caller is no longer a manager and is refused"
        );

        let owner_scope = owner_scope_for_tests(&[group.into()]);
        let members = pg.list_group_members(Some(&owner_scope), group).await?;
        assert!(
            members
                .iter()
                .any(|(_, relation)| *relation == Relation::Admin),
            "the group keeps a manager"
        );
        Ok(())
    }
    .await;
    drop_db(&db_name).await.ok();
    result.expect("managers removing each other");
}
