//! End-to-end: when a `core/inspires` edge points at a specific
//! Perspective, the edge targets only that Perspective. Wake execution is
//! external to Proxima.

#![allow(clippy::too_many_lines, clippy::unnecessary_literal_bound)]

mod common;

use common::{insert_home, migrated_db, test_owner};
use proxima_core::{CORE_INSPIRES_RELATION, Owner};
use proxima_pg_testkit::drop_db;
use proxima_storage_pg::PgStorage;
use uuid::Uuid;

async fn author_inspires_edge(
    pg: &PgStorage,
    owner: &Owner,
    source_goal_id: Uuid,
    target_memory_id: Uuid,
) -> Result<(), Box<dyn std::error::Error>> {
    let (owner_kind, owner_id) = proxima_storage_pg::access::owner_columns::owner_binds(owner);
    let edge_id = Uuid::now_v7();
    let mut tx = pg.pool().begin().await?;
    sqlx::query(
        "INSERT INTO proxima_core.edges
            (edge_id, owner_kind, owner_id, relation, relation_class,
             source_kind, source_memory_id, source_goal_id,
             target_kind, target_memory_id, target_goal_id,
             authorship_kind)
         VALUES ($1, $2, $3, $4, 'Causal', 'Goal', NULL, $5, 'Perspective', $6, NULL,
                 'User')",
    )
    .bind(edge_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(CORE_INSPIRES_RELATION)
    .bind(source_goal_id)
    .bind(target_memory_id)
    .execute(&mut *tx)
    .await?;

    let seq = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO proxima_core.change_event
            (seq, owner_kind, owner_id, kind,
             edge_id, edge_relation,
             edge_source_goal_id,
             edge_target_memory_id)
         VALUES ($1, $2, $3, 'EdgeAppend', $4, $5,
                 $6, $7)",
    )
    .bind(seq)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(edge_id)
    .bind(CORE_INSPIRES_RELATION)
    .bind(source_goal_id)
    .bind(target_memory_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

async fn seed_active_goal(
    pg: &PgStorage,
    owner: &Owner,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let (owner_kind, owner_id) = proxima_storage_pg::access::owner_columns::owner_binds(owner);
    let goal_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO proxima_core.goals
            (goal_id, owner_kind, owner_id, schema_id, schema_version,
             title, text, state, authorship_kind, request_id, payload,
             idempotency_key)
         VALUES ($1, $2, $3, 'core/simple-text-v1', 1,
                 'goal targeting', 'target Alice only', 'Active', 'User',
                 'goal-targeting-e2e', convert_to('{}', 'UTF8'),
                 md5($2::text || ':' || $3::text || ':' || 'goal-targeting-e2e'))",
    )
    .bind(goal_id)
    .bind(owner_kind)
    .bind(owner_id)
    .execute(pg.pool())
    .await?;
    insert_home(pg.pool(), goal_id, owner).await?;
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
             operator_kind, model_id, prompt_version)
         VALUES ($1, $2, $3, 'test/engineer-perspective-v1', 1, 'Perspective', $4,
                 'AtoP', 'test-model', 'test-v1')",
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(label)
    .execute(pg.pool())
    .await?;
    Ok(memory_id)
}

#[tokio::test(flavor = "multi_thread")]
async fn inspires_edge_targets_only_intended_engineer_instance() {
    let (db, pg) = migrated_db().await;

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = test_owner();
        let alice_self = seed_perspective(&pg, &owner, "Alice").await?;
        let bob_self = seed_perspective(&pg, &owner, "Bob").await?;
        assert_ne!(alice_self, bob_self);

        // Author an inspires edge from an active Goal -> Alice's Perspective.
        let goal_id = seed_active_goal(&pg, &owner).await?;
        author_inspires_edge(&pg, &owner, goal_id, alice_self).await?;

        let target: Uuid = sqlx::query_scalar(
            "SELECT edge_target_memory_id
             FROM proxima_core.change_event
             WHERE kind = 'EdgeAppend'
               AND edge_relation = $1
               AND edge_source_goal_id = $2",
        )
        .bind(CORE_INSPIRES_RELATION)
        .bind(goal_id)
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(target, alice_self);
        assert_ne!(target, bob_self);

        Ok(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db).await;
    result.expect("goal_targeting_e2e failed");
}
