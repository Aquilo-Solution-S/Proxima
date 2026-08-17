//! EXPLAIN plan-shape guards for code-flavor hot reads.
#![allow(clippy::doc_markdown, clippy::too_many_lines)]

mod common;

use common::{migrated_db, seed_memory, test_owner};
use proxima_code::mcp::search_chunks::chunk_gin_sql_for_tests;
use proxima_code::{CodeChunkV1, FileRevisionV1};
use proxima_core::{AbstractionPayload, FactPayload};
use proxima_pg_testkit::drop_db;
use proxima_storage_pg::query::file_revision_heads_sql_for_tests;
use uuid::Uuid;

#[tokio::test]
async fn code_hot_path_plans_use_expected_indexes() {
    let (db_name, pg) = migrated_db().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = test_owner();
        let pool = pg.pool_for_tests();
        let repo_id = Uuid::now_v7();
        let (_, chunk_t) = seed_memory(
            pool,
            &owner,
            <CodeChunkV1 as AbstractionPayload>::SCHEMA_ID,
            "abstraction",
            None,
            None,
            &[],
        )
        .await?;
        sqlx::query(
            "INSERT INTO proxima_code.code_chunk_v1
                (t, repo_id, file_path, chunk_index, text, language, chunk_type,
                 byte_range_start, byte_range_end, line_range_start, line_range_end, state)
             VALUES ($1, $2, 'src/lib.rs', 0, 'fn needle() {}', 'rust', 'fn',
                     0, 14, 1, 1, 'Present')",
        )
        .bind(chunk_t)
        .bind(repo_id)
        .execute(pool)
        .await?;
        let (_, file_t) = seed_memory(
            pool,
            &owner,
            FileRevisionV1::SCHEMA_ID,
            "fact",
            None,
            None,
            &[],
        )
        .await?;
        sqlx::query(
            "INSERT INTO proxima_code.file_revision_v1
                (t, repo_id, file_path, language, content_sha256,
                 size_bytes, indexed_commit_sha, state)
             VALUES ($1, $2, 'src/lib.rs', 'rust', $3, 14, 'deadbeef', 'Present')",
        )
        .bind(file_t)
        .bind(repo_id)
        .bind([7u8; 32].to_vec())
        .execute(pool)
        .await?;
        // SQL-POLICY: fixed-fragment
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            "ANALYZE proxima_code.code_chunk_v1;
             ANALYZE proxima_code.file_revision_v1;
             ANALYZE proxima_core.memory_head",
        ))
        .execute(pool)
        .await?;

        let mut tx = pool.begin().await?;
        sqlx::query("SET LOCAL enable_seqscan = off")
            .execute(&mut *tx)
            .await?;
        sqlx::query("SET LOCAL enable_sort = off")
            .execute(&mut *tx)
            .await?;

        let chunk = chunk_gin_sql_for_tests();
        let chunk_explain = format!("EXPLAIN (FORMAT JSON, COSTS OFF) {chunk}");
        // SQL-POLICY: fixed-fragment
        let plan: serde_json::Value = sqlx::query_scalar(sqlx::AssertSqlSafe(chunk_explain))
            .bind("needle")
            .bind(Some(repo_id))
            .bind(None::<String>)
            .bind("%needle%")
            .bind(None::<String>)
            .bind(20_i64)
            .bind("")
            .fetch_one(&mut *tx)
            .await?;
        let chunk_plan = plan.to_string();
        assert!(
            chunk_plan.contains("idx_code_chunk_v1_search_tsv")
                || chunk_plan.contains("idx_code_chunk_v1_nk"),
            "chunk GIN must index code_chunk_v1; plan:\n{chunk_plan}"
        );

        let heads = file_revision_heads_sql_for_tests();
        let heads_explain = format!("EXPLAIN (FORMAT JSON, COSTS OFF) {heads}");
        // SQL-POLICY: fixed-fragment
        let plan: serde_json::Value = sqlx::query_scalar(sqlx::AssertSqlSafe(heads_explain))
            .bind(owner.stored_owner_id())
            .bind(repo_id)
            .bind(FileRevisionV1::SCHEMA_ID)
            .bind(vec!["src/lib.rs".to_string()])
            .fetch_one(&mut *tx)
            .await?;
        assert!(
            plan.to_string().contains("idx_file_revision_v1_nk")
                || plan.to_string().contains("memory_head_owner_schema_idx"),
            "file-revision heads must use nk or head index; plan:\n{plan}"
        );

        tx.rollback().await?;
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("code hot-path EXPLAIN failed");
}
