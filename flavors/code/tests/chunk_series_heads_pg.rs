//! File-level owned chunk series listing.

mod common;

use common::{migrated_db, seed_memory_with_sidecars, test_owner};
use proxima_code::CodeChunkV1;
use proxima_core::AbstractionPayload;
use proxima_pg_testkit::drop_db;
use proxima_storage_pg::query::{owned_chunk_series_heads, owned_present_chunk_indexes};
use uuid::Uuid;

#[tokio::test]
async fn owned_chunk_series_heads_lists_present_and_tombstone() {
    let (db_name, pg) = migrated_db().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = test_owner();
        let pool = pg.pool_for_tests();
        let repo_id = Uuid::now_v7();
        let schema = <CodeChunkV1 as AbstractionPayload>::SCHEMA_ID;
        let mut handles = Vec::new();
        for (index, state) in [(0_i32, "Present"), (1, "Present"), (2, "Tombstone")] {
            let (handle, t) = seed_memory_with_sidecars(
                pool,
                &owner,
                schema,
                "abstraction",
                None,
                None,
                &[],
                &[<CodeChunkV1 as AbstractionPayload>::sidecar_table()],
            )
            .await?;
            sqlx::query(
                "INSERT INTO proxima_code.code_chunk_v1
                    (t, repo_id, file_path, chunk_index, text, language, chunk_type,
                     byte_range_start, byte_range_end, line_range_start, line_range_end, state)
                 VALUES ($1, $2, 'src/lib.rs', $3, 'fn x() {}', 'rust', 'fn',
                         0, 8, 1, 1, $4::proxima_code.file_state)",
            )
            .bind(t)
            .bind(repo_id)
            .bind(index)
            .bind(state)
            .execute(pool)
            .await?;
            handles.push(handle);
        }

        let heads = owned_chunk_series_heads(
            pool,
            owner,
            &<CodeChunkV1 as AbstractionPayload>::schema_id(),
            repo_id,
            "src/lib.rs",
        )
        .await?;
        assert_eq!(
            heads
                .iter()
                .map(|head| (head.chunk_index, head.handle, head.state.as_str()))
                .collect::<Vec<_>>(),
            vec![
                (0, handles[0], "Present"),
                (1, handles[1], "Present"),
                (2, handles[2], "Tombstone"),
            ]
        );

        let present = owned_present_chunk_indexes(
            pool,
            owner,
            &<CodeChunkV1 as AbstractionPayload>::schema_id(),
            repo_id,
            "src/lib.rs",
        )
        .await?;
        assert_eq!(present, vec![0, 1]);
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("owned_chunk_series_heads listing failed");
}
