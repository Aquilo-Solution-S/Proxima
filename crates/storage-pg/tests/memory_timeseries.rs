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
        refs: Vec::new(),
        blob_id: None,
        kind: "fact".into(),
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

#[tokio::test]
async fn memory_timeseries_pins_blob_and_closed_handle() {
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

        let file = ingest_fact_atomic(pool, &permit, &draft(None), None).await?;
        let mut chunk = draft(None);
        chunk.refs = vec![file.memory_id.into_inner()];
        let chunk_out = ingest_fact_atomic(pool, &permit, &chunk, None).await?;
        let mut tx = pool.begin().await?;
        let row = read_memory_by_t(&mut tx, chunk_out.memory_id.into_inner())
            .await?
            .expect("chunk");
        tx.commit().await?;
        assert_eq!(row.refs, vec![file.memory_id.into_inner()]);

        let missing = Uuid::now_v7();
        let mut bad = draft(None);
        bad.refs = vec![missing];
        let err = ingest_fact_atomic(pool, &permit, &bad, None)
            .await
            .expect_err("missing pin");
        assert!(
            err.to_string().contains("does not exist") || err.to_string().contains("23503"),
            "got: {err}"
        );

        sqlx::query("INSERT INTO proxima_core.closed_handle (handle) VALUES ($1)")
            .bind(file.handle)
            .execute(pool)
            .await?;
        let mut closed = draft(None);
        closed.refs = vec![file.memory_id.into_inner()];
        let err = ingest_fact_atomic(pool, &permit, &closed, None)
            .await
            .expect_err("closed_handle");
        assert!(
            err.to_string().contains("closed_handle") || err.to_string().contains("23514"),
            "got: {err}"
        );

        let hash = vec![7u8; 32];
        let blob_id: Uuid = sqlx::query_scalar(
            "INSERT INTO proxima_core.blob (owner_id, schema_id, content_hash)
             VALUES ($1, 'core/bytes-v1', $2)
             RETURNING blob_id",
        )
        .bind(owner.stored_owner_id())
        .bind(&hash)
        .fetch_one(pool)
        .await?;
        let mut cited = draft(None);
        cited.blob_id = Some(blob_id);
        let cited_out = ingest_fact_atomic(pool, &permit, &cited, None).await?;
        let stored: Option<Uuid> =
            sqlx::query_scalar("SELECT blob_id FROM proxima_core.memory WHERE t = $1")
                .bind(cited_out.memory_id.into_inner())
                .fetch_one(pool)
                .await?;
        assert_eq!(stored, Some(blob_id));

        let edges: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM information_schema.tables
              WHERE table_schema = 'proxima_core' AND table_name = 'edges'",
        )
        .fetch_one(pool)
        .await?;
        assert_eq!(edges, 0, "no edges table");

        let mut abs = draft(None);
        abs.kind = "abstraction".into();
        abs.derived_from = vec![proxima_core::EdgeEndpoint::memory(
            proxima_core::EntityKind::Fact,
            chunk_out.memory_id,
        )];
        let abs_out = ingest_fact_atomic(pool, &permit, &abs, None)
            .await
            .expect("A origins Fact t");
        sqlx::query(
            "CREATE TABLE proxima_core.sidecar_sum (
                 t uuid PRIMARY KEY REFERENCES proxima_core.memory (t),
                 text text NOT NULL,
                 lexical_language regconfig NOT NULL DEFAULT 'simple'
             )",
        )
        .execute(pool)
        .await?;
        sqlx::query(
            "INSERT INTO proxima_core.sidecar_sum (t, text) VALUES ($1, 'summary')",
        )
        .bind(abs_out.memory_id.into_inner())
        .execute(pool)
        .await?;

        let mut persp = draft(None);
        persp.kind = "perspective".into();
        persp.blob_id = Some(blob_id);
        let err = ingest_fact_atomic(pool, &permit, &persp, None)
            .await
            .expect_err("P cannot cite");
        assert!(
            err.to_string().contains("blob") || err.to_string().contains("23514"),
            "got: {err}"
        );

        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("pin/blob test failed");
}
