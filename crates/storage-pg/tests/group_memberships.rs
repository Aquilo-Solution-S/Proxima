//! `list_group_members` against the v0.0.8 table (no `created_at`).

use proxima_core::storage_ports::{OwnerMembershipAdminPort, OwnerWritePermit};
use proxima_core::{AccessKind, GroupId, OwnerRef, Relation, UserId};
use proxima_pg_testkit::{create_db, db_url, drop_db};
use proxima_storage_pg::PgStorage;
use uuid::Uuid;

#[tokio::test]
async fn list_group_members_runs_against_v008_schema() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;

        let group = GroupId::new(Uuid::now_v7());
        let admin = UserId::new(Uuid::now_v7());
        let editor = UserId::new(Uuid::now_v7());
        pg.bootstrap_group_admin(group, admin, admin.into_inner())
            .await?;

        let permit = OwnerWritePermit::new_for_tests(OwnerRef::Group(group), AccessKind::Fact);
        pg.add_group_member(&permit, group, editor, Relation::Editor, Uuid::now_v7())
            .await?;

        let members = pg.list_group_members(group).await?;
        assert_eq!(members.len(), 2, "admin + editor");
        assert!(members.contains(&(admin, Relation::Admin)));
        assert!(members.contains(&(editor, Relation::Editor)));

        let page = pg.list_group_members_page(group, None, 10).await?;
        assert_eq!(page, members, "full list matches first page");
        Ok(())
    }
    .await;

    drop_db(&db_name).await.ok();
    result.expect("list_group_members must run against v0.0.8 group_memberships");
}
