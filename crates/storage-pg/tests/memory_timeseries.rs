//! Slice 2: FactIngest timeseries write/read + ingest_keys replay.

use proxima_core::storage_ports::OwnerWritePermit;
use proxima_core::verbs::fact_ingest::FactWriteCommand;
use proxima_core::{AccessKind, OwnerRef, SchemaId, SchemaVersion, UserId};
use proxima_pg_testkit::{create_db, db_url, drop_db};
use proxima_storage_pg::PgStorage;
use proxima_storage_pg::verbs::fact_ingest::ingest_fact_atomic;
use proxima_storage_pg::verbs::memory_timeseries::{read_memory_by_t, read_memory_head};
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
    }
}

#[tokio::test]
async fn memory_timeseries_keyless_and_ingest_key_replay() {
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

        let first = ingest_fact_atomic(pg.pool_for_tests(), &permit, &draft(None), None).await?;
        let second = ingest_fact_atomic(pg.pool_for_tests(), &permit, &draft(None), None).await?;
        assert_ne!(
            first.memory_id, second.memory_id,
            "keyless Fact must mint a new t"
        );
        assert_ne!(first.handle, second.handle);

        let sourced = draft(Some(("src/webhook", "delivery-1")));
        let a = ingest_fact_atomic(pg.pool_for_tests(), &permit, &sourced, None).await?;
        assert!(!a.idempotent_replay);
        let b = ingest_fact_atomic(pg.pool_for_tests(), &permit, &sourced, None).await?;
        assert!(b.idempotent_replay);
        assert_eq!(a.memory_id, b.memory_id);
        assert_eq!(a.handle, b.handle);

        let mut tx = pg.pool_for_tests().begin().await?;
        let by_t = read_memory_by_t(&mut tx, a.memory_id.into_inner())
            .await?
            .expect("read by t");
        let by_head = read_memory_head(&mut tx, a.handle)
            .await?
            .expect("read by head");
        tx.commit().await?;
        assert_eq!(by_t.t, a.memory_id.into_inner());
        assert_eq!(by_head.t, a.memory_id.into_inner());
        assert_eq!(by_t.handle, a.handle);
        assert_eq!(by_head.handle, a.handle);

        let head_t: Uuid = sqlx::query_scalar(
            "SELECT t FROM proxima_core.memory_head WHERE handle = $1",
        )
        .bind(a.handle)
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(head_t, a.memory_id.into_inner(), "replay must not bump head");

        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("memory_timeseries test failed");
}
