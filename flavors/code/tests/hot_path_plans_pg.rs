//! EXPLAIN plan-shape guards for code-flavor hot reads.
#![allow(clippy::doc_markdown, clippy::too_many_lines)]

mod common;

use common::{migrated_db, project_code, seed_memory, test_owner};
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
        project_code(
            pool,
            chunk_t,
            <CodeChunkV1 as AbstractionPayload>::SCHEMA_ID,
            None,
        )
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
        // A template-sized table has no plan worth pinning: every access
        // path costs about the same on two rows. Seed the situation the
        // composite index exists for — a NEIGHBOUR owner whose `CORPUS_ROWS`
        // chunks all match the same query word, in the same repository. Now
        // `search_tsv @@ 'needle'` alone reaches every one of them and only
        // `owner_id` separates this caller's single hit from the neighbour's
        // corpus, so a plan that does not index on `owner_id` is a plan that
        // reads another owner's postings.
        let neighbour = test_owner();
        seed_chunk_corpus(pool, &neighbour, repo_id).await?;
        // SQL-POLICY: fixed-fragment
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            "ANALYZE proxima_code.code_chunk_v1;
             ANALYZE proxima_code.projection;
             ANALYZE proxima_code.file_revision_v1;
             ANALYZE proxima_core.memory;
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
            .bind(vec![owner.stored_owner_id()])
            .fetch_one(&mut *tx)
            .await?;
        let chunk_plan = plan.to_string();
        // Naming the index is not enough, and this is the trap the pin this
        // replaced fell into: a multicolumn GIN is searchable on any SUBSET
        // of its columns, so `code_projection_owner_tsv_gin` appears in the
        // plan even with no owner predicate at all — as a tsvector-only
        // scan that walks every owner's postings and filters afterwards.
        // What has to hold is that `owner_id` is an INDEX condition. That
        // is true only while the owner is a predicate on `p` itself;
        // reaching it through a join to a sidecar, or dropping it, puts it
        // back in the heap filter.
        let index_cond = gin_index_cond(&plan).unwrap_or_else(|| {
            panic!("chunk scan must reach the projection's composite GIN; plan:\n{chunk_plan}")
        });
        assert!(
            index_cond.contains("owner_id"),
            "the composite GIN must index on owner_id, not filter on it afterwards; \
             index cond was `{index_cond}`; plan:\n{chunk_plan}"
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

/// How many decoy chunks the plan pin needs. Small enough to seed in two
/// statements, large enough that walking them all is visibly the worse plan.
const CORPUS_ROWS: i32 = 4_000;

/// The `Index Cond` of the first `code_projection_owner_tsv_gin` scan in an
/// `EXPLAIN (FORMAT JSON)` tree, or `None` when the plan never reaches that
/// index.
fn gin_index_cond(plan: &serde_json::Value) -> Option<String> {
    match plan {
        serde_json::Value::Array(items) => items.iter().find_map(gin_index_cond),
        serde_json::Value::Object(node) => {
            if node.get("Index Name").and_then(serde_json::Value::as_str)
                == Some("code_projection_owner_tsv_gin")
            {
                return Some(
                    node.get("Index Cond")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                );
            }
            node.values().find_map(gin_index_cond)
        }
        _ => None,
    }
}

/// Seed `CORPUS_ROWS` head chunks for `owner` in `repo_id`, every one of
/// them matching `needle`, each with its own admission and projection row.
///
/// Two statements, not one, and the split is forced: `memory_align_head` is
/// a BEFORE INSERT trigger that reads `memory_head` back, and every CTE of
/// one statement reads the same snapshot, so heads written in a sibling CTE
/// are invisible to it. The sidecar and projection rows are safe to write
/// alongside their admissions in the second statement, because referential
/// integrity is an AFTER trigger and fires once the whole statement's rows
/// are in place.
async fn seed_chunk_corpus(
    pool: &sqlx::PgPool,
    owner: &proxima_core::Owner,
    repo_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO proxima_core.owners (owner_id, kind)
         VALUES ($1, $2::proxima_core.owner_kind) ON CONFLICT DO NOTHING",
    )
    .bind(owner.stored_owner_id())
    .bind(proxima_core::OwnerRefKind::of(owner).as_str())
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t)
         SELECT uuidv7(), 'fact', 'proxima-code/code-chunk-v1', $1, uuidv7()
           FROM generate_series(1, $2)",
    )
    .bind(owner.stored_owner_id())
    .bind(CORPUS_ROWS)
    .execute(pool)
    .await?;
    // The decoys are exactly the heads that have no admission yet, which is
    // the statement above and nothing else: `seed_memory` writes head and
    // admission together.
    sqlx::query(
        "WITH ids AS MATERIALIZED (
             SELECT h.handle, h.t, row_number() OVER (ORDER BY h.t) AS n
               FROM proxima_core.memory_head h
              WHERE h.owner_id = $1
                AND NOT EXISTS (SELECT 1 FROM proxima_core.memory m WHERE m.t = h.t)
         ), admissions AS (
             INSERT INTO proxima_core.memory (handle, t, kind, owner_id, schema_id)
             SELECT handle, t, 'fact', $1, 'proxima-code/code-chunk-v1' FROM ids
             RETURNING t
         ), chunks AS (
             INSERT INTO proxima_code.code_chunk_v1
                 (t, repo_id, file_path, chunk_index, text, language, chunk_type,
                  byte_range_start, byte_range_end, line_range_start, line_range_end, state)
             SELECT t, $2, 'src/decoy_' || n || '.rs', 0,
                    'fn needle_' || n || '() { let needle = ' || n || '; }',
                    'rust', 'fn', 0, 40, 1, 1, 'Present'
               FROM ids
             RETURNING t
         )
         INSERT INTO proxima_code.projection (memory_id, schema_id, owner_id, search_tsv)
         SELECT t, 'proxima-code/code-chunk-v1', $1,
                to_tsvector(proxima_code.code_lexical_config(),
                            'fn needle_' || n || ' let needle straw ' || n)
           FROM ids",
    )
    .bind(owner.stored_owner_id())
    .bind(repo_id)
    .execute(pool)
    .await
    .map(|_| ())
}
