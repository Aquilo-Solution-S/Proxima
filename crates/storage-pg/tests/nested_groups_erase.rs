use proxima_core::owner_inverse::{
    EraseAuthorization, OwnerEraseOutcome, OwnerEraseRefusal, OwnerEraseTarget, OwnerSurfaces,
};
use proxima_core::storage_ports::OwnerInversePort;
use proxima_core::{FlavorRegistry, GroupId, OwnerId, Relation, UserId};
use proxima_pg_testkit::{db_url, drop_db};
use proxima_storage_pg::test_fixtures::{create_core_db, register_group_for_tests};
use proxima_storage_pg::{PgStorage, add_group_member};
use uuid::Uuid;

fn surfaces() -> OwnerSurfaces {
    OwnerSurfaces::for_registry(&FlavorRegistry::new().freeze_or_panic_for_tests())
}

#[tokio::test]
async fn nested_group_erasure_refuses_parent_and_removes_child_edge()
-> Result<(), Box<dyn std::error::Error>> {
    let name = format!("proxima_test_nested_erase_{}", Uuid::now_v7().simple());
    create_core_db(&name).await.expect("create test database");
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(db_url(&name)).await?;
        let parent = GroupId::new(Uuid::now_v7());
        let child = GroupId::new(Uuid::now_v7());
        register_group_for_tests(pg.pool_for_tests(), parent).await?;
        register_group_for_tests(pg.pool_for_tests(), child).await?;
        let mut tx = pg.pool_for_tests().begin().await?;
        add_group_member(&mut tx, parent, OwnerId::from(child), Relation::Viewer).await?;
        tx.commit().await?;

        let parent_auth =
            EraseAuthorization::new_for_tests(OwnerEraseTarget::GroupOwner { group_id: parent });
        let refused = pg
            .erase_group_owner(&parent_auth, parent, &surfaces())
            .await?;
        assert!(matches!(
            refused,
            OwnerEraseOutcome::Refused {
                reason: OwnerEraseRefusal::OwnerNotAbandoned,
                ..
            }
        ));

        let child_auth =
            EraseAuthorization::new_for_tests(OwnerEraseTarget::GroupOwner { group_id: child });
        let erased = pg
            .erase_group_owner(&child_auth, child, &surfaces())
            .await?;
        assert!(matches!(erased, OwnerEraseOutcome::Completed { .. }));
        let edge_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM proxima_core.group_edges WHERE child_group_id = $1",
        )
        .bind(child.into_inner())
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(edge_count, 0);
        Ok(())
    }
    .await;
    drop_db(&name).await.ok();
    result
}

#[tokio::test]
async fn erasing_person_removes_projected_roles() -> Result<(), Box<dyn std::error::Error>> {
    let name = format!("proxima_test_person_erase_{}", Uuid::now_v7().simple());
    create_core_db(&name).await.expect("create test database");
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(db_url(&name)).await?;
        let group = GroupId::new(Uuid::now_v7());
        let person = UserId::new(Uuid::now_v7());
        register_group_for_tests(pg.pool_for_tests(), group).await?;
        let mut tx = pg.pool_for_tests().begin().await?;
        add_group_member(&mut tx, group, OwnerId::from(person), Relation::Viewer).await?;
        tx.commit().await?;
        let auth = EraseAuthorization::new_for_tests(OwnerEraseTarget::PersonalOwner {
            user_id: person,
            drop_event_id: "nested-groups-test".into(),
        });
        let erased = pg.erase_personal_owner(&auth, person, &surfaces()).await?;
        assert!(matches!(erased, OwnerEraseOutcome::Completed { .. }));
        let rows: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM proxima_core.group_role_projection WHERE member_user_id = $1",
        )
        .bind(person.into_inner())
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(rows, 0);
        Ok(())
    }
    .await;
    drop_db(&name).await.ok();
    result
}
