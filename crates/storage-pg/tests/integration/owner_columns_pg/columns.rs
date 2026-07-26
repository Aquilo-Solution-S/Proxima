//! Owner columns themselves: the migration shape and the columns written on fact and goal writes.

use super::{
    assert_no_live_entity_lacks_home, assert_single_home, fresh_fact_draft, fresh_goal_draft,
    insert_self, owner_write_permit,
};

use crate::common;
use proxima_core::storage_ports::*;
use proxima_core::verbs::goal_write::{
    CreateGoalAtomicRequest, GoalAssignmentTarget, GoalAtomicContext, GoalTopologyWrite,
};
use proxima_core::{FlavorRegistry, OwnerRef, UserId};
use uuid::Uuid;

#[tokio::test]
async fn migration_creates_owner_columns_and_membership() {
    let (pg, db) = common::fresh_pg().await;
    let pool = pg.pool_for_tests();

    let (n,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM information_schema.tables
          WHERE table_schema='proxima_core'
            AND table_name IN ('group_memberships')",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(n, 1, "group membership table exists");
    let stale_owner_table: Option<String> =
        sqlx::query_scalar("SELECT to_regclass('proxima_core.' || 'entity_' || 'owner')::text")
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(stale_owner_table, None);

    common::drop_db(&db).await.unwrap();
}

#[tokio::test]
async fn owner_columns_written_on_fact_ingest() {
    let (pg, db) = common::fresh_pg().await;
    let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
    let permit = owner_write_permit(&owner, proxima_core::AccessKind::Fact).await;

    let outcome = pg
        .ingest_fact_atomic(&permit, &fresh_fact_draft(owner), None)
        .await
        .unwrap();

    assert_single_home(&pg, outcome.memory_id.into_inner(), &owner).await;
    assert_no_live_entity_lacks_home(&pg).await;

    common::drop_db(&db).await.unwrap();
}

#[tokio::test]
async fn owner_columns_written_on_goal_create() {
    let (pg, db) = common::fresh_pg().await;
    let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
    let self_id = insert_self(&pg, &owner).await;
    let registry = FlavorRegistry::new().freeze_or_panic_for_tests();
    let mut draft = fresh_goal_draft(owner);
    let permit = owner_write_permit(&owner, proxima_core::AccessKind::Goal).await;
    draft.topology = GoalTopologyWrite::new(
        GoalAssignmentTarget::perspective(self_id),
        Vec::new(),
        Vec::new(),
    )
    .expect("empty test topology is valid");

    let outcome = pg
        .create_goal_atomic(
            &CreateGoalAtomicRequest {
                draft,
                context: GoalAtomicContext {
                    registry: &registry,
                    embedding_model_id: None,
                    author_self_perspective_id: Some(self_id),
                },
            },
            &permit,
        )
        .await
        .unwrap();

    assert_single_home(&pg, outcome.goal_id.into_inner(), &owner).await;
    assert_no_live_entity_lacks_home(&pg).await;

    common::drop_db(&db).await.unwrap();
}
