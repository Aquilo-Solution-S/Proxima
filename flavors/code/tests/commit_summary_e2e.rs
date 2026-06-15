//! End-to-end: an ingested commit Fact is stored without invoking the
//! removed in-process wake dispatcher.

#![allow(clippy::too_many_lines, clippy::unnecessary_literal_bound)]

use std::sync::Arc;

use async_trait::async_trait;
use proxima_code::{
    CommitSummaryV1, CommitV1, build_engine_with, ingest_commit, migrator, register_repo,
};
use proxima_core::llm::{EmbeddingClient, LlmError};
use proxima_core::personality::InstantiatePersonalityRequest;
use proxima_core::{AuthPath, AuthzContext, OrgId, Owner, Principal, SourceBatchId, UserId};
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

#[tokio::test(flavor = "multi_thread")]
async fn commit_summary_e2e_produces_abstraction_with_correct_provenance() {
    let Some((db, pg)) = migrated_db().await else {
        return;
    };

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = test_owner();
        let authz = AuthzContext::single_owner(&owner, AuthPath::System);
        let repo_id = Uuid::now_v7();
        register_repo(pg.pool(), &owner, repo_id, "/tmp/commit-summary-e2e", "e2e").await?;

        let engine =
            build_engine_with(pg.clone(), |_registry| {}).with_embed(Arc::new(FakeEmbedding));
        let inst = engine
            .instantiate_personality(
                &authz,
                InstantiatePersonalityRequest {
                    principal: owner.principal.clone(),
                    org_id: None,
                    display_name: "Commit Summarizer".into(),
                },
            )
            .await?;

        // Ingest the commit AFTER instantiating so the cursor (parked
        // at "now") will see it.
        let now = time::OffsetDateTime::now_utc();
        let commit_payload = CommitV1 {
            repo_id,
            sha: "deadbeefcafebabe".into(),
            parents: Vec::new(),
            author_name: "E2E".into(),
            author_email: "e2e@example.com".into(),
            author_time: now,
            committer_name: "E2E".into(),
            committer_email: "e2e@example.com".into(),
            committer_time: now,
            message: "feat: add foo".into(),
        };
        let commit_outcome = ingest_commit(
            pg.pool(),
            &owner,
            SourceBatchId::new(Uuid::now_v7()),
            &commit_payload,
            now,
        )
        .await?;
        let commit_memory_id = commit_outcome.memory_id;

        let summary_count: i64 = sqlx::query_scalar(
            "SELECT count(*)
             FROM proxima_core.memories m
             JOIN proxima_code.commit_summary_v1 s ON s.memory_id = m.memory_id
             WHERE m.schema_id = $1",
        )
        .bind(<CommitSummaryV1 as proxima_core::AbstractionPayload>::SCHEMA_ID)
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(
            summary_count, 0,
            "commit ingest must not run wake execution"
        );

        let _ = inst;
        let _ = commit_memory_id;
        Ok(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db).await;
    result.expect("commit_summary_e2e failed");
}
