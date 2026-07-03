mod common;

use common::{migrated_db, test_owner};
use proxima_code::testkit::{erase_repo, register_repo};
use proxima_code::{CommitV1, TestRequestV1};
use proxima_core::verbs::schema::{PayloadKind, SchemaInfo};
use proxima_core::{FactPayload, Owner, SchemaId, SchemaVersion};
use proxima_pg_testkit::drop_db;
use uuid::Uuid;

fn owner_parts(owner: &Owner) -> (proxima_core::OwnerRefKind, Option<Uuid>) {
    proxima_storage_pg::access::owner_columns::owner_binds(owner)
}

fn fact_schema(schema_id: &str, sidecar_table: &str) -> SchemaInfo {
    let mut schema = SchemaInfo::opaque(
        SchemaId::new(schema_id.into()),
        SchemaVersion::new(1),
        PayloadKind::Fact,
    );
    schema.sidecar_table = Some(sidecar_table.into());
    schema
}

fn schemas_for_test() -> Vec<SchemaInfo> {
    vec![
        fact_schema(CommitV1::SCHEMA_ID, "proxima_code.commit_v1"),
        fact_schema(TestRequestV1::SCHEMA_ID, "proxima_code.test_requested_v1"),
    ]
}

async fn insert_repo_commit_with_test_request(
    pool: &sqlx::PgPool,
    owner: &Owner,
    repo_id: Uuid,
    memory_id: Uuid,
) -> Result<(), sqlx::Error> {
    let (owner_kind, owner_id) = owner_parts(owner);
    let source_batch_id = Uuid::now_v7();
    let receipt_id = Uuid::now_v7().as_bytes().to_vec();

    sqlx::query(
        "INSERT INTO proxima_core.source_batches
            (id, source_id, owner_kind, owner_id)
         VALUES ($1, 'test/erase-repo', $2, $3)",
    )
    .bind(source_batch_id)
    .bind(owner_kind)
    .bind(owner_id)
    .execute(pool)
    .await?;

    sqlx::query(
        "INSERT INTO proxima_core.fact_receipts
            (receipt_id, source, source_batch_id, owner_kind,
             owner_id, schema_id, schema_version,
             observed_at, occurred_at)
         VALUES ($1, 'test/erase-repo', $2, $3, $4, $5, 1, now(), now())",
    )
    .bind(&receipt_id)
    .bind(source_batch_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(CommitV1::SCHEMA_ID)
    .execute(pool)
    .await?;

    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_kind, owner_id, schema_id, schema_version, receipt_id)
         VALUES ($1, $2, $3, $4, 1, $5)",
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(CommitV1::SCHEMA_ID)
    .bind(&receipt_id)
    .execute(pool)
    .await?;

    sqlx::query(
        "INSERT INTO proxima_code.commit_v1
            (memory_id, repo_id, sha, parents, author_name, author_email,
             author_time, committer_name, committer_email, committer_time, message)
         VALUES ($1, $2, 'abc1234', ARRAY[]::text[], 'A', 'a@example.com',
             now(), 'A', 'a@example.com', now(), 'fixture')",
    )
    .bind(memory_id)
    .bind(repo_id)
    .execute(pool)
    .await?;

    sqlx::query(
        "INSERT INTO proxima_code.test_requested_v1
            (memory_id, repo_id, title, instructions, test_key, criteria_count)
         VALUES ($1, $2, 'bug 2', 'delete sidecar', 'bug-2', 1)",
    )
    .bind(memory_id)
    .bind(repo_id)
    .execute(pool)
    .await?;

    sqlx::query(
        "INSERT INTO proxima_code.test_requested_criterion_v1
            (test_requested_memory_id, criterion_index, criterion_key,
             description, required, verifier_kind)
         VALUES ($1, 0, 'c', 'criterion', true, 'reviewer_only')",
    )
    .bind(memory_id)
    .execute(pool)
    .await?;

    Ok(())
}

