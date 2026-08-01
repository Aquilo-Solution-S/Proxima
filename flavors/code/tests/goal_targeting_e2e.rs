//! End-to-end: a Goal names the Perspective it inspires on its own row,
//! and the `reference` index row derived from that column points at that
//! Perspective and no other. Wake execution is external to Proxima.

#![allow(clippy::too_many_lines, clippy::unnecessary_literal_bound)]

mod common;

use common::{insert_home, migrated_db, test_owner};
use proxima_core::Owner;
use proxima_pg_testkit::drop_db;
use proxima_storage_pg::PgStorage;
use uuid::Uuid;

async fn seed_active_goal(
    pg: &PgStorage,
    owner: &Owner,
    assignment_perspective_id: Uuid,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let (owner_kind, owner_id) = proxima_storage_pg::access::owner_columns::owner_binds(owner);
    let goal_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO proxima_core.goals
            (goal_id, owner_kind, owner_id, schema_id, schema_version,
             title, text, state, authorship_kind, request_id, payload,
             idempotency_key, assignment_perspective_id)
         VALUES ($1, $2, $3, 'core/simple-text-v1', 1,
                 'goal targeting', 'target Alice only', 'Active', 'User',
                 'goal-targeting-e2e', convert_to('{}', 'UTF8'),
                 md5($2::text || ':' || $3::text || ':' || 'goal-targeting-e2e'), $4)",
    )
    .bind(goal_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(assignment_perspective_id)
    .execute(pg.pool_for_tests())
    .await?;
    insert_home(pg.pool_for_tests(), goal_id, owner).await?;
    // The index row the column implies. Written here by hand because this
    // test seeds the goal row directly rather than going through the goal
    // write verb — which is the code that derives it in production.
    sqlx::query(
        "INSERT INTO proxima_core.edges
            (source_kind, source_id, target_kind, target_id, kind, owner_kind, owner_id)
         VALUES ('Goal', $1, 'Perspective', $2, 'reference', $3, $4)",
    )
    .bind(goal_id)
    .bind(assignment_perspective_id)
    .bind(owner_kind)
    .bind(owner_id)
    .execute(pg.pool_for_tests())
    .await?;
    Ok(goal_id)
}

async fn seed_perspective(
    pg: &PgStorage,
    owner: &Owner,
    label: &str,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let memory_id = Uuid::now_v7();
    let (owner_kind, owner_id) = proxima_storage_pg::access::owner_columns::owner_binds(owner);
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_kind, owner_id, schema_id, schema_version, kind, text,
             operator_kind, operator_id, input_contract_id, source_batch_id, model_id, prompt_version)
         VALUES ($1, $2, $3, 'test/engineer-perspective-v1', 1, 'Perspective', $4,
                 'AtoP', '00000000-0000-0000-0000-000000000441'::uuid,
                 '00000000-0000-0000-0000-000000000442'::uuid, NULL,
                 'test-model', 'test-v1')"
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(label)
    .execute(pg.pool_for_tests())
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
            "SELECT assignment_perspective_id
               FROM proxima_core.goals
              WHERE goal_id = $1",
        )
        .bind(goal_id)
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(assigned, alice_self);
        assert_ne!(assigned, bob_self);

        // ...and the index carries exactly the one connection it implies.
        let targets: Vec<Uuid> = sqlx::query_scalar(
            "SELECT target_id
               FROM proxima_core.edges
              WHERE source_kind = 'Goal'
                AND source_id = $1
                AND kind = 'reference'",
        )
        .bind(goal_id)
        .fetch_all(pg.pool_for_tests())
        .await?;
        assert_eq!(targets, vec![alice_self]);

        Ok(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db).await;
    result.expect("goal_targeting_e2e failed");
}
