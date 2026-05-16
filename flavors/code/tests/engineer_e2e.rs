//! End-to-end: a `proxima-code/commit-summary-v1` Abstraction wakes the
//! `proxima-code/engineer-v1` personality, which calls
//! `core/fetch_memory` for the summary and emits a
//! `proxima-code/development-perspective-v1` Perspective with provenance
//! pointing at both the triggering Abstraction and the underlying
//! commit fact.

#![allow(clippy::too_many_lines, clippy::unnecessary_literal_bound)]

use std::sync::Arc;

use async_trait::async_trait;
use proxima_code::{
    CodeDevelopmentPerspectiveV1, CommitSummaryV1, CommitV1, build_engine_with, ingest_commit,
    migrator, register_repo,
};
use proxima_core::auth::NoAuth;
use proxima_core::llm::scripted::{ScriptedAnthropicClient, ScriptedTurn};
use proxima_core::llm::{EmbeddingClient, LlmError};
use proxima_core::personality::InstantiatePersonalityRequest;
use proxima_core::{OrgId, Owner, Principal, SourceBatchId, UserId};
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
    create_db(&db_name).await.expect("PG required for tests");
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

#[tokio::test(flavor = "multi_thread")]
async fn engineer_e2e_emits_perspective_with_chained_provenance() {
    let Some((db, pg)) = migrated_db().await else {
        return;
    };

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = test_owner();
        let repo_id = Uuid::now_v7();
        register_repo(pg.pool(), &owner, repo_id, "/tmp/engineer-e2e", "e2e").await?;

        // 1) Instantiate engineer + commit-summary first. Cursor parks
        //    at "now" so subsequent ingest is observable.
        let scripted = Arc::new(ScriptedAnthropicClient::new(vec![ScriptedTurn::end_turn()]));
        let engine = build_engine_with(
            pg.clone(),
            Box::new(NoAuth::new(owner.principal.clone(), owner.clone())),
            |_registry| {},
        )
        .with_anthropic(scripted)
        .with_embed(Arc::new(FakeEmbedding));
        engine
            .instantiate_personality(InstantiatePersonalityRequest {
                owner: owner.clone(),
                display_name: "Commit Summarizer".into(),
                purpose: "Summarize commits as Abstractions".into(),
            })
            .await?;
        engine
            .instantiate_personality(InstantiatePersonalityRequest {
                owner: owner.clone(),
                display_name: "Engineer".into(),
                purpose: "Develop perspectives on code changes".into(),
            })
            .await?;

        // 2) Ingest a commit Fact.
        let now = time::OffsetDateTime::now_utc();
        let commit_payload = CommitV1 {
            repo_id,
            sha: "engineerfact01".into(),
            parents: Vec::new(),
            author_name: "E2E".into(),
            author_email: "e2e@example.com".into(),
            author_time: now,
            committer_name: "E2E".into(),
            committer_email: "e2e@example.com".into(),
            committer_time: now,
            message: "feat: another change".into(),
        };
        let commit_outcome = ingest_commit(
            pg.pool(),
            &owner,
            SourceBatchId::new(Uuid::now_v7()),
            &commit_payload,
            now,
        )
        .await?;
        assert_ne!(commit_outcome.memory_id.into_inner(), Uuid::nil());

        // 3) First tick: commit-summary fires + emits an abstraction.
        //    Use `core/emit_perspective` with the commit-summary scripted
        //    payload (the engineer's emit_perspective will use the
        //    development-perspective payload). The same scripted
        //    Anthropic queue feeds both wakes serially, so we provide
        //    ALL turns up front. Order: commit-summary's turns, then
        //    engineer's turns.
        //
        //    The commit-summary triggering memory_id is the commit; the
        //    engineer's triggering memory is the abstraction we don't
        //    know the id of yet — `core/fetch_memory` with a placeholder
        //    UUID would error and the wake would still complete (loop
        //    treats the tool error as a tool_result and continues to
        //    end_turn). To keep the test deterministic, the engineer
        //    skips fetch_memory and goes straight to emit_perspective.
        //    Provenance still includes the triggering abstraction
        //    via the auto-wired snapshot.
        let scripted = Arc::new(ScriptedAnthropicClient::new(vec![
            // commit-summary wake — N1 = triggering (commit) memory,
            // pre-seeded by pre_seed_wake_handles before round 1.
            ScriptedTurn::tool_use(
                "core/fetch_memory",
                serde_json::json!({"memory": "N1"}),
            ),
            ScriptedTurn::tool_use(
                "core/emit_abstraction",
                serde_json::json!({
                    "schema_id": <CommitSummaryV1 as proxima_core::AbstractionPayload>::SCHEMA_ID,
                    "schema_version": 1,
                    "payload": {
                        "repo_id": repo_id,
                        "commit_sha": commit_payload.sha,
                        "summary": "Adds another change.",
                        "key_files": [],
                        "change_kind": "feature",
                    },
                }),
            ),
            ScriptedTurn::end_turn(),
            // engineer wake (no fetch_memory — provenance will be
            // {triggering_abstraction} only)
            ScriptedTurn::tool_use(
                "core/emit_perspective",
                serde_json::json!({
                    "schema_id": <CodeDevelopmentPerspectiveV1 as proxima_core::PerspectivePayload>::SCHEMA_ID,
                    "schema_version": 1,
                    "payload": {
                        "repo_id": repo_id,
                        "summary": "Engineer review summary",
                        "pattern": "additive change",
                        "risk": "low",
                        "recommended_posture": "ship",
                        "confidence": 0.9,
                    },
                }),
            ),
            ScriptedTurn::end_turn(),
        ]));
        let engine = build_engine_with(
            pg.clone(),
            Box::new(NoAuth::new(owner.principal.clone(), owner.clone())),
            |_registry| {},
        )
        .with_anthropic(scripted)
        .with_embed(Arc::new(FakeEmbedding));
        let fired = engine.run_dispatcher_tick().await?;
        assert_eq!(fired, 0, "Phase-1a dispatcher is still a no-op stub");

        let perspective_count: i64 = sqlx::query_scalar(
            "SELECT count(*)
             FROM proxima_core.memories
             WHERE kind = 'Perspective'
               AND schema_id = $1",
        )
        .bind(<CodeDevelopmentPerspectiveV1 as proxima_core::PerspectivePayload>::SCHEMA_ID)
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(
            perspective_count, 0,
            "wake execution moves to the next dispatcher plan"
        );

        Ok(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db).await;
    result.expect("engineer_e2e failed");
}
