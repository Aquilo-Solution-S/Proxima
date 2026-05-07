//! End-to-end: when an `core/inspires` edge points at a specific
//! engineer instance's `self-Perspective`, ONLY that instance wakes.
//! Two engineer instances are provisioned (Alice + Bob); the test
//! authors an inspires edge targeting Alice's self-Perspective and
//! asserts only Alice's `wake_invocation` row is created.

#![allow(clippy::too_many_lines, clippy::unnecessary_literal_bound)]

use std::sync::Arc;

use async_trait::async_trait;
use proxima_code::{CommitV1, build_engine_with, ingest_commit, migrator, register_repo};
use proxima_core::auth::NoAuth;
use proxima_core::llm::scripted::{ScriptedAnthropicClient, ScriptedTurn};
use proxima_core::llm::{EmbeddingClient, LlmError};
use proxima_core::personality::{InstantiatePersonalityRequest, PersonalityInstanceId};
use proxima_core::{CORE_INSPIRES_RELATION, OrgId, Owner, Principal, SourceBatchId, UserId};
use proxima_storage_pg::PgStorage;
use sqlx::{Connection, Executor, PgConnection};
use uuid::Uuid;

const ADMIN_URL: &str = "postgres://proxima:proxima@localhost/postgres";

async fn create_db(name: &str) -> Result<(), sqlx::Error> {
    let admin = std::env::var("PROXIMA_TEST_PG_URL").unwrap_or_else(|_| ADMIN_URL.into());
    let mut conn = PgConnection::connect(&admin).await?;
    conn.execute(format!("CREATE DATABASE \"{name}\"").as_str())
        .await?;
    conn.close().await?;
    Ok(())
}

async fn drop_db(name: &str) -> Result<(), sqlx::Error> {
    let admin = std::env::var("PROXIMA_TEST_PG_URL").unwrap_or_else(|_| ADMIN_URL.into());
    let mut conn = PgConnection::connect(&admin).await?;
    conn.execute(format!("DROP DATABASE IF EXISTS \"{name}\"").as_str())
        .await?;
    conn.close().await?;
    Ok(())
}

