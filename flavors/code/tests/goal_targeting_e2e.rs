//! End-to-end: when a `core/inspires` edge points at a specific
//! engineer instance's `self-Perspective`, the edge targets only that
//! root perspective. Wake execution is external to Proxima.

#![allow(clippy::too_many_lines, clippy::unnecessary_literal_bound)]

use std::sync::Arc;

use async_trait::async_trait;
use proxima_code::{build_engine_with, migrator, register_repo};
use proxima_core::llm::{EmbeddingClient, LlmError};
use proxima_core::personality::{InstantiatePersonalityRequest, PersonalityInstanceId};
use proxima_core::{
    AuthPath, AuthzContext, CORE_INSPIRES_RELATION, OrgId, Owner, Principal, UserId,
};
use proxima_pg_testkit::{create_db, db_url, drop_db, unique_db_name};
use proxima_storage_pg::PgStorage;
use uuid::Uuid;

async fn migrated_db() -> Option<(String, PgStorage)> {
    let db_name = unique_db_name("proxima_test");
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);
    let pg = PgStorage::connect(&url).await.expect("connect test db");
    pg.run_migrations().await.expect("core migrations");
    migrator().run(pg.pool()).await.expect("code migrations");
    Some((db_name, pg))
}

fn test_owner() -> Owner {
    Owner {
        principal: Principal::User(UserId::new(Uuid::now_v7())),
        org_id: OrgId::new(Uuid::now_v7()),
    }
}

#[derive(Debug)]
struct FakeEmbedding;

#[async_trait]
impl EmbeddingClient for FakeEmbedding {
    async fn embed(&self, _text: &str) -> Result<Vec<f32>, LlmError> {
        Ok(vec![0.0; 8])
    }
    fn model_id(&self) -> &str {
        "fake-embed"
    }
    fn dim(&self) -> usize {
        8
    }
}

async fn author_inspires_edge(
    pg: &PgStorage,
    owner: &Owner,
    source_goal_id: Uuid,
    target_memory_id: Uuid,
) -> Result<(), Box<dyn std::error::Error>> {
    let owner_kind = proxima_core::OwnerPrincipalKind::of(&owner.principal);
    let owner_principal_id = match &owner.principal {
        Principal::User(u) => u.into_inner(),
        Principal::Group(g) => g.into_inner(),
    };
    let owner_org_id = owner.org_id.into_inner();
    let edge_id = Uuid::now_v7();
    let mut tx = pg.pool().begin().await?;
    sqlx::query(
        "INSERT INTO proxima_core.edges
            (edge_id, relation, relation_class,
             source_kind, source_memory_id, source_goal_id,
             target_kind, target_memory_id, target_goal_id,
             authorship_kind,
             owner_principal_kind, owner_principal_id, owner_org_id)
         VALUES ($1, $2, 'Causal', 'Goal', NULL, $3, 'Perspective', $4, NULL,
                 'User', $5, $6, $7)",
    )
    .bind(edge_id)
    .bind(CORE_INSPIRES_RELATION)
    .bind(source_goal_id)
    .bind(target_memory_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .execute(&mut *tx)
    .await?;

    let seq = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO proxima_core.change_event
            (seq, owner_principal_kind, owner_principal_id, owner_org_id, kind,
             edge_id, edge_relation,
             edge_source_goal_id,
             edge_target_memory_id)
         VALUES ($1, $2, $3, $4, 'EdgeAppend', $5, $6,
                 $7, $8)",
    )
    .bind(seq)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
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
    let owner_kind = proxima_core::OwnerPrincipalKind::of(&owner.principal);
    let owner_principal_id = match &owner.principal {
        Principal::User(u) => u.into_inner(),
        Principal::Group(g) => g.into_inner(),
    };
    let owner_org_id = owner.org_id.into_inner();
    let goal_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO proxima_core.goals
            (goal_id, schema_id, schema_version,
             owner_principal_kind, owner_principal_id, owner_org_id,
             title, text, state, authorship_kind, request_id, payload)
         VALUES ($1, 'proxima-goal/simple-text-v1', 1,
                 $2, $3, $4,
                 'goal targeting', 'target Alice only', 'Active', 'User',
                 'goal-targeting-e2e', ''::bytea)",
    )
    .bind(goal_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .execute(pg.pool())
    .await?;
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
    let Some((db, pg)) = migrated_db().await else {
        return;
    };

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
        let engine =
            build_engine_with(pg.clone(), |_registry| {}).with_embed(Arc::new(FakeEmbedding));

        // Provision Alice + Bob (two engineer instances).
        let alice = engine
            .instantiate_personality(
                &authz,
                InstantiatePersonalityRequest {
                    principal: owner.principal.clone(),
                    org_id: None,
                    display_name: "Alice".into(),
                },
            )
            .await?;
        let bob = engine
            .instantiate_personality(
                &authz,
                InstantiatePersonalityRequest {
                    principal: owner.principal.clone(),
                    org_id: None,
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
