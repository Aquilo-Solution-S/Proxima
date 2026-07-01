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

async fn count_rows(pool: &sqlx::PgPool, sql: &'static str, id: Uuid) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(sql).bind(id).fetch_one(pool).await
}

#[tokio::test]
async fn erase_repo_is_deferred_to_pr9_compliance_ports() {
    let (db_name, pg) = migrated_db().await;

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = test_owner();
        let repo_id = Uuid::now_v7();
        register_repo(
            pg.pool_for_tests(),
            &owner,
            repo_id,
            &format!("/tmp/proxima-erase-repo-{repo_id}"),
            "erase repo fixture",
        )
        .await?;

        let memory_id = Uuid::now_v7();
        insert_repo_commit_with_test_request(pg.pool_for_tests(), &owner, repo_id, memory_id)
            .await?;

        let err = erase_repo(pg.pool_for_tests(), &owner, repo_id, &schemas_for_test())
            .await
            .expect_err("PR8 must defer repo erase to PR9 compliance erase ports");
        assert!(
            err.to_string()
                .contains("repo erase is deferred to PR9 compliance erase ports"),
            "unexpected erase_repo error: {err}"
        );

        assert_eq!(
            count_rows(
                pg.pool_for_tests(),
                "SELECT COUNT(*)::bigint FROM proxima_code.test_requested_v1 WHERE memory_id = $1",
                memory_id,
            )
            .await?,
            1
        );
        assert_eq!(
            count_rows(
                pg.pool_for_tests(),
                "SELECT COUNT(*)::bigint FROM proxima_core.memories WHERE memory_id = $1",
                memory_id,
            )
            .await?,
            1
        );
        assert_eq!(
            count_rows(
                pg.pool_for_tests(),
                "SELECT COUNT(*)::bigint FROM proxima_code.repos WHERE repo_id = $1",
                repo_id,
            )
            .await?,
            1
        );

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("erase_repo_is_deferred_to_pr9_compliance_ports failed");
}
