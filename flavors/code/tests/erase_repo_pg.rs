mod common;

use common::{migrated_db, test_owner};
use proxima_code::CommitV1;
use proxima_code::RepoScope;
use proxima_code::testkit::{erase_repo, register_repo};
use proxima_core::{FactPayload, Owner};
use proxima_pg_testkit::drop_db;
use uuid::Uuid;

async fn insert_repo_commit_with_test_request(
    pool: &sqlx::PgPool,
    owner: &Owner,
    repo_id: Uuid,
    memory_id: Uuid,
) -> Result<(), sqlx::Error> {
    let owner_id = owner.stored_owner_id();
    let handle = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO proxima_core.owners (owner_id, kind)
         VALUES ($1, 'personal') ON CONFLICT DO NOTHING",
    )
    .bind(owner_id)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t)
         VALUES ($1, 'fact', $2, $3, $4)",
    )
    .bind(handle)
    .bind(CommitV1::SCHEMA_ID)
    .bind(owner_id)
    .bind(memory_id)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.memory (handle, t, kind, owner_id)
         VALUES ($1, $2, 'fact', $3)",
    )
    .bind(handle)
    .bind(memory_id)
    .bind(owner_id)
    .execute(pool)
    .await?;

    sqlx::query(
        "INSERT INTO proxima_code.commit_v1
            (t, repo_id, sha, parents, author_name, author_email,
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
            (t, repo_id, title, instructions, test_key, criteria_count)
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
    register_repo(
        pool,
        &owner,
        repo_id,
        &repo_path,
        "erase repo fixture",
        &RepoScope::default(),
    )
    .await?;
    register_repo(
        pool,
        &owner,
        other_repo_id,
        &format!("/tmp/proxima-erase-repo-keep-{other_repo_id}"),
        "keep repo fixture",
        &RepoScope::default(),
    )
    .await?;

    let memory_id = Uuid::now_v7();
    insert_repo_commit_with_test_request(pool, &owner, repo_id, memory_id).await?;
    let other_memory_id = Uuid::now_v7();
    insert_repo_commit_with_test_request(pool, &owner, other_repo_id, other_memory_id).await?;

    let receipt = erase_repo(pool, &owner, repo_id).await?;
    assert_eq!(receipt.repo_id, repo_id);
    assert_eq!(receipt.facts_deleted, 1);
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
            "SELECT COUNT(*)::bigint FROM proxima_code.commit_v1 WHERE memory_id = $1",
        )
        .bind(memory_id)
        .fetch_one(pool)
        .await?,
        0_i64
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*)::bigint FROM proxima_core.memory WHERE t = $1",
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
            "SELECT COUNT(*)::bigint FROM proxima_core.memory WHERE t = $1",
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
    register_repo(
        pool,
        owner,
        repo_id,
        repo_path,
        "rebuilt repo fixture",
        &RepoScope::default(),
    )
    .await?;
    let rebuilt_memory_id = Uuid::now_v7();
    insert_repo_commit_with_test_request(pool, owner, repo_id, rebuilt_memory_id).await?;
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*)::bigint FROM proxima_core.memory WHERE t = $1",
        )
        .bind(rebuilt_memory_id)
        .fetch_one(pool)
        .await?,
        1_i64
    );
    Ok(())
}
