//! Slice 6: forget / hydrate / erase.
#![allow(clippy::doc_markdown, clippy::too_many_lines)]

use proxima_core::storage_ports::OwnerWritePermit;
use proxima_core::verbs::fact_ingest::FactWriteCommand;
use proxima_core::{AccessKind, OwnerRef, SchemaId, SchemaVersion, UserId};
use proxima_pg_testkit::{create_db, db_url, drop_db};
use proxima_storage_pg::PgStorage;
use proxima_storage_pg::verbs::fact_ingest::ingest_fact_atomic;
use proxima_storage_pg::verbs::forget::{
    MemoryColdStore, cold_object_key, erase_memory, forget_memory, hydrate_memory,
};
use uuid::Uuid;

fn draft(source: Option<(&str, &str)>) -> FactWriteCommand {
    FactWriteCommand {
        schema_id: SchemaId::new("core/test-fact-v1".to_string()),
        schema_version: SchemaVersion::new(1),
        handle: None,
        source_id: source.map(|(s, _)| s.to_owned()),
        ingest_key: source.map(|(_, k)| k.to_owned()),
        payload: Vec::new(),
        rendered_text: None,
        lexical_language: None,
        receipt: None,
        citation: None,
        derived_from: Vec::new(),
        refs: Vec::new(),
        blob_id: None,
        kind: "fact".into(),
    }
}

#[tokio::test]
async fn forget_hydrate_erase_and_world_never() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let pool = pg.pool_for_tests();
        let sourced = draft(Some(("src", "k1")));
        let written = ingest_fact_atomic(pool, &permit, &sourced, None).await?;
        let t = written.memory_id.into_inner();
        let key = cold_object_key("ownerhash", written.handle, t);
        assert!(key.starts_with("cold/"));
        assert!(!key.contains(&owner.stored_owner_id().to_string()));

        let cold = MemoryColdStore::default();
        let mut tx = pool.begin().await?;
        forget_memory(&mut tx, &cold, &key, t).await?;
        tx.commit().await?;

        let hot: i64 = sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.memory WHERE t = $1")
            .bind(t)
            .fetch_one(pool)
            .await?;
        assert_eq!(hot, 0);
        let stub: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.cooled WHERE t = $1")
                .bind(t)
                .fetch_one(pool)
                .await?;
        assert_eq!(stub, 1);
        let keys: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.ingest_keys WHERE ingest_key = 'k1'",
        )
        .fetch_one(pool)
        .await?;
        assert_eq!(keys, 1, "forget does not touch ingest_keys");

        let mut tx = pool.begin().await?;
        hydrate_memory(&mut tx, &cold, t).await?;
        tx.commit().await?;
        let hot: i64 = sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.memory WHERE t = $1")
            .bind(t)
            .fetch_one(pool)
            .await?;
        assert_eq!(hot, 1);
        let op: String = sqlx::query_scalar(
            "SELECT op::text FROM proxima_core.announce WHERE t = $1 ORDER BY seq DESC LIMIT 1",
        )
        .bind(t)
        .fetch_one(pool)
        .await?;
        assert_eq!(op, "append");

        let mut tx = pool.begin().await?;
        forget_memory(&mut tx, &cold, &key, t).await?;
        erase_memory(&mut tx, &cold, &owner, t).await?;
        tx.commit().await?;
        let stub: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.cooled WHERE t = $1")
                .bind(t)
                .fetch_one(pool)
                .await?;
        assert_eq!(stub, 0);

        let mut tx = pool.begin().await?;
        let err = erase_memory(&mut tx, &cold, &OwnerRef::World, t)
            .await
            .expect_err("World never");
        assert!(err.to_string().contains("World"), "got: {err}");

        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("forget test failed");
}
