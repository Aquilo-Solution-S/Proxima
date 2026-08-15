//! Same file path is one series: later ingest reuses `handle`.

mod common;

use common::{migrated_db, owner_write_permit, test_owner};
use proxima_code::testkit::{ingest_commit, ingest_file_revision};
use proxima_code::{CommitV1, FileRevisionV1, FileState};
use proxima_core::{AccessKind, SourceBatchId};
use proxima_pg_testkit::drop_db;
use uuid::Uuid;

fn source_batch_id() -> SourceBatchId {
    SourceBatchId::new(Uuid::now_v7())
}

fn content_hash(seed: &str) -> [u8; 32] {
    *blake3::hash(seed.as_bytes()).as_bytes()
}

fn file_revision(repo_id: Uuid, file_path: &str, version: &str) -> FileRevisionV1 {
    FileRevisionV1 {
        repo_id,
        file_path: file_path.to_string(),
        language: Some("rust".to_string()),
        content_sha256: content_hash(version),
        size_bytes: u64::try_from(version.len()).unwrap_or(u64::MAX),
        indexed_commit_sha: format!("{version:0<40}"),
        state: FileState::Present,
    }
}

fn commit(repo_id: Uuid) -> CommitV1 {
    let now = time::OffsetDateTime::now_utc();
    CommitV1 {
        repo_id,
        sha: "0123456789abcdef0123456789abcdef01234567".to_string(),
        parents: Vec::new(),
        author_name: "Ada".to_string(),
        author_email: "ada@example.test".to_string(),
        author_time: now,
        committer_name: "Ada".to_string(),
        committer_email: "ada@example.test".to_string(),
        committer_time: now,
        message: "initial".to_string(),
    }
}

#[tokio::test]
async fn code_stateful_ingest_derives_fact_entity_heads() {
    let (db_name, pg) = migrated_db().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = test_owner();
        let permit = owner_write_permit(&owner, AccessKind::Fact).await?;
        let repo_id = Uuid::now_v7();
        let file_path = "src/lib.rs";
        let now = time::OffsetDateTime::now_utc();

        let first = ingest_file_revision(
            pg.pool_for_tests(),
            &permit,
            source_batch_id(),
            &file_revision(repo_id, file_path, "v1"),
            now,
        )
        .await?;
        let second = ingest_file_revision(
            pg.pool_for_tests(),
            &permit,
            source_batch_id(),
            &file_revision(repo_id, file_path, "v2"),
            now,
        )
        .await?;
        assert_eq!(
            first.handle, second.handle,
            "same path is one series"
        );
        assert_ne!(
            first.memory_id, second.memory_id,
            "new observation is a new t"
        );

        ingest_commit(
            pg.pool_for_tests(),
            &permit,
            source_batch_id(),
            &commit(repo_id),
            now,
        )
        .await?;
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("code_stateful_ingest_derives_fact_entity_heads failed");
}
