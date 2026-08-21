//! Slice 6: forget / hydrate / erase.
#![allow(clippy::doc_markdown, clippy::too_many_lines)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use proxima_core::storage_ports::{MemoryAuthoringPort, OwnerTransferPort, OwnerWritePermit};
use proxima_core::verbs::fact_ingest::{CitationSpec, FactWriteCommand};
use proxima_core::{
    AccessKind, ColdObjectStore, EdgeEndpoint, EntityId, EntityKind, GroupId, OwnerRef, SchemaId,
    SchemaVersion, StorageError, UserId,
};
use proxima_pg_testkit::{create_db, db_url, drop_db};
use proxima_storage_pg::PgStorage;
use proxima_storage_pg::core_pg_sidecars;
use proxima_storage_pg::verbs::fact_ingest::ingest_fact_atomic;
use proxima_storage_pg::verbs::forget::{
    MemoryColdStore, cold_object_key, commit_forget, erase_memory, forget_memory,
    forget_memory_oneshot, hydrate_memory, purge_cold_objects_after_commit, snapshot_hot,
};
use proxima_storage_pg::verbs::memory_timeseries::ingest_fact_timeseries;
use uuid::Uuid;

/// The forget's registry-resolved legs, exactly as `PgStorage` assembles
/// them. An alias for [`transfer_surfaces`]: both verbs read the same set,
/// and calling it by the verb under test is what keeps a reader from
/// wondering whether they differ.
fn surfaces() -> proxima_core::owner_inverse::OwnerSurfaces {
    transfer_surfaces()
}

/// The transfer's registry-resolved legs, exactly as the engine assembles
/// them. Passing a hand-built set here would test a registry production
/// never sees.
fn transfer_surfaces() -> proxima_core::owner_inverse::OwnerSurfaces {
    proxima_core::owner_inverse::OwnerSurfaces::for_registry(
        &proxima_core::FlavorRegistry::new().freeze_or_panic_for_tests(),
    )
}

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
async fn forget_hydrate_and_erase() {
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

        let key = cold_object_key(t);
        assert!(key.starts_with("cold/"));
        assert!(!key.contains(&owner.stored_owner_id().to_string()));

        let cold = MemoryColdStore::default();
        let mut tx = pool.begin().await?;
        forget_memory(
            &mut tx,
            &core_pg_sidecars(),
            &surfaces(),
            &cold,
            &key,
            t,
            owner.stored_owner_id(),
        )
        .await?;
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
        forget_memory(
            &mut tx,
            &core_pg_sidecars(),
            &surfaces(),
            &cold,
            &key,
            t,
            owner.stored_owner_id(),
        )
        .await?;
        let plan = erase_memory(&mut tx, &core_pg_sidecars(), &surfaces(), &owner, t).await?;
        tx.commit().await?;
        assert_eq!(
            plan.object_keys(),
            std::slice::from_ref(&key),
            "the erase owes the cold object it just unlinked"
        );
        let purge = purge_cold_objects_after_commit(pool, &cold, &plan).await;
        assert_eq!(purge.purged, 1);
        assert!(!purge.pending);
        assert!(
            matches!(cold.get(&key).await, Err(StorageError::NotFound)),
            "the post-commit purge destroys the object"
        );
        let stub: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.cooled WHERE t = $1")
                .bind(t)
                .fetch_one(pool)
                .await?;
        assert_eq!(stub, 0);
        let pending: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.cold_purge_pending WHERE object_key = $1",
        )
        .bind(&key)
        .fetch_one(pool)
        .await?;
        assert_eq!(pending, 0, "a purged object clears its pending mark");

        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("forget test failed");
}

