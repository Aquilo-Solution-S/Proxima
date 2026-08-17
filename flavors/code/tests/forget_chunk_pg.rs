//! Engine forget/hydrate of a real `code_chunk_v1` sidecar row.

mod common;

use common::{migrated_db, owner_write_permit, seed_memory_with_sidecars, test_owner};
use proxima_code::CodeChunkV1;
use proxima_core::storage_ports::{MemoryAuthoringPort, OwnerWritePermit};
use proxima_core::{AbstractionPayload, AccessKind, MemoryId};
use proxima_pg_testkit::drop_db;
use proxima_storage_pg::verbs::forget::{MemoryColdStore, hydrate_memory};
use std::sync::Arc;
use uuid::Uuid;

#[tokio::test]
async fn forget_hydrate_restores_code_chunk_sidecar() {
    let (db_name, pg) = migrated_db().await;
    let cold = Arc::new(MemoryColdStore::default());
    let pg = pg.with_cold(cold.clone());
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = test_owner();
        let permit: OwnerWritePermit = owner_write_permit(&owner, AccessKind::Fact).await?;
        let pool = pg.pool_for_tests();
        let (_handle, memory_id) = seed_memory_with_sidecars(
            pool,
            &owner,
            <CodeChunkV1 as AbstractionPayload>::SCHEMA_ID,
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
             VALUES ($1, $2, 'src/lib.rs', 0, 'fn forget_me() {}', 'rust', 'fn',
                     0, 16, 1, 1, 'Present')",
        )
        .bind(memory_id)
        .bind(Uuid::now_v7())
        .execute(pool)
        .await?;

        MemoryAuthoringPort::forget_memory(&pg, &permit, MemoryId::new(memory_id)).await?;
        let hot: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_code.code_chunk_v1 WHERE t = $1",
        )
        .bind(memory_id)
        .fetch_one(pool)
        .await?;
        assert_eq!(hot, 0, "forget deletes the flavor sidecar before memory");

        let mut tx = pool.begin().await?;
        hydrate_memory(&mut tx, pg.sidecars(), cold.as_ref(), memory_id).await?;
        tx.commit().await?;

        let text: String =
            sqlx::query_scalar("SELECT text FROM proxima_code.code_chunk_v1 WHERE t = $1")
                .bind(memory_id)
                .fetch_one(pool)
                .await?;
        assert_eq!(text, "fn forget_me() {}");
        let tsv: bool = sqlx::query_scalar(
            "SELECT search_tsv IS NOT NULL FROM proxima_code.code_chunk_v1 WHERE t = $1",
        )
        .bind(memory_id)
        .fetch_one(pool)
        .await?;
        assert!(tsv, "generated search_tsv is recomputed on hydrate");
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("forget/hydrate code_chunk_v1 failed");
}
