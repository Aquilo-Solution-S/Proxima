mod common;

use common::{migrated_db, test_owner};
use proxima_code::{
    CodeChunkV1, CommitV1, FileRevisionV1, FileState, append_code_slice, ingest_commit,
    ingest_file_revision,
};
use proxima_core::{AbstractionPayload, FactPayload, Owner, SourceBatchId};
use proxima_pg_testkit::drop_db;
use sqlx::PgPool;
use uuid::Uuid;

fn owner_cols(owner: &Owner) -> (proxima_core::OwnerRefKind, Option<Uuid>) {
    owner.columns()
}

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

fn code_chunk(repo_id: Uuid, file_path: &str, chunk_index: u32) -> CodeChunkV1 {
    CodeChunkV1 {
        repo_id,
        file_path: file_path.to_string(),
        chunk_index,
        text: "fn a() {}\n".to_string(),
        language: Some("rust".to_string()),
        chunk_type: "function".to_string(),
        byte_range_start: 0,
        byte_range_end: 10,
        line_range_start: 1,
        line_range_end: 1,
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

fn file_revision_key(repo_id: Uuid, file_path: &str) -> Vec<String> {
    vec![repo_id.to_string(), file_path.to_string()]
}

async fn fact_entity_rows(
    pool: &PgPool,
    owner: &Owner,
    schema_id: &str,
    natural_key: &[String],
) -> Result<Vec<(Uuid, Uuid)>, sqlx::Error> {
    let (kind, principal_id) = owner_cols(owner);
    sqlx::query_as(
        "SELECT fact_entity_id, current_memory_id
           FROM proxima_core.fact_entities
          WHERE owner_kind = $1
            AND owner_id = $2
            AND schema_id = $3
            AND schema_version = 1
            AND natural_key = $4
          ORDER BY fact_entity_id",
    )
    .bind(kind)
    .bind(principal_id)
    .bind(schema_id)
    .bind(natural_key)
    .fetch_all(pool)
    .await
}

async fn fact_entity_count_for_schema(
    pool: &PgPool,
    owner: &Owner,
    schema_id: &str,
) -> Result<i64, sqlx::Error> {
    let (kind, principal_id) = owner_cols(owner);
    sqlx::query_scalar(
        "SELECT count(*)::bigint
           FROM proxima_core.fact_entities
          WHERE owner_kind = $1
            AND owner_id = $2
            AND schema_id = $3
            AND schema_version = 1",
    )
    .bind(kind)
    .bind(principal_id)
    .bind(schema_id)
    .fetch_one(pool)
    .await
}

async fn memory_fact_entity_id(
    pool: &PgPool,
    memory_id: proxima_core::MemoryId,
) -> Result<Option<Uuid>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT fact_entity_id
           FROM proxima_core.memories
          WHERE memory_id = $1",
    )
    .bind(memory_id.into_inner())
    .fetch_one(pool)
    .await
}

async fn memory_kind(
    pool: &PgPool,
    memory_id: proxima_core::MemoryId,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT kind::text
           FROM proxima_core.memories
          WHERE memory_id = $1",
    )
    .bind(memory_id.into_inner())
    .fetch_optional(pool)
    .await
}

#[tokio::test]
async fn code_stateful_ingest_derives_fact_entity_heads() {
    let (db_name, pg) = migrated_db().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = test_owner();
        let repo_id = Uuid::now_v7();
        let observed_at = time::OffsetDateTime::now_utc();
        let file_path = "src/a.rs";

        let first_file = file_revision(repo_id, file_path, "v1");
        let first_outcome = ingest_file_revision(
            pg.pool(),
            &owner,
            source_batch_id(),
            &first_file,
            observed_at,
        )
        .await?;
        let file_key = file_revision_key(repo_id, file_path);
        let rows =
            fact_entity_rows(pg.pool(), &owner, FileRevisionV1::SCHEMA_ID, &file_key).await?;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].1, first_outcome.memory_id.into_inner());
        assert!(
            memory_fact_entity_id(pg.pool(), first_outcome.memory_id)
                .await?
                .is_some()
        );

        let second_file = file_revision(repo_id, file_path, "v2");
        let second_outcome = ingest_file_revision(
            pg.pool(),
            &owner,
            source_batch_id(),
            &second_file,
            observed_at,
        )
        .await?;
        let rows =
            fact_entity_rows(pg.pool(), &owner, FileRevisionV1::SCHEMA_ID, &file_key).await?;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].1, second_outcome.memory_id.into_inner());
        assert!(
            memory_fact_entity_id(pg.pool(), second_outcome.memory_id)
                .await?
                .is_some()
        );

        let chunk = code_chunk(repo_id, file_path, 0);
        let chunk_outcome =
            append_code_slice(pg.pool(), &owner, &chunk, second_outcome.memory_id, None).await?;
        assert_eq!(
            fact_entity_count_for_schema(
                pg.pool(),
                &owner,
                <CodeChunkV1 as AbstractionPayload>::SCHEMA_ID,
            )
            .await?,
            0
        );
        assert_eq!(
            memory_kind(pg.pool(), chunk_outcome.memory_id)
                .await?
                .as_deref(),
            Some("Abstraction")
        );

        let commit = commit(repo_id);
        ingest_commit(pg.pool(), &owner, source_batch_id(), &commit, observed_at).await?;
        let commit_entities =
            fact_entity_count_for_schema(pg.pool(), &owner, CommitV1::SCHEMA_ID).await?;
        assert_eq!(commit_entities, 0);

        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("code_stateful_ingest_derives_fact_entity_heads failed");
}