/// An erase announce is a ChangeHistory event, and a reader pages events by
/// series handle. Binding `t` into the handle column made every erase event
/// name a series that does not exist.
#[tokio::test]
async fn erase_announce_carries_the_series_handle() {
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

        // Two t on one handle, so a handle-shaped and a t-shaped value differ.
        let first = ingest_fact_atomic(pool, &permit, &draft(Some(("src", "e1"))), None).await?;
        let mut second_draft = draft(Some(("src", "e2")));
        second_draft.handle = Some(first.handle);
        let second = ingest_fact_atomic(pool, &permit, &second_draft, None).await?;
        assert_eq!(second.handle, first.handle);
        let t = second.memory_id.into_inner();
        assert_ne!(t, second.handle, "the erased t is not its own handle");

        let mut tx = pool.begin().await?;
        erase_memory(&mut tx, &core_pg_sidecars(), &surfaces(), &owner, t).await?;
        tx.commit().await?;

        let announced: Uuid = sqlx::query_scalar(
            "SELECT handle FROM proxima_core.announce
              WHERE t = $1 AND op = 'erase'
              ORDER BY seq DESC LIMIT 1",
        )
        .bind(t)
        .fetch_one(pool)
        .await?;
        assert_eq!(
            announced, first.handle,
            "the erase announce must name the series, not the erased t"
        );
        let series_heads: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.memory_head WHERE handle = $1",
        )
        .bind(announced)
        .fetch_one(pool)
        .await?;
        assert_eq!(
            series_heads, 1,
            "the announced handle resolves to a live series"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("erase announce handle test failed");
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

        let key = cold_object_key(t);
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
        let key = cold_object_key(second.memory_id.into_inner());
        let mut tx = pool.begin().await?;
        forget_memory(
            &mut tx,
            &core_pg_sidecars(),
            &surfaces(),
            &cold,
            &key,
            second.memory_id.into_inner(),
            owner.stored_owner_id(),
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

struct BlockingPutCold {
    inner: MemoryColdStore,
    first_put_entered: tokio::sync::Semaphore,
    release_first_put: tokio::sync::Semaphore,
    puts: AtomicUsize,
    deletes: AtomicUsize,
}

#[async_trait::async_trait]
impl ColdObjectStore for BlockingPutCold {
    async fn put(&self, key: &str, bytes: &[u8]) -> Result<(), StorageError> {
        let put_index = self.puts.fetch_add(1, Ordering::SeqCst);
        if put_index == 0 {
            self.first_put_entered.add_permits(1);
            self.release_first_put
                .acquire()
                .await
                .map_err(|err| StorageError::Internal(err.to_string()))?
                .forget();
        }
        self.inner.put(key, bytes).await
    }

    async fn get(&self, key: &str) -> Result<Vec<u8>, StorageError> {
        self.inner.get(key).await
    }

    async fn delete(&self, key: &str) -> Result<(), StorageError> {
        self.deletes.fetch_add(1, Ordering::SeqCst);
        self.inner.delete(key).await
    }
}

#[tokio::test]
async fn concurrent_forget_serializes_before_cold_put() {
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
        let written = ingest_fact_atomic(pg.pool_for_tests(), &permit, &draft(None), None).await?;
        let t = written.memory_id.into_inner();
        let key = cold_object_key(t);
        let cold = Arc::new(BlockingPutCold {
            inner: MemoryColdStore::default(),
            first_put_entered: tokio::sync::Semaphore::new(0),
            release_first_put: tokio::sync::Semaphore::new(0),
            puts: AtomicUsize::new(0),
            deletes: AtomicUsize::new(0),
        });

        let first = {
            let pool = pg.pool_for_tests().clone();
            let cold = cold.clone();
            let key = key.clone();
            tokio::spawn(async move {
                let sidecars = core_pg_sidecars();
                forget_memory_oneshot(
                    &pool,
                    &sidecars,
                    &surfaces(),
                    cold.as_ref(),
                    &key,
                    t,
                    owner.stored_owner_id(),
                )
                .await
            })
        };
        cold.first_put_entered.acquire().await?.forget();

        let mut second = {
            let pool = pg.pool_for_tests().clone();
            let cold = cold.clone();
            let key = key.clone();
            tokio::spawn(async move {
                let sidecars = core_pg_sidecars();
                forget_memory_oneshot(
                    &pool,
                    &sidecars,
                    &surfaces(),
                    cold.as_ref(),
                    &key,
                    t,
                    owner.stored_owner_id(),
                )
                .await
            })
        };
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut second)
                .await
                .is_err(),
            "the second forget must wait before PUT"
        );

        cold.release_first_put.add_permits(1);
        first.await??;
        let second_error = second
            .await?
            .expect_err("the serialized loser must observe the row already cooled");
        assert!(matches!(second_error, StorageError::NotFound));
        assert_eq!(cold.puts.load(Ordering::SeqCst), 1);
        assert_eq!(cold.deletes.load(Ordering::SeqCst), 0);
        assert!(!cold.get(&key).await?.is_empty());
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("concurrent forget serialization test failed");
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

/// `commit_forget` re-PUTs when the locked dump differs from the snapshot.
///
/// This used to make them differ with an `UPDATE` of the sidecar's `body`.
/// It cannot any more: `agent_note_v1` is append-only WITH the projection,
/// because the projection row is derived once and an in-place text edit
/// would leave the vector describing text that is gone. A row that lands
/// BETWEEN the snapshot and the lock is the remaining — and the only real
/// — way for the dump to move, and it is the case the function's doc
/// comment names ("late sidecar").
#[tokio::test]
async fn commit_forget_reputs_when_a_sidecar_row_lands_after_the_snapshot() {
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

        // Stamped, but the sidecar row is not there yet.
        let mut conn = pool.acquire().await?;
        let snapshot = snapshot_hot(&mut conn, &core_pg_sidecars(), t).await?;
        drop(conn);

        sqlx::query(
            "INSERT INTO proxima_core.agent_note_v1 (t, note_id, title, body, tags)
             VALUES ($1, $1, 'n', 'late body', ARRAY['tag'])",
        )
        .bind(t)
        .execute(pool)
        .await?;

        let cold = CountingCold {
            inner: MemoryColdStore::default(),
            puts: AtomicUsize::new(0),
        };
        let key = cold_object_key(t);
        let mut tx = pool.begin().await?;
        commit_forget(
            &mut tx,
            &core_pg_sidecars(),
            &surfaces(),
            &cold,
            &key,
            &snapshot,
            owner.stored_owner_id(),
        )
        .await?;
        tx.commit().await?;
        assert!(
            cold.puts.load(Ordering::SeqCst) >= 1,
            "locked dump must re-PUT after a late sidecar row"
        );

        let mut tx = pool.begin().await?;
        hydrate_memory(&mut tx, &core_pg_sidecars(), &cold, t).await?;
        tx.commit().await?;
        let note: String =
            sqlx::query_scalar("SELECT body FROM proxima_core.agent_note_v1 WHERE t = $1")
                .bind(t)
                .fetch_one(pool)
                .await?;
        assert_eq!(
            note, "late body",
            "the re-PUT dump, not the snapshot, is what hydrate restores"
        );
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
        let key = cold_object_key(t);
        let mut tx = pool.begin().await?;
        forget_memory(
            &mut tx,
            &sidecars,
            &surfaces(),
            &cold,
            &key,
            t,
            owner.stored_owner_id(),
        )
        .await?;
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
        let key = cold_object_key(t);
        let mut tx = pool.begin().await?;
        forget_memory(
            &mut tx,
            &core_pg_sidecars(),
            &surfaces(),
            &cold,
            &key,
            t,
            owner.stored_owner_id(),
        )
        .await?;
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
        // A memory has ONE schema id and `projection` is keyed
        // `(memory_id, schema_id)`, so hydrate rebuilds at most one row: the
        // one for the memory's OWN schema. The rebuild loop runs per DUMPED
        // TABLE, and selecting the statement by table alone made it write a
        // row per stamped extra, each claiming a schema this memory is not.
        //
        // Here the answer is none of them. This fixture's memory is
        // `core/test-fact-v1`, which declares no projection over either
        // stamped table, so neither dump can produce a row — the same answer
        // the write path gives. If that schema ever gains a projection this
        // assertion is the place to decide what the extras should do.
        let projected: Vec<String> = sqlx::query_scalar(
            "SELECT schema_id FROM proxima_core.projection WHERE memory_id = $1",
        )
        .bind(t)
        .fetch_all(pool)
        .await?;
        assert!(
            projected.is_empty(),
            "a stamped extra must not earn a projection row of its own; got \
             {projected:?}"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("extras forget failed");
}

fn derived_abstraction(origin_kind: EntityKind, origin: Uuid) -> FactWriteCommand {
    let mut cmd = draft(None);
    cmd.kind = "abstraction".into();
    cmd.derived_from = vec![EdgeEndpoint::memory(
        origin_kind,
        proxima_core::MemoryId::new(origin),
    )];
    cmd
}

#[tokio::test]
async fn forget_and_admit_preserve_grounding_support() {
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
        let cold = MemoryColdStore::default();

        let fact = ingest_fact_atomic(pool, &permit, &draft(None), None).await?;
        let abs = ingest_fact_atomic(
            pool,
            &permit,
            &derived_abstraction(EntityKind::Fact, fact.memory_id.into_inner()),
            None,
        )
        .await
        .expect("A from Fact");
        let abs2 = ingest_fact_atomic(
            pool,
            &permit,
            &derived_abstraction(EntityKind::Abstraction, abs.memory_id.into_inner()),
            None,
        )
        .await
        .expect("A2 from A");

        let mut tx = pool.begin().await?;
        let err = forget_memory(
            &mut tx,
            &core_pg_sidecars(),
            &surfaces(),
            &cold,
            "cold/refuse-a",
            abs.memory_id.into_inner(),
            owner.stored_owner_id(),
        )
        .await
        .expect_err("forget A while A2 pins only A");
        assert!(
            err.to_string().contains("ungrounded") || err.to_string().contains("23514"),
            "got: {err}"
        );
        tx.rollback().await?;
        let still_hot: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.memory WHERE t = $1")
                .bind(abs.memory_id.into_inner())
                .fetch_one(pool)
                .await?;
        assert_eq!(still_hot, 1, "refused forget must leave A hot");

        let mut tx = pool.begin().await?;
        forget_memory(
            &mut tx,
            &core_pg_sidecars(),
            &surfaces(),
            &cold,
            "cold/fact",
            fact.memory_id.into_inner(),
            owner.stored_owner_id(),
        )
        .await
        .expect("forget Fact under A is a cooled-Fact leaf");
        tx.commit().await?;

        let mut mixed = draft(None);
        mixed.kind = "abstraction".into();
        mixed.derived_from = vec![
            EdgeEndpoint::memory(EntityKind::Abstraction, abs.memory_id),
            EdgeEndpoint::memory(EntityKind::Fact, fact.memory_id),
        ];
        let mixed_abs = ingest_fact_atomic(pool, &permit, &mixed, None)
            .await
            .expect("A from hot A + cooled Fact");

        let mut tx = pool.begin().await?;
        forget_memory(
            &mut tx,
            &core_pg_sidecars(),
            &surfaces(),
            &cold,
            "cold/mixed-src",
            abs.memory_id.into_inner(),
            owner.stored_owner_id(),
        )
        .await
        .expect_err("A2 still pins only A");
        tx.rollback().await?;

        let mut tx = pool.begin().await?;
        forget_memory(
            &mut tx,
            &core_pg_sidecars(),
            &surfaces(),
            &cold,
            "cold/a2",
            abs2.memory_id.into_inner(),
            owner.stored_owner_id(),
        )
        .await
        .expect("forget A2 (no dependers)");
        tx.commit().await?;

        let mut tx = pool.begin().await?;
        forget_memory(
            &mut tx,
            &core_pg_sidecars(),
            &surfaces(),
            &cold,
            "cold/a-after-a2",
            abs.memory_id.into_inner(),
            owner.stored_owner_id(),
        )
        .await
        .expect("forget A after A2 gone; mixed_abs still has cooled Fact");
        tx.commit().await?;

        let err = ingest_fact_atomic(
            pool,
            &permit,
            &derived_abstraction(EntityKind::Abstraction, abs.memory_id.into_inner()),
            None,
        )
        .await
        .expect_err("admit A from cooled A only");
        assert!(
            err.to_string().contains("cooled fact") || err.to_string().contains("23514"),
            "got: {err}"
        );

        ingest_fact_atomic(
            pool,
            &permit,
            &derived_abstraction(EntityKind::Fact, fact.memory_id.into_inner()),
            None,
        )
        .await
        .expect("admit A from cooled Fact");

        let _ = mixed_abs;
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("grounding-support forget/admit test failed");
}

#[tokio::test]
async fn refused_forget_does_not_leave_untracked_cold_object() {
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
        let cold = MemoryColdStore::default();
        let fact = ingest_fact_atomic(pool, &permit, &draft(None), None).await?;
        let abs = ingest_fact_atomic(
            pool,
            &permit,
            &derived_abstraction(EntityKind::Fact, fact.memory_id.into_inner()),
            None,
        )
        .await?;
        let abs2 = ingest_fact_atomic(
            pool,
            &permit,
            &derived_abstraction(EntityKind::Abstraction, abs.memory_id.into_inner()),
            None,
        )
        .await?;
        let key = "cold/refuse-orphan";
        let mut tx = pool.begin().await?;
        forget_memory(
            &mut tx,
            &core_pg_sidecars(),
            &surfaces(),
            &cold,
            key,
            abs.memory_id.into_inner(),
            owner.stored_owner_id(),
        )
        .await
        .expect_err("A2 still pins only A");
        tx.rollback().await?;
        let leftover = ColdObjectStore::get(&cold, key).await;
        assert!(
            matches!(leftover, Err(StorageError::NotFound)),
            "refused forget must not leave an untracked cold object: {leftover:?}"
        );
        let _ = (fact, abs2);
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("refused-forget orphan test failed");
}

#[tokio::test]
async fn concurrent_erase_after_forget_put_does_not_leave_cold_object() {
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
        let owner_id = owner.stored_owner_id();
        let key = cold_object_key(t);
        let cold = Arc::new(BlockingPutCold {
            inner: MemoryColdStore::default(),
            first_put_entered: tokio::sync::Semaphore::new(0),
            release_first_put: tokio::sync::Semaphore::new(0),
            puts: AtomicUsize::new(0),
            deletes: AtomicUsize::new(0),
        });

        let forget = {
            let pool = pool.clone();
            let cold = Arc::clone(&cold);
            let key = key.clone();
            tokio::spawn(async move {
                forget_memory_oneshot(
                    &pool,
                    &core_pg_sidecars(),
                    &surfaces(),
                    cold.as_ref(),
                    &key,
                    t,
                    owner_id,
                )
                .await
            })
        };
        cold.first_put_entered.acquire().await?.forget();

        let mut erase_tx = pool.begin().await?;
        erase_memory(&mut erase_tx, &core_pg_sidecars(), &surfaces(), &owner, t).await?;
        erase_tx.commit().await?;

        // The PUT completes only after erase committed. The forget reread is
        // then NotFound, with no cooled locator allowed to retain the object.
        cold.release_first_put.add_permits(1);
        let err = forget
            .await?
            .expect_err("hard erase must win the overlapping forget");
        assert!(matches!(err, StorageError::NotFound), "got {err:?}");
        assert_eq!(cold.puts.load(Ordering::SeqCst), 1);
        assert_eq!(cold.deletes.load(Ordering::SeqCst), 1);
        assert!(
            matches!(cold.get(&key).await, Err(StorageError::NotFound)),
            "a completed hard erase must not leave an untracked cold payload"
        );

        let rows: i64 = sqlx::query_scalar(
            "SELECT (SELECT count(*) FROM proxima_core.memory WHERE t = $1)
                  + (SELECT count(*) FROM proxima_core.cooled WHERE t = $1)",
        )
        .bind(t)
        .fetch_one(&pool)
        .await?;
        assert_eq!(rows, 0, "erase removes both hot and cooled state");
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("concurrent erase/forget cleanup test failed");
}

/// Pinned shape for the loser of a double forget and for a plain second
/// call: `NotFound`, the same answer an unknown `t` gets. Not `Ok`, because
/// the identical miss is produced by a concurrent transfer that moved the row
/// out of the caller's ownership, where reporting success would be a lie.
/// Either way the attempt leaves the existing cooled object untouched.
#[tokio::test]
async fn forget_of_an_already_cooled_t_reports_not_found() {
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
        let pool = pg.pool_for_tests().clone();
        let written = ingest_fact_atomic(&pool, &permit, &draft(None), None).await?;
        let t = written.memory_id.into_inner();
        let key = cold_object_key(t);

        MemoryAuthoringPort::forget_memory(&pg, &permit, written.memory_id).await?;

        let err = MemoryAuthoringPort::forget_memory(&pg, &permit, written.memory_id)
            .await
            .expect_err("second forget of the same t");
        assert!(matches!(err, StorageError::NotFound), "got {err:?}");

        let mut tx = pool.begin().await?;
        let err = forget_memory(
            &mut tx,
            &core_pg_sidecars(),
            &surfaces(),
            cold.as_ref(),
            &key,
            t,
            owner.stored_owner_id(),
        )
        .await
        .expect_err("verb-level forget of a cooled t");
        assert!(matches!(err, StorageError::NotFound), "got {err:?}");
        tx.rollback().await?;

        ColdObjectStore::get(cold.as_ref(), &key)
            .await
            .expect("a refused re-forget must not delete the cooled object");
        let mut tx = pool.begin().await?;
        hydrate_memory(&mut tx, &core_pg_sidecars(), cold.as_ref(), t).await?;
        tx.commit().await?;
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("already-cooled forget outcome test failed");
}

/// Forget keeps `ingest_keys` and moves the handle to the cooled stub, so an
/// at-least-once source re-delivering a cooled admission must still get the
/// idempotent replay carrying the original handle and citation.
#[tokio::test]
async fn redelivering_a_cooled_ingest_key_is_an_idempotent_replay() {
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
        let mut sourced = draft(Some(("src/webhook", "delivery-cooled")));
        sourced.citation = Some(
            CitationSpec::v1(
                "core/test-cited-object-v1",
                [7_u8; 32],
                "core/test-citation-mapping-v1",
            )
            .into(),
        );
        let written = ingest_fact_atomic(pool, &permit, &sourced, None).await?;
        let t = written.memory_id.into_inner();
        let cited_object_id = written
            .cited_object_id
            .expect("citation-bearing write returns its object");

        let cold = MemoryColdStore::default();
        let key = cold_object_key(t);
        let mut tx = pool.begin().await?;
        forget_memory(
            &mut tx,
            &core_pg_sidecars(),
            &surfaces(),
            &cold,
            &key,
            t,
            owner.stored_owner_id(),
        )
        .await?;
        tx.commit().await?;

        let cooled_blob: Option<Uuid> =
            sqlx::query_scalar("SELECT blob_id FROM proxima_core.cooled WHERE t = $1")
                .bind(t)
                .fetch_one(pool)
                .await?;
        assert_eq!(cooled_blob, Some(cited_object_id));

        // The retry need not repeat the citation input: replay reads the
        // original witness from the admission selected by its ingest key.
        let mut replay_input = sourced.clone();
        replay_input.citation = None;
        replay_input.blob_id = None;
        let replay = ingest_fact_atomic(pool, &permit, &replay_input, None)
            .await
            .expect("re-delivery of a cooled admission");
        assert!(replay.idempotent_replay);
        assert_eq!(replay.memory_id, written.memory_id);
        assert_eq!(replay.handle, written.handle, "replay keeps the handle");
        assert_eq!(
            replay.cited_object_id,
            Some(cited_object_id),
            "replay keeps the original cited object"
        );

        let hot: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.memory WHERE ingest_key = 'delivery-cooled'",
        )
        .fetch_one(pool)
        .await?;
        assert_eq!(hot, 0, "a replay mints no new hot row");
        let stub: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.cooled WHERE t = $1")
                .bind(t)
                .fetch_one(pool)
                .await?;
        assert_eq!(stub, 1, "the admission stays cooled");
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("cooled ingest-key replay test failed");
}

#[tokio::test]
async fn forget_pinless_abstraction_is_refused() {
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
        ingest_fact_atomic(pool, &permit, &draft(None), None).await?;
        let owner_id = owner.stored_owner_id();
        let handle = Uuid::now_v7();
        let t = Uuid::now_v7();
        let content_id: Uuid = sqlx::query_scalar(
            "INSERT INTO proxima_core.content (owner_id, schema_id, content_hash)
             VALUES ($1, 'core/test-abs-v1', $2)
             RETURNING content_id",
        )
        .bind(owner_id)
        .bind(vec![0_u8; 32])
        .fetch_one(pool)
        .await?;
        sqlx::query(
            "INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t)
             VALUES ($1, 'abstraction', 'core/test-abs-v1', $2, $3)",
        )
        .bind(handle)
        .bind(owner_id)
        .bind(t)
        .execute(pool)
        .await?;
        let err = sqlx::query(
            "INSERT INTO proxima_core.memory
                (handle, t, kind, owner_id, schema_id, content_id, origins, refs)
             VALUES ($1, $2, 'abstraction', $3, 'core/test-abs-v1', $4, '{}', '{}')",
        )
        .bind(handle)
        .bind(t)
        .bind(owner_id)
        .bind(content_id)
        .execute(pool)
        .await
        .expect_err("pinless A");
        assert!(
            err.to_string().contains("cooled fact") || err.to_string().contains("23514"),
            "got: {err}"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("pinless abstraction test failed");
}

#[tokio::test]
async fn concurrent_forget_keeps_one_grounding_support() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let p1 = OwnerWritePermit::new_for_tests(
            OwnerRef::Personal(UserId::new(Uuid::now_v7())),
            AccessKind::Fact,
        );
        let p2 = OwnerWritePermit::new_for_tests(
            OwnerRef::Personal(UserId::new(Uuid::now_v7())),
            AccessKind::Fact,
        );
        let p3 = OwnerWritePermit::new_for_tests(
            OwnerRef::Personal(UserId::new(Uuid::now_v7())),
            AccessKind::Fact,
        );
        let pool = pg.pool_for_tests().clone();
        let f1 = ingest_fact_atomic(&pool, &p1, &draft(None), None).await?;
        let f2 = ingest_fact_atomic(&pool, &p2, &draft(None), None).await?;
        ingest_fact_atomic(&pool, &p3, &draft(None), None).await?;
        let a1 = ingest_fact_atomic(
            &pool,
            &p1,
            &derived_abstraction(EntityKind::Fact, f1.memory_id.into_inner()),
            None,
        )
        .await?;
        let a2 = ingest_fact_atomic(
            &pool,
            &p2,
            &derived_abstraction(EntityKind::Fact, f2.memory_id.into_inner()),
            None,
        )
        .await?;
        let mut both = draft(None);
        both.kind = "abstraction".into();
        both.derived_from = vec![
            EdgeEndpoint::memory(EntityKind::Abstraction, a1.memory_id),
            EdgeEndpoint::memory(EntityKind::Abstraction, a2.memory_id),
        ];
        let dep = ingest_fact_atomic(&pool, &p3, &both, None).await?;

        let a1_t = a1.memory_id.into_inner();
        let a2_t = a2.memory_id.into_inner();
        let dep_t = dep.memory_id.into_inner();
        let mut gate = pool.begin().await?;
        let _: Uuid =
            sqlx::query_scalar("SELECT t FROM proxima_core.memory WHERE t = $1 FOR UPDATE")
                .bind(dep_t)
                .fetch_one(&mut *gate)
                .await?;

        let pool_a = pool.clone();
        let pool_b = pool.clone();
        let a1_owner = p1.owner().stored_owner_id();
        let a2_owner = p2.owner().stored_owner_id();
        let f1 = tokio::spawn(async move {
            let mut tx = pool_a.begin().await.map_err(|err| err.to_string())?;
            let r = forget_memory(
                &mut tx,
                &core_pg_sidecars(),
                &surfaces(),
                &MemoryColdStore::default(),
                "cold/conc-a1",
                a1_t,
                a1_owner,
            )
            .await;
            match &r {
                Ok(()) => tx.commit().await.map_err(|err| err.to_string())?,
                Err(_) => tx.rollback().await.map_err(|err| err.to_string())?,
            }
            Ok::<_, String>(r)
        });
        let f2 = tokio::spawn(async move {
            let mut tx = pool_b.begin().await.map_err(|err| err.to_string())?;
            let r = forget_memory(
                &mut tx,
                &core_pg_sidecars(),
                &surfaces(),
                &MemoryColdStore::default(),
                "cold/conc-a2",
                a2_t,
                a2_owner,
            )
            .await;
            match &r {
                Ok(()) => tx.commit().await.map_err(|err| err.to_string())?,
                Err(_) => tx.rollback().await.map_err(|err| err.to_string())?,
            }
            Ok::<_, String>(r)
        });

        for _ in 0..50 {
            if f1.is_finished() || f2.is_finished() {
                break;
            }
            let waiting: i64 =
                sqlx::query_scalar("SELECT count(*)::bigint FROM pg_locks WHERE NOT granted")
                    .fetch_one(&pool)
                    .await?;
            if waiting >= 2 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(
            !f1.is_finished() && !f2.is_finished(),
            "both forgets must wait on the depender FOR UPDATE"
        );
        gate.rollback().await?;

        let r1 = f1.await.expect("join a1").expect("tx a1");
        let r2 = f2.await.expect("join a2").expect("tx a2");
        let ok_count = usize::from(r1.is_ok()) + usize::from(r2.is_ok());
        assert_eq!(
            ok_count, 1,
            "exactly one of A1/A2 forgets may commit; {r1:?} {r2:?}"
        );
        let remaining: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint
               FROM unnest(
                    (SELECT origins || refs FROM proxima_core.memory WHERE t = $1)
               ) AS p(id)
              WHERE EXISTS (SELECT 1 FROM proxima_core.memory h WHERE h.t = p.id)
                 OR EXISTS (
                        SELECT 1 FROM proxima_core.cooled c
                         WHERE c.t = p.id AND c.kind = 'fact'
                    )",
        )
        .bind(dep.memory_id.into_inner())
        .fetch_one(&pool)
        .await?;
        assert!(
            remaining > 0,
            "dependent must keep a hot pin or cooled Fact"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("concurrent forget grounding test failed");
}

#[tokio::test]
async fn forget_blocks_admit_until_grounding_rechecked() {
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
        let fact = ingest_fact_atomic(&pool, &permit, &draft(None), None).await?;
        let abs = ingest_fact_atomic(
            &pool,
            &permit,
            &derived_abstraction(EntityKind::Fact, fact.memory_id.into_inner()),
            None,
        )
        .await?;
        let a_t = abs.memory_id.into_inner();

        let mut tx_f = pool.begin().await?;
        let _: Uuid = sqlx::query_scalar("SELECT t FROM proxima_core.memory WHERE t = $1 FOR UPDATE")
            .bind(a_t)
            .fetch_one(&mut *tx_f)
            .await?;

        let owner_id = owner.stored_owner_id();
        let handle = Uuid::now_v7();
        let new_t = Uuid::now_v7();
        let content_id: Uuid = sqlx::query_scalar(
            "INSERT INTO proxima_core.content (owner_id, schema_id, content_hash)
             VALUES ($1, 'core/test-abs-v1', $2)
             RETURNING content_id",
        )
        .bind(owner_id)
        .bind(vec![1_u8; 32])
        .fetch_one(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t)
             VALUES ($1, 'abstraction', 'core/test-abs-v1', $2, $3)",
        )
        .bind(handle)
        .bind(owner_id)
        .bind(new_t)
        .execute(&pool)
        .await?;

        let pool_admit = pool.clone();
        let admit = tokio::spawn(async move {
            sqlx::query(
                "INSERT INTO proxima_core.memory
                    (handle, t, kind, owner_id, schema_id, content_id, origins, refs)
                 VALUES ($1, $2, 'abstraction', $3, 'core/test-abs-v1', $4, ARRAY[$5]::uuid[], '{}')",
            )
            .bind(handle)
            .bind(new_t)
            .bind(owner_id)
            .bind(content_id)
            .bind(a_t)
            .execute(&pool_admit)
            .await
        });

        for _ in 0..50 {
            let waiting: i64 =
                sqlx::query_scalar("SELECT count(*)::bigint FROM pg_locks WHERE NOT granted")
                    .fetch_one(&pool)
                    .await?;
            if waiting > 0 || admit.is_finished() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(
            !admit.is_finished(),
            "admit must wait on FOR SHARE, not pass B2 against the still-hot A"
        );

        forget_memory(
            &mut tx_f,
            &core_pg_sidecars(),
            &surfaces(),
            &MemoryColdStore::default(),
            "cold/admit-race",
            a_t,
            owner_id,
        )
        .await?;
        tx_f.commit().await?;

        let err = admit.await?.expect_err("admit after forget of sole A origin");
        assert!(
            err.to_string().contains("cooled fact") || err.to_string().contains("23514"),
            "got: {err}"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("admit-vs-forget overlap test failed");
}

struct FailDeleteCold;

#[async_trait::async_trait]
impl ColdObjectStore for FailDeleteCold {
    async fn put(&self, _key: &str, _bytes: &[u8]) -> Result<(), StorageError> {
        Ok(())
    }

    async fn get(&self, _key: &str) -> Result<Vec<u8>, StorageError> {
        Err(StorageError::NotFound)
    }

    async fn delete(&self, _key: &str) -> Result<(), StorageError> {
        Err(StorageError::Internal("cold delete refused".into()))
    }
}

#[tokio::test]
async fn commit_forget_aborts_when_owner_transferred() {
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
        let written = ingest_fact_atomic(pool, &permit, &draft(None), None).await?;
        let t = written.memory_id.into_inner();
        let mut conn = pool.acquire().await?;
        let snapshot = snapshot_hot(&mut conn, &core_pg_sidecars(), t).await?;
        drop(conn);
        let dest = OwnerRef::Group(GroupId::new(Uuid::now_v7()));
        assert!(
            pg.transfer_to_owner(
                &permit,
                EntityId::Memory(written.memory_id),
                dest,
                &transfer_surfaces()
            )
            .await?
        );

        let cold = MemoryColdStore::default();
        let key = cold_object_key(t);
        let mut tx = pool.begin().await?;
        let err = commit_forget(
            &mut tx,
            &core_pg_sidecars(),
            &surfaces(),
            &cold,
            &key,
            &snapshot,
            owner.stored_owner_id(),
        )
        .await
        .expect_err("forget after a transfer must not cool the new owner");
        assert!(matches!(err, StorageError::NotFound), "got {err:?}");
        tx.rollback().await?;

        let moved: Uuid =
            sqlx::query_scalar("SELECT owner_id FROM proxima_core.memory WHERE t = $1")
                .bind(t)
                .fetch_one(pool)
                .await?;
        assert_eq!(moved, dest.stored_owner_id());
        let cooled: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.cooled WHERE t = $1")
                .bind(t)
                .fetch_one(pool)
                .await?;
        assert_eq!(cooled, 0);
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("forget-after-transfer must abort");
}

/// An erase that destroyed its cold object inside the caller's transaction lost
/// the object outright whenever that transaction later rolled back: the `cooled`
/// locator came back and the bytes did not. The destruction is deferred to after
/// the commit, and a rolled-back erase must leave locator and object together.
#[tokio::test]
async fn a_rolled_back_erase_keeps_the_cold_object_and_its_locator() {
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
        let written = ingest_fact_atomic(pool, &permit, &draft(None), None).await?;
        let t = written.memory_id.into_inner();
        let key = cold_object_key(t);
        let cold = MemoryColdStore::default();
        let mut tx = pool.begin().await?;
        forget_memory(
            &mut tx,
            &core_pg_sidecars(),
            &surfaces(),
            &cold,
            &key,
            t,
            owner.stored_owner_id(),
        )
        .await?;
        tx.commit().await?;

        let mut tx = pool.begin().await?;
        let plan = erase_memory(&mut tx, &core_pg_sidecars(), &surfaces(), &owner, t).await?;
        assert_eq!(plan.object_keys(), std::slice::from_ref(&key));
        tx.rollback().await?;

        assert!(
            cold.get(&key).await.is_ok(),
            "nothing may destroy the object before the erase commits"
        );
        let stub: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.cooled WHERE t = $1")
                .bind(t)
                .fetch_one(pool)
                .await?;
        assert_eq!(stub, 1, "the rolled-back erase restores the cooled locator");
        let pending: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.cold_purge_pending WHERE object_key = $1",
        )
        .bind(&key)
        .fetch_one(pool)
        .await?;
        assert_eq!(
            pending, 0,
            "the pending mark rolls back with the erase that made it"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("rolled-back erase consistency test failed");
}

/// The erase is already committed by the time the object store is asked, so a
/// refusing store cannot undo it. What it must not do is lose the debt: the
/// key stays in `cold_purge_pending` as the durable record a retry reads.
#[tokio::test]
async fn a_refusing_cold_store_leaves_the_purge_mark_for_retry() {
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
        let written = ingest_fact_atomic(pool, &permit, &draft(None), None).await?;
        let t = written.memory_id.into_inner();
        let key = cold_object_key(t);
        let ok_cold = MemoryColdStore::default();
        let mut tx = pool.begin().await?;
        forget_memory(
            &mut tx,
            &core_pg_sidecars(),
            &surfaces(),
            &ok_cold,
            &key,
            t,
            owner.stored_owner_id(),
        )
        .await?;
        tx.commit().await?;

        let mut tx = pool.begin().await?;
        let plan = erase_memory(&mut tx, &core_pg_sidecars(), &surfaces(), &owner, t).await?;
        tx.commit().await?;

        let purged = purge_cold_objects_after_commit(pool, &FailDeleteCold, &plan).await;
        assert_eq!(purged.purged, 0, "a refusing object store destroys nothing");
        assert!(purged.pending);
        let pending: Vec<String> = sqlx::query_scalar(
            "SELECT object_key FROM proxima_core.cold_purge_pending WHERE owner_id = $1",
        )
        .bind(owner.stored_owner_id())
        .fetch_all(pool)
        .await?;
        assert_eq!(
            pending,
            vec![key],
            "the object outlived its erase, so the queue keeps the debt"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("erase fail-closed test failed");
}

/// The two corrected `ForgetRule` declarations, pinned as BEHAVIOUR.
///
/// `ingest_keys` declared `DeleteWithMemory` and `memory_head` declared it
/// too; neither was true, and neither could have been found by reading,
/// because nothing consumed the field. Phase 4 corrects both to
/// `Keep { why }` — and a correction that only changes a string is worth
/// nothing, so this asserts the shipped behaviour the new declarations
/// claim, in both directions:
///
/// - a cool leaves the receipt and REWINDS the head to the surviving
///   newest `t`;
/// - an erase of the last version removes the receipt and takes the head
///   with it.
///
/// Restoring either declaration to `DeleteWithMemory` after Phase 4's
/// forget leg reads the contract makes this fail, which is the point: the
/// declaration is now falsifiable.
#[tokio::test]
async fn cooling_keeps_the_receipt_and_rewinds_the_head_while_erase_takes_both() {
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
        let cold = MemoryColdStore::default();

        // Two versions on one handle, each with its own receipt.
        let first = ingest_fact_atomic(&pool, &permit, &draft(Some(("src", "k1"))), None).await?;
        let mut later = draft(Some(("src", "k2")));
        later.handle = Some(first.handle);
        let second = ingest_fact_atomic(&pool, &permit, &later, None).await?;
        let (t1, t2) = (first.memory_id.into_inner(), second.memory_id.into_inner());

        let receipts = |t: Uuid| {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, i64>(
                    "SELECT count(*)::bigint FROM proxima_core.ingest_keys WHERE t = $1",
                )
                .bind(t)
                .fetch_one(&pool)
                .await
            }
        };
        let head_t = || {
            let pool = pool.clone();
            let handle = first.handle;
            async move {
                sqlx::query_scalar::<_, Option<Uuid>>(
                    "SELECT t FROM proxima_core.memory_head WHERE handle = $1",
                )
                .bind(handle)
                .fetch_optional(&pool)
                .await
                .map(Option::flatten)
            }
        };

        assert_eq!(
            head_t().await?,
            Some(t2),
            "the head names the newest version"
        );

        // ── `ForgetRule::Keep` on ingest_keys: cooling the NEWEST version
        // leaves its receipt behind. ────────────────────────────────────
        forget_memory_oneshot(
            &pool,
            &core_pg_sidecars(),
            &surfaces(),
            &cold,
            &cold_object_key(t2),
            t2,
            owner.stored_owner_id(),
        )
        .await?;
        assert_eq!(
            receipts(t2).await?,
            1,
            "cooling a version does not un-admit it: `ingest_keys` stay, exactly as \
             core_forget's own wire description says"
        );

        // ── `ForgetRule::Keep` on memory_head: the head REWINDS rather
        // than being deleted with the memory. ───────────────────────────
        assert_eq!(
            head_t().await?,
            Some(t1),
            "the head is rewound to the surviving newest t, not deleted — which is why \
             `DeleteWithMemory` was true for exactly one revision of a series"
        );

        // ── Erase is the verb that takes them. ──────────────────────────
        let mut tx = pool.begin().await?;
        erase_memory(&mut tx, &core_pg_sidecars(), &surfaces(), &owner, t1).await?;
        tx.commit().await?;
        assert_eq!(
            receipts(t1).await?,
            0,
            "erase_memory is the only statement that removes a receipt"
        );
        assert_eq!(
            receipts(t2).await?,
            1,
            "erasing one version does not touch another's receipt"
        );
        assert_eq!(
            head_t().await?,
            None,
            "the head is deleted only when the hot series empties"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("the corrected forget declarations must match shipped behaviour");
}
