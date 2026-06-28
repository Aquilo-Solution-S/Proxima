//! End-to-end: when a `core/inspires` edge points at a specific
//! engineer instance's `self-Perspective`, the edge targets only that
//! root perspective. Wake execution is external to Proxima.

#![allow(clippy::too_many_lines, clippy::unnecessary_literal_bound)]

use std::sync::Arc;

mod common;

use common::{insert_home, migrated_db, test_owner};
use proxima_code::{build_engine_with, register_repo};
use proxima_core::personality::{InstantiatePersonalityRequest, PersonalityInstanceId};
use proxima_core::test_fixtures::ConstantEmbedding;
use proxima_core::{AuthPath, AuthzContext, CORE_INSPIRES_RELATION, Owner};
use proxima_pg_testkit::drop_db;
use proxima_storage_pg::PgStorage;
use uuid::Uuid;

async fn author_inspires_edge(
    pg: &PgStorage,
    owner: &Owner,
    source_goal_id: Uuid,
    target_memory_id: Uuid,
) -> Result<(), Box<dyn std::error::Error>> {
    let (owner_kind, owner_principal_id) = owner.columns();
    let edge_id = Uuid::now_v7();
    let mut tx = pg.pool().begin().await?;
    sqlx::query(
        "INSERT INTO proxima_core.edges
            (edge_id, relation, relation_class,
             source_kind, source_memory_id, source_goal_id,
             target_kind, target_memory_id, target_goal_id,
             authorship_kind)
         VALUES ($1, $2, 'Causal', 'Goal', NULL, $3, 'Perspective', $4, NULL,
                 'User')",
    )
    .bind(edge_id)
    .bind(CORE_INSPIRES_RELATION)
    .bind(source_goal_id)
    .bind(target_memory_id)
    .execute(&mut *tx)
    .await?;

    let seq = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO proxima_core.change_event
            (seq, owner_principal_kind, owner_principal_id, kind,
             edge_id, edge_relation,
             edge_source_goal_id,
             edge_target_memory_id)
         VALUES ($1, $2, $3, 'EdgeAppend', $4, $5,
                 $6, $7)",
    )
    .bind(seq)
    .bind(owner_kind)
    .bind(owner_principal_id)
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
    let (owner_kind, owner_principal_id) = owner.columns();
    let goal_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO proxima_core.goals
            (goal_id, schema_id, schema_version,
             title, text, state, authorship_kind, request_id, payload,
             idempotency_key)
         VALUES ($1, 'core/simple-text-v1', 1,
                 'goal targeting', 'target Alice only', 'Active', 'User',
                 'goal-targeting-e2e', convert_to('{}', 'UTF8'),
                 md5($2::text || ':' || $3::text || ':' || 'goal-targeting-e2e'))",
    )
    .bind(goal_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .execute(pg.pool())
    .await?;
    insert_home(pg.pool(), goal_id, owner).await?;
    Ok(goal_id)
}

async fn self_perspective_for(
    pg: &PgStorage,
    instance_id: PersonalityInstanceId,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let id: Uuid = sqlx::query_scalar(
        "SELECT current_root_perspective_memory_id
         FROM proxima_core.personality
         WHERE personality_instance_id = $1",
    )
    .bind(instance_id.into_inner())
    .fetch_one(pg.pool())
    .await?;
    Ok(id)
}

#[tokio::test(flavor = "multi_thread")]
async fn inspires_edge_targets_only_intended_engineer_instance() {
    let (db, pg) = migrated_db().await;

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = test_owner();
        let repo_id = Uuid::now_v7();
        register_repo(pg.pool(), &owner, repo_id, "/tmp/goal-targeting-e2e", "e2e").await?;

        // Build an engine WITHOUT auto-instantiating other personalities
        // — we only want engineers under test. The commit-summary
        // personality is also registered (proxima_flavor!), so we
        // accept its inert personality row but won't author commit-fact events
        // until needed.
        let authz = AuthzContext::single_owner(&owner, AuthPath::System);
        let engine = build_engine_with(pg.clone(), |_registry| {})
            .with_embed(Arc::new(ConstantEmbedding::zero("fake-embed")));

        // Provision Alice + Bob (two engineer instances).
        let alice = engine
            .instantiate_personality(
                &authz,
                InstantiatePersonalityRequest {
                    principal: owner,
                    display_name: "Alice".into(),
                },
            )
            .await?;
        let bob = engine
            .instantiate_personality(
                &authz,
                InstantiatePersonalityRequest {
                    principal: owner,
                    display_name: "Bob".into(),
                },
            )
            .await?;
        let alice_self = self_perspective_for(&pg, alice.instance_id).await?;
        let bob_self = self_perspective_for(&pg, bob.instance_id).await?;
        assert_ne!(alice_self, bob_self);

        // Author an inspires edge from an active Goal -> Alice's
        // self-Perspective.
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
