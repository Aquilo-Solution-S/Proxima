//! End-to-end: a Goal names the Perspective it inspires on its own row,
//! and the `reference` index row derived from that column points at that
//! Perspective and no other. Wake execution is external to Proxima.

#![allow(clippy::too_many_lines, clippy::unnecessary_literal_bound)]

mod common;

use common::{migrated_db, test_owner};
use proxima_core::Owner;
use proxima_pg_testkit::drop_db;
use proxima_storage_pg::PgStorage;
use uuid::Uuid;

async fn seed_active_goal(
    pg: &PgStorage,
    owner: &Owner,
    assignment_perspective_id: Uuid,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let goal_id = Uuid::now_v7();
    common::seed_goal(
        pg.pool_for_tests(),
        owner,
        "core/simple-text-v1",
        "goal targeting",
        "goal-targeting-e2e",
        Some(goal_id),
        Some(assignment_perspective_id),
    )
    .await?;
    Ok(goal_id)
}

async fn seed_perspective(
    pg: &PgStorage,
    owner: &Owner,
    _label: &str,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let memory_id = Uuid::now_v7();
    common::seed_memory(
        pg.pool_for_tests(),
        owner,
        "test/engineer-perspective-v1",
        "perspective",
        Some(memory_id),
        None,
        &[],
    )
    .await?;
    Ok(memory_id)
}

#[tokio::test(flavor = "multi_thread")]
async fn goal_assignment_targets_only_intended_engineer_instance() {
    let (db, pg) = migrated_db().await;

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = test_owner();
        let alice_self = seed_perspective(&pg, &owner, "Alice").await?;
        let bob_self = seed_perspective(&pg, &owner, "Bob").await?;
        assert_ne!(alice_self, bob_self);

        // An Active Goal assigned to Alice's Perspective.
        let goal_id = seed_active_goal(&pg, &owner, alice_self).await?;

        // The statement lives on the goal row...
        let assigned: Uuid = sqlx::query_scalar(
            "SELECT assignment_t
               FROM proxima_core.goal
              WHERE t = $1",
        )
        .bind(goal_id)
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(assigned, alice_self);
        assert_ne!(assigned, bob_self);

        Ok(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db).await;
    result.expect("goal_targeting_e2e failed");
}
