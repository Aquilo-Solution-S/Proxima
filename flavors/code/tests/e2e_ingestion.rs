#![allow(clippy::doc_markdown, clippy::too_many_lines)]

use proxima_code::RepoScope;

mod common;

use common::{migrated_db, test_owner};
use proxima_code::testkit::{advance_stage, mark_failed, mark_succeeded, register_repo, start_run};
use proxima_code::{RunStage, RunStatus, StageCounters};
use proxima_core::Owner;
use proxima_pg_testkit::drop_db;
use uuid::Uuid;

async fn register_test_repo(pool: &sqlx::PgPool, owner: &Owner, repo_id: Uuid) {
    register_repo(
        pool,
        None,
        owner,
        repo_id,
        "/tmp/proxima-e2e",
        "proxima-e2e",
        &RepoScope::default(),
    )
    .await
    .expect("register repo");
}

#[tokio::test]
async fn run_transitions_and_failure_persist() {
    let (db_name, pg) = migrated_db().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = test_owner();
        let repo_id = Uuid::now_v7();
        register_test_repo(pg.pool_for_tests(), &owner, repo_id).await;

        let run = start_run(pg.pool_for_tests(), None, &owner, repo_id).await?;
        let r2 = advance_stage(
            pg.pool_for_tests(),
            None,
            run.run_id,
            RunStage::Facts,
            &StageCounters::zeroed(),
        )
        .await?;
        assert_eq!(r2.status, RunStatus::Running);
        assert_eq!(r2.stage, RunStage::Facts);
        let r3 = mark_succeeded(
            pg.pool_for_tests(),
            None,
            run.run_id,
            &StageCounters::zeroed(),
        )
        .await?;
        assert_eq!(r3.status, RunStatus::Succeeded);
        assert!(r3.finished_at.is_some());

        let repo_id2 = Uuid::now_v7();
        register_repo(
            pg.pool_for_tests(),
            None,
            &owner,
            repo_id2,
            "/tmp/proxima-e2e-2",
            "repo2",
            &RepoScope::default(),
        )
        .await?;
        let failed = start_run(pg.pool_for_tests(), None, &owner, repo_id2).await?;
        let failed = mark_failed(pg.pool_for_tests(), None, failed.run_id, "boom").await?;
        assert_eq!(failed.status, RunStatus::Failed);
        assert_eq!(failed.error_message.as_deref(), Some("boom"));
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("run_transitions_and_failure_persist failed");
}
