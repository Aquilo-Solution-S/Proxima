//! Slice 6: forget / hydrate / erase.
#![allow(clippy::doc_markdown, clippy::too_many_lines)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use proxima_core::storage_ports::{MemoryAuthoringPort, OwnerWritePermit};
use proxima_core::verbs::fact_ingest::FactWriteCommand;
use proxima_core::{
    AccessKind, ColdObjectStore, OwnerRef, SchemaId, SchemaVersion, StorageError, UserId,
};
use proxima_pg_testkit::{create_db, db_url, drop_db};
use proxima_storage_pg::PgStorage;
use proxima_storage_pg::core_pg_sidecars;
use proxima_storage_pg::verbs::fact_ingest::ingest_fact_atomic;
use proxima_storage_pg::verbs::forget::{
    MemoryColdStore, cold_object_key, commit_forget, erase_memory, forget_memory, hydrate_memory,
    owner_hash_hex, snapshot_hot,
};
use proxima_storage_pg::verbs::memory_timeseries::ingest_fact_timeseries;
use uuid::Uuid;

const AGENT_NOTE: &str = "proxima_core.agent_note_v1";
const UTTERANCE: &str = "proxima_core.utterance_v1";
const GHOST_TABLE: &str = "proxima_core.w4_does_not_exist_v1";

async fn ingest_stamped(
    pool: &sqlx::PgPool,
    permit: &OwnerWritePermit,
    draft: &FactWriteCommand,
    tables: &[String],
) -> Result<proxima_core::verbs::fact_ingest::FactIngestOutcome, StorageError> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|err| StorageError::Internal(err.to_string()))?;
    let outcome = ingest_fact_timeseries(&mut tx, permit.owner(), draft, tables, None).await?;
    tx.commit()
        .await
        .map_err(|err| StorageError::Internal(err.to_string()))?;
    Ok(outcome)
}

async fn sidecar_tables_for(pool: &sqlx::PgPool, t: Uuid) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar("SELECT sidecar_tables FROM proxima_core.memory WHERE t = $1")
        .bind(t)
        .fetch_one(pool)
        .await
}

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
        let mut sourced = draft(Some(("src", "k1")));
        sourced.rendered_text = Some("Actual title\nbody".into());
        let written = ingest_fact_atomic(pool, &permit, &sourced, None).await?;
        let t = written.memory_id.into_inner();
        let stamped = sidecar_tables_for(pool, t).await?;
        assert!(
            stamped.is_empty(),
            "sidecar-less ingest stamps '{{}}'; forget still cools: {stamped:?}"
        );
        let sketch_before: String =
            sqlx::query_scalar("SELECT text FROM proxima_core.sketch WHERE t = $1")
                .bind(t)
                .fetch_one(pool)
                .await?;
        assert_eq!(sketch_before, "Actual title");

        let key = cold_object_key("ownerhash", written.handle, t);
        assert!(key.starts_with("cold/"));
        assert!(!key.contains(&owner.stored_owner_id().to_string()));

        let cold = MemoryColdStore::default();
        let mut tx = pool.begin().await?;
        forget_memory(&mut tx, &core_pg_sidecars(), &cold, &key, t).await?;
        tx.commit().await?;

        let sketches: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.sketch WHERE t = $1")
                .bind(t)
                .fetch_one(pool)
                .await?;
        assert_eq!(sketches, 0, "forget must delete the hot sketch");

        let hot: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.memory WHERE t = $1")
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
        let heads: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.memory_head WHERE handle = $1",
        )
        .bind(written.handle)
        .fetch_one(pool)
        .await?;
        assert_eq!(heads, 0, "P3: last-t forget deletes the head");
        let keys: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.ingest_keys WHERE ingest_key = 'k1'",
        )
        .fetch_one(pool)
        .await?;
        assert_eq!(keys, 1, "forget does not touch ingest_keys");

        let mut tx = pool.begin().await?;
        hydrate_memory(&mut tx, &core_pg_sidecars(), &cold, t).await?;
        tx.commit().await?;
        let hot: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.memory WHERE t = $1")
                .bind(t)
                .fetch_one(pool)
                .await?;
        assert_eq!(hot, 1);
        let head_t: Uuid =
            sqlx::query_scalar("SELECT t FROM proxima_core.memory_head WHERE handle = $1")
                .bind(written.handle)
                .fetch_one(pool)
                .await?;
        assert_eq!(head_t, t, "P3: hydrate recreates head at the same t");
        let sketch_after: String =
            sqlx::query_scalar("SELECT text FROM proxima_core.sketch WHERE t = $1")
                .bind(t)
                .fetch_one(pool)
                .await?;
        assert_eq!(sketch_after, "Actual title");
        let restored: (Option<String>, Option<String>, Vec<Uuid>, Vec<Uuid>) = sqlx::query_as(
            "SELECT source_id, ingest_key, origins, refs FROM proxima_core.memory WHERE t = $1",
        )
        .bind(t)
        .fetch_one(pool)
        .await?;
        assert_eq!(restored.0.as_deref(), Some("src"));
        assert_eq!(restored.1.as_deref(), Some("k1"));
        let op: String = sqlx::query_scalar(
            "SELECT op::text FROM proxima_core.announce WHERE t = $1 ORDER BY seq DESC LIMIT 1",
        )
        .bind(t)
        .fetch_one(pool)
        .await?;
        assert_eq!(op, "append");

        let mut tx = pool.begin().await?;
        forget_memory(&mut tx, &core_pg_sidecars(), &cold, &key, t).await?;
        erase_memory(&mut tx, &core_pg_sidecars(), &cold, &owner, t).await?;
        tx.commit().await?;
        let stub: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.cooled WHERE t = $1")
                .bind(t)
                .fetch_one(pool)
                .await?;
        assert_eq!(stub, 0);

        let mut tx = pool.begin().await?;
        let err = erase_memory(&mut tx, &core_pg_sidecars(), &cold, &OwnerRef::World, t)
            .await
            .expect_err("World never");
        assert!(err.to_string().contains("World"), "got: {err}");

        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("forget test failed");
}