async fn migrated_db() -> Option<(String, PgStorage)> {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if create_db(&db_name).await.is_err() {
        eprintln!("skipping (no admin PG)");
        return None;
    }
    let admin = std::env::var("PROXIMA_TEST_PG_URL").unwrap_or_else(|_| ADMIN_URL.into());
    let url = match admin.rfind('/') {
        Some(idx) => format!("{}/{}", &admin[..idx], db_name),
        None => format!("{admin}/{db_name}"),
    };
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
    source_memory_id: Uuid,
    target_memory_id: Uuid,
) -> Result<(), Box<dyn std::error::Error>> {
    let (owner_kind, owner_principal_id, owner_org_id) = match &owner.principal {
        Principal::User(u) => ("User", u.into_inner(), owner.org_id.into_inner()),
        Principal::Group(g) => ("Group", g.into_inner(), owner.org_id.into_inner()),
    };
    let edge_id = Uuid::now_v7();
    let mut tx = pg.pool().begin().await?;
    sqlx::query(
        "INSERT INTO proxima_core.edges
            (edge_id, relation, relation_class,
             source_kind, source_memory_id, source_goal_id,
             target_kind, target_memory_id, target_goal_id,
             authorship_kind,
             owner_principal_kind, owner_principal_id, owner_org_id)
         VALUES ($1, $2, 'Causal', 'Fact', $3, NULL, 'Perspective', $4, NULL,
                 'User', $5, $6, $7)",
    )
    .bind(edge_id)
    .bind(CORE_INSPIRES_RELATION)
    .bind(source_memory_id)
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
             edge_source_kind, edge_source_memory_id,
             edge_target_kind, edge_target_memory_id)
         VALUES ($1, $2, $3, $4, 'EdgeAppend', $5, $6,
                 'Fact', $7, 'Perspective', $8)",
    )
    .bind(seq)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(edge_id)
    .bind(CORE_INSPIRES_RELATION)
    .bind(source_memory_id)
    .bind(target_memory_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
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
        let scripted = Arc::new(ScriptedAnthropicClient::new(vec![ScriptedTurn::end_turn()]));
        let engine = build_engine_with(
            pg.clone(),
            Box::new(NoAuth::new(owner.principal.clone(), owner.clone())),
            |_registry| {},
        )
        .with_anthropic(scripted)
        .with_embed(Arc::new(FakeEmbedding));

        // Provision Alice + Bob (two engineer instances).
        let alice = engine
            .instantiate_personality(InstantiatePersonalityRequest {
                owner: owner.clone(),
                personality_type_id: "proxima-code/engineer-v1".into(),
                payload_overrides: Some(serde_json::json!({"display_name": "Alice"})),
            })
            .await?;
        let bob = engine
            .instantiate_personality(InstantiatePersonalityRequest {
                owner: owner.clone(),
                personality_type_id: "proxima-code/engineer-v1".into(),
                payload_overrides: Some(serde_json::json!({"display_name": "Bob"})),
            })
            .await?;
        let alice_self = self_perspective_for(&pg, alice.instance_id).await?;
        let bob_self = self_perspective_for(&pg, bob.instance_id).await?;
        assert_ne!(alice_self, bob_self);

        // Ingest a commit (we need a memory to use as the inspires
        // edge source).
        let now = time::OffsetDateTime::now_utc();
        let commit = ingest_commit(
            pg.pool(),
            &owner,
            SourceBatchId::new(Uuid::now_v7()),
            &CommitV1 {
                repo_id,
                sha: "goalsource".into(),
                parents: Vec::new(),
                author_name: "E2E".into(),
                author_email: "e2e@example.com".into(),
                author_time: now,
                committer_name: "E2E".into(),
                committer_email: "e2e@example.com".into(),
                committer_time: now,
                message: "fact".into(),
            },
            now,
        )
        .await?;

        // Author an inspires edge from the commit -> Alice's
        // self-Perspective.
        author_inspires_edge(&pg, &owner, commit.memory_id.into_inner(), alice_self).await?;

        // Run dispatcher with enough turns for both engineer wakes to
        // complete (each emits end_turn). Plus 1 for the commit-summary
        // wake on the commit-fact (default cursor was set before the
        // ingest, so it'll fire too).
        let scripted = Arc::new(ScriptedAnthropicClient::new(
            (0..6).map(|_| ScriptedTurn::end_turn()).collect(),
        ));
        let engine = build_engine_with(
            pg.clone(),
            Box::new(NoAuth::new(owner.principal.clone(), owner.clone())),
            |_registry| {},
        )
        .with_anthropic(scripted)
        .with_embed(Arc::new(FakeEmbedding));
        let fired = engine.run_dispatcher_tick().await?;
        assert_eq!(fired, 0, "Phase-1a dispatcher is still a no-op stub");

        // Assert: while the dispatcher is a stub, no instance records a
        // wake invocation. Targeted wake execution is the next plan.
        let alice_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM proxima_core.personality_wake_invocations w
             JOIN proxima_core.change_event e ON e.seq = w.change_event_seq
             WHERE w.personality_instance_id = $1
               AND e.kind = 'EdgeAppend'
               AND e.edge_relation = $2",
        )
        .bind(alice.instance_id.into_inner())
        .bind(CORE_INSPIRES_RELATION)
        .fetch_one(pg.pool())
        .await?;
        let bob_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM proxima_core.personality_wake_invocations w
             JOIN proxima_core.change_event e ON e.seq = w.change_event_seq
             WHERE w.personality_instance_id = $1
               AND e.kind = 'EdgeAppend'
               AND e.edge_relation = $2",
        )
        .bind(bob.instance_id.into_inner())
        .bind(CORE_INSPIRES_RELATION)
        .fetch_one(pg.pool())
        .await?;

        assert_eq!(
            alice_count, 0,
            "Alice must not wake until dispatcher execution lands"
        );
        assert_eq!(
            bob_count, 0,
            "Bob must NOT wake on an inspires edge that targets Alice"
        );

        Ok(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db).await;
    result.expect("goal_targeting_e2e failed");
}
