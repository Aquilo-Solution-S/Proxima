//! Two managers remove each other at the same time (issue #476, item 2).
//! Both requests have passed the engine's manage check; a pause point parks
//! each one inside its transaction right before the group-tree lock, so both
//! reach the lock together and storage alone decides.

use proxima_core::storage_ports::{
    MembershipWritePermit, OwnerMembershipAdminPort, OwnerWritePermit,
};
use proxima_core::test_fixtures::owner_scope_for_tests;
use proxima_core::{AccessKind, GroupId, OwnerId, Relation, UserId};
use proxima_pg_testkit::{db_url, drop_db};
use uuid::Uuid;

use crate::PgStorage;
use crate::pause_point::{BEFORE_GROUP_LOCK, arm};
use crate::test_fixtures::create_core_db;

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
        let permit = OwnerWritePermit::new_for_tests(OwnerId::from(group), AccessKind::Fact);
        pg.add_group_member(
            &MembershipWritePermit::new_for_tests(&permit, None, None, None),
            group,
            OwnerId::from(m2),
            Relation::Admin,
            Uuid::now_v7(),
        )
        .await?;

        // One gate per removal: whichever request arrives first takes the
        // first gate, the other the second.
        let first_gate = arm(BEFORE_GROUP_LOCK, group.into_inner());
        let second_gate = arm(BEFORE_GROUP_LOCK, group.into_inner());
        let remove = |caller: UserId, member: UserId| {
            let pg = pg.clone();
            tokio::spawn(async move {
                let permit =
                    OwnerWritePermit::new_for_tests(OwnerId::from(group), AccessKind::Fact);
                pg.remove_group_member(
                    &MembershipWritePermit::new_for_tests(&permit, None, None, None)
                        .with_caller(caller),
                    group,
                    OwnerId::from(member),
                )
                .await
            })
        };
        let a = remove(m1, m2);
        let b = remove(m2, m1);
        // Both requests are parked before the lock at the same time.
        tokio::join!(first_gate.reached(), second_gate.reached());
        tokio::join!(first_gate.resume(), second_gate.resume());
        let (a, b) = (a.await?, b.await?);

        let succeeded = [a.is_ok(), b.is_ok()].iter().filter(|ok| **ok).count();
        assert_eq!(
            succeeded, 1,
            "exactly one removal succeeds; the other caller is no longer a manager"
        );
        let owner_scope = owner_scope_for_tests(&[group.into()]);
        let members = pg.list_group_members(Some(&owner_scope), group).await?;
        assert!(
            members
                .iter()
                .any(|(_, relation)| *relation == Relation::Admin),
            "the group keeps at least one manager"
        );
        Ok(())
    }
    .await;
    drop_db(&db_name).await.ok();
    result.expect("managers removing each other");
}

/// The zero-manager guard is unconditional: a host-internal write, which
/// names no caller, may not remove a group's last manager either.
#[tokio::test]
async fn host_removal_of_last_manager_refused() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        let group = GroupId::new(Uuid::now_v7());
        let manager = UserId::new(Uuid::now_v7());
        pg.bootstrap_group_admin(group, manager, manager.into_inner())
            .await?;
        let permit = OwnerWritePermit::new_for_tests(OwnerId::from(group), AccessKind::Fact);

        let refused = pg
            .remove_group_member(
                &MembershipWritePermit::new_for_tests(&permit, None, None, None),
                group,
                OwnerId::from(manager),
            )
            .await;
        assert!(
            matches!(refused, Err(proxima_core::StorageError::Conflict(_))),
            "a host write may not remove the last manager: {refused:?}"
        );
        let owner_scope = owner_scope_for_tests(&[group.into()]);
        let members = pg.list_group_members(Some(&owner_scope), group).await?;
        assert!(
            members
                .iter()
                .any(|(_, relation)| *relation == Relation::Admin),
            "the group keeps its manager"
        );
        Ok(())
    }
    .await;
    drop_db(&db_name).await.ok();
    result.expect("host removal of the last manager");
}