#[tokio::test]
async fn engine_forget_puts_held_store_hydrate_restores_same_t() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let cold = Arc::new(MemoryColdStore::default());
        let pg = PgStorage::connect(&url).await?.with_cold(cold.clone());
        pg.run_migrations().await?;
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let pool = pg.pool_for_tests();

        let origin = ingest_fact_atomic(pool, &permit, &draft(None), None).await?;
        let mut sourced = draft(Some(("src", "k-held")));
        sourced.refs = vec![origin.memory_id.into_inner()];
        let written = ingest_stamped(pool, &permit, &sourced, &[AGENT_NOTE.to_owned()]).await?;
        let t = written.memory_id.into_inner();
        assert_eq!(sidecar_tables_for(pool, t).await?, vec![AGENT_NOTE]);
        sqlx::query(
            "INSERT INTO proxima_core.agent_note_v1 (t, note_id, title, body, tags)
             VALUES ($1, $1, 'n', 'sidecar body', ARRAY['tag'])",
        )
        .bind(t)
        .execute(pool)
        .await?;
        sqlx::query(
            "INSERT INTO proxima_core.embeddings
                (entity_id, model_id, embedding_version, vec, owner_id)
             VALUES ($1, 'test-embed', 1, $3::vector, $2)",
        )
        .bind(t)
        .bind(owner.stored_owner_id())
        .bind(format!(
            "[{}]",
            std::iter::once("1")
                .chain(std::iter::repeat_n("0", 1023))
                .collect::<Vec<_>>()
                .join(",")
        ))
        .execute(pool)
        .await?;

        MemoryAuthoringPort::forget_memory(&pg, &permit, written.memory_id).await?;

        let key = cold_object_key(&owner_hash_hex(&owner), written.handle, t);
        let payload = cold.get(&key).await?;
        assert!(
            payload.len() > 64,
            "held store must receive the full memory+sidecar record, got {} bytes",
            payload.len()
        );
        let hot: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.memory WHERE t = $1")
                .bind(t)
                .fetch_one(pool)
                .await?;
        assert_eq!(hot, 0);
        let embed_hot: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.embeddings WHERE entity_id = $1",
        )
        .bind(t)
        .fetch_one(pool)
        .await?;
        assert_eq!(embed_hot, 0, "forget drops vectors");

        let mut tx = pool.begin().await?;
        hydrate_memory(&mut tx, pg.sidecars(), cold.as_ref(), t).await?;
        tx.commit().await?;

        let restored: (Uuid, Vec<Uuid>, Vec<Uuid>) =
            sqlx::query_as("SELECT t, origins, refs FROM proxima_core.memory WHERE t = $1")
                .bind(t)
                .fetch_one(pool)
                .await?;
        assert_eq!(restored.0, t, "hydrate restores the same t");
        assert_eq!(restored.2, vec![origin.memory_id.into_inner()]);
        let note: String =
            sqlx::query_scalar("SELECT body FROM proxima_core.agent_note_v1 WHERE t = $1")
                .bind(t)
                .fetch_one(pool)
                .await?;
        assert_eq!(note, "sidecar body");
        let jobs: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.embedding_jobs
              WHERE entity_id = $1 AND model_id = 'test-embed'",
        )
        .bind(t)
        .fetch_one(pool)
        .await?;
        assert_eq!(jobs, 1, "hydrate enqueues embed; vectors stay out of S3");
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("engine forget/hydrate test failed");
}