#[tokio::test]
async fn erase_repo_deletes_repo_rows_and_preserves_other_repos() {
    let (db_name, pg) = migrated_db().await;
    let result = exercise_repo_erase(pg.pool_for_tests()).await;
    let _ = drop_db(&db_name).await;
    result.expect("erase_repo_deletes_repo_rows_and_preserves_other_repos failed");
}

async fn exercise_repo_erase(pool: &sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let owner = test_owner();
    let repo_id = Uuid::now_v7();
    let other_repo_id = Uuid::now_v7();
    let repo_path = format!("/tmp/proxima-erase-repo-{repo_id}");
    register_repo(pool, &owner, repo_id, &repo_path, "erase repo fixture").await?;
    register_repo(
        pool,
        &owner,
        other_repo_id,
        &format!("/tmp/proxima-erase-repo-keep-{other_repo_id}"),
        "keep repo fixture",
    )
    .await?;

    let memory_id = Uuid::now_v7();
    insert_repo_commit_with_test_request(pool, &owner, repo_id, memory_id).await?;
    let other_memory_id = Uuid::now_v7();
    insert_repo_commit_with_test_request(pool, &owner, other_repo_id, other_memory_id).await?;

    let receipt = erase_repo(pool, &owner, repo_id, &schemas_for_test()).await?;
    assert_eq!(receipt.repo_id, repo_id);
    assert_eq!(receipt.facts_deleted, 1);
    assert_eq!(receipt.abstractions_deleted, 0);
    assert_eq!(receipt.receipts_deleted, 1);
    assert_eq!(receipt.source_batches_deleted, 1);
    assert!(receipt.repo_record_deleted);

    assert_repo_erased(pool, repo_id, memory_id).await?;
    assert_other_repo_preserved(pool, other_repo_id, other_memory_id).await?;
    assert_repo_rebuild_allowed(pool, &owner, repo_id, &repo_path).await
}

async fn assert_repo_erased(
    pool: &sqlx::PgPool,
    repo_id: Uuid,
    memory_id: Uuid,
) -> Result<(), Box<dyn std::error::Error>> {
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*)::bigint FROM proxima_code.test_requested_v1 WHERE memory_id = $1",
        )
        .bind(memory_id)
        .fetch_one(pool)
        .await?,
        0_i64
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*)::bigint FROM proxima_code.test_requested_criterion_v1 WHERE test_requested_memory_id = $1",
        )
        .bind(memory_id)
        .fetch_one(pool)
        .await?,
        0_i64
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*)::bigint FROM proxima_core.memories WHERE memory_id = $1",
        )
        .bind(memory_id)
        .fetch_one(pool)
        .await?,
        0_i64
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*)::bigint FROM proxima_code.repos WHERE repo_id = $1",
        )
        .bind(repo_id)
        .fetch_one(pool)
        .await?,
        0_i64
    );
    Ok(())
}

async fn assert_other_repo_preserved(
    pool: &sqlx::PgPool,
    repo_id: Uuid,
    memory_id: Uuid,
) -> Result<(), Box<dyn std::error::Error>> {
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*)::bigint FROM proxima_core.memories WHERE memory_id = $1",
        )
        .bind(memory_id)
        .fetch_one(pool)
        .await?,
        1_i64
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*)::bigint FROM proxima_code.repos WHERE repo_id = $1",
        )
        .bind(repo_id)
        .fetch_one(pool)
        .await?,
        1_i64
    );
    Ok(())
}

async fn assert_repo_rebuild_allowed(
    pool: &sqlx::PgPool,
    owner: &Owner,
    repo_id: Uuid,
    repo_path: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    register_repo(pool, owner, repo_id, repo_path, "rebuilt repo fixture").await?;
    let rebuilt_memory_id = Uuid::now_v7();
    insert_repo_commit_with_test_request(pool, owner, repo_id, rebuilt_memory_id).await?;
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*)::bigint FROM proxima_core.memories WHERE memory_id = $1",
        )
        .bind(rebuilt_memory_id)
        .fetch_one(pool)
        .await?,
        1_i64
    );
    Ok(())
}
