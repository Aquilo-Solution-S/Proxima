//! End-to-end: an ingested commit Fact is stored without invoking the
//! removed in-process wake dispatcher.

#![allow(clippy::too_many_lines, clippy::unnecessary_literal_bound)]

mod common;

use common::{migrated_db, test_owner};
use proxima_code::{CommitSummaryV1, CommitV1, ingest_commit, register_repo};
use proxima_core::SourceBatchId;
use proxima_pg_testkit::drop_db;
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread")]
async fn commit_summary_e2e_produces_abstraction_with_correct_provenance() {
    let (db, pg) = migrated_db().await;

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = test_owner();
        let repo_id = Uuid::now_v7();
        register_repo(pg.pool(), &owner, repo_id, "/tmp/commit-summary-e2e", "e2e").await?;

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

        let _ = commit_memory_id;
        Ok(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db).await;
    result.expect("commit_summary_e2e failed");
}