#[tokio::test]
async fn forget_non_last_t_rewinds_memory_head() {
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
        let first = ingest_fact_atomic(pool, &permit, &draft(None), None).await?;
        let mut later = draft(None);
        later.handle = Some(first.handle);
        let second = ingest_fact_atomic(pool, &permit, &later, None).await?;
        assert_eq!(second.handle, first.handle);
        assert_ne!(second.memory_id, first.memory_id);

        let cold = MemoryColdStore::default();
        let key = cold_object_key("ownerhash", second.handle, second.memory_id.into_inner());
        let mut tx = pool.begin().await?;
        forget_memory(
            &mut tx,
            &core_pg_sidecars(),
            &cold,
            &key,
            second.memory_id.into_inner(),
        )
        .await?;
        tx.commit().await?;

        let head_t: Uuid =
            sqlx::query_scalar("SELECT t FROM proxima_core.memory_head WHERE handle = $1")
                .bind(first.handle)
                .fetch_one(pool)
                .await?;
        assert_eq!(head_t, first.memory_id.into_inner());
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("forget rewind failed");
}

struct UnlockOnPut {
    pool: sqlx::PgPool,
    t: Uuid,
    puts: AtomicUsize,
}

#[async_trait::async_trait]
impl ColdObjectStore for UnlockOnPut {
    async fn put(&self, _key: &str, _bytes: &[u8]) -> Result<(), StorageError> {
        self.puts.fetch_add(1, Ordering::SeqCst);
        sqlx::query_scalar::<_, Uuid>(
            "SELECT t FROM proxima_core.memory WHERE t = $1 FOR UPDATE NOWAIT",
        )
        .bind(self.t)
        .fetch_one(&self.pool)
        .await
        .map_err(|err| StorageError::Internal(format!("row locked during put: {err}")))?;
        Ok(())
    }

    async fn get(&self, _key: &str) -> Result<Vec<u8>, StorageError> {
        Err(StorageError::NotFound)
    }

    async fn delete(&self, _key: &str) -> Result<(), StorageError> {
        Ok(())
    }
}

struct CountingCold {
    inner: MemoryColdStore,
    puts: AtomicUsize,
}

#[async_trait::async_trait]
impl ColdObjectStore for CountingCold {
    async fn put(&self, key: &str, bytes: &[u8]) -> Result<(), StorageError> {
        self.puts.fetch_add(1, Ordering::SeqCst);
        self.inner.put(key, bytes).await
    }

    async fn get(&self, key: &str) -> Result<Vec<u8>, StorageError> {
        self.inner.get(key).await
    }

    async fn delete(&self, key: &str) -> Result<(), StorageError> {
        self.inner.delete(key).await
    }
}

#[tokio::test]
async fn oneshot_forget_put_does_not_hold_row_lock() {
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
        let pool = pg.pool_for_tests().clone();
        let written = ingest_fact_atomic(&pool, &permit, &draft(None), None).await?;
        let t = written.memory_id.into_inner();
        let probe = Arc::new(UnlockOnPut {
            pool,
            t,
            puts: AtomicUsize::new(0),
        });
        let pg = pg.with_cold(probe.clone());
        MemoryAuthoringPort::forget_memory(&pg, &permit, written.memory_id).await?;
        assert_eq!(probe.puts.load(Ordering::SeqCst), 1);
        let hot: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.memory WHERE t = $1")
                .bind(t)
                .fetch_one(pg.pool_for_tests())
                .await?;
        assert_eq!(hot, 0);
        let stub: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.cooled WHERE t = $1")
                .bind(t)
                .fetch_one(pg.pool_for_tests())
                .await?;
        assert_eq!(stub, 1);
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("oneshot forget lock probe failed");
}

#[tokio::test]
async fn commit_forget_reputs_when_sidecar_changed() {
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
        let written = ingest_stamped(pool, &permit, &draft(None), &[AGENT_NOTE.to_owned()]).await?;
        let t = written.memory_id.into_inner();
        sqlx::query(
            "INSERT INTO proxima_core.agent_note_v1 (t, note_id, title, body, tags)
             VALUES ($1, $1, 'n', 'sidecar body', ARRAY['tag'])",
        )
        .bind(t)
        .execute(pool)
        .await?;

        let mut conn = pool.acquire().await?;
        let snapshot = snapshot_hot(&mut conn, &core_pg_sidecars(), t).await?;
        drop(conn);

        sqlx::query("UPDATE proxima_core.agent_note_v1 SET body = 'updated' WHERE t = $1")
            .bind(t)
            .execute(pool)
            .await?;

        let cold = CountingCold {
            inner: MemoryColdStore::default(),
            puts: AtomicUsize::new(0),
        };
        let key = cold_object_key(&owner_hash_hex(&owner), written.handle, t);
        let mut tx = pool.begin().await?;
        commit_forget(&mut tx, &core_pg_sidecars(), &cold, &key, &snapshot).await?;
        tx.commit().await?;
        assert!(
            cold.puts.load(Ordering::SeqCst) >= 1,
            "locked dump must re-PUT after sidecar change"
        );

        let mut tx = pool.begin().await?;
        hydrate_memory(&mut tx, &core_pg_sidecars(), &cold, t).await?;
        tx.commit().await?;
        let note: String =
            sqlx::query_scalar("SELECT body FROM proxima_core.agent_note_v1 WHERE t = $1")
                .bind(t)
                .fetch_one(pool)
                .await?;
        assert_eq!(note, "updated");
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("commit_forget re-PUT test failed");
}

#[tokio::test]
async fn forget_dumps_only_stamped_tables_and_skips_unregistered_scan() {
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
        let sidecars = core_pg_sidecars().with_unusable_memory_table("w4/ghost-v1", GHOST_TABLE);

        let written = ingest_stamped(pool, &permit, &draft(None), &[AGENT_NOTE.to_owned()]).await?;
        let t = written.memory_id.into_inner();
        sqlx::query(
            "INSERT INTO proxima_core.agent_note_v1 (t, note_id, title, body, tags)
             VALUES ($1, $1, 'n', 'stamped', ARRAY['tag'])",
        )
        .bind(t)
        .execute(pool)
        .await?;

        let cold = MemoryColdStore::default();
        let key = cold_object_key("ownerhash", written.handle, t);
        let mut tx = pool.begin().await?;
        forget_memory(&mut tx, &sidecars, &cold, &key, t).await?;
        tx.commit().await?;

        let notes: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.agent_note_v1 WHERE t = $1",
        )
        .bind(t)
        .fetch_one(pool)
        .await?;
        assert_eq!(notes, 0, "stamped sidecar is dumped and deleted");

        let mut tx = pool.begin().await?;
        hydrate_memory(&mut tx, &sidecars, &cold, t).await?;
        tx.commit().await?;
        let note: String =
            sqlx::query_scalar("SELECT body FROM proxima_core.agent_note_v1 WHERE t = $1")
                .bind(t)
                .fetch_one(pool)
                .await?;
        assert_eq!(note, "stamped");
        let restored = sidecar_tables_for(pool, t).await?;
        assert_eq!(restored, vec![AGENT_NOTE]);
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("stamped-only forget failed");
}

#[tokio::test]
async fn forget_dumps_every_stamped_extra() {
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
        let tables = [AGENT_NOTE.to_owned(), UTTERANCE.to_owned()];
        let written = ingest_stamped(pool, &permit, &draft(None), &tables).await?;
        let t = written.memory_id.into_inner();
        assert_eq!(sidecar_tables_for(pool, t).await?, tables);
        sqlx::query(
            "INSERT INTO proxima_core.agent_note_v1 (t, note_id, title, body, tags)
             VALUES ($1, $1, 'n', 'note', ARRAY['tag'])",
        )
        .bind(t)
        .execute(pool)
        .await?;
        sqlx::query(
            "INSERT INTO proxima_core.utterance_v1 (t, speaker, conversation_id, text)
             VALUES ($1, 'user', 'c1', 'said')",
        )
        .bind(t)
        .execute(pool)
        .await?;

        let cold = MemoryColdStore::default();
        let key = cold_object_key("ownerhash", written.handle, t);
        let mut tx = pool.begin().await?;
        forget_memory(&mut tx, &core_pg_sidecars(), &cold, &key, t).await?;
        tx.commit().await?;
        let leftover: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.agent_note_v1 WHERE t = $1",
        )
        .bind(t)
        .fetch_one(pool)
        .await?;
        assert_eq!(leftover, 0);
        let leftover_u: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.utterance_v1 WHERE t = $1",
        )
        .bind(t)
        .fetch_one(pool)
        .await?;
        assert_eq!(leftover_u, 0);

        let mut tx = pool.begin().await?;
        hydrate_memory(&mut tx, &core_pg_sidecars(), &cold, t).await?;
        tx.commit().await?;
        assert_eq!(sidecar_tables_for(pool, t).await?, tables);
        let note: String =
            sqlx::query_scalar("SELECT body FROM proxima_core.agent_note_v1 WHERE t = $1")
                .bind(t)
                .fetch_one(pool)
                .await?;
        let said: String =
            sqlx::query_scalar("SELECT text FROM proxima_core.utterance_v1 WHERE t = $1")
                .bind(t)
                .fetch_one(pool)
                .await?;
        assert_eq!(note, "note");
        assert_eq!(said, "said");
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("extras forget failed");
}
