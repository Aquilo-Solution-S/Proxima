//! #488 S10: forget marks the row. The memory row and its sidecar rows stay in
//! Postgres with `forgotten_at` set; the search projection and embeddings go;
//! no object store is called.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use proxima_core::storage_ports::{FactIngestPort, MemoryAuthoringPort, OwnerWritePermit};
use proxima_core::{AccessKind, ColdObjectStore, OwnerId, StorageError, UserId};
use uuid::Uuid;

use crate::test_fixtures::fresh_pg;
use crate::verbs::forget::forget_pg_tests::{draft, ingest_stamped};

const AGENT_NOTE: &str = "proxima_core.agent_note_v1";

/// Counts every call and refuses it: forget must not reach an object store.
#[derive(Default)]
struct CountingCold {
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl ColdObjectStore for CountingCold {
    async fn delete(&self, _key: &str) -> Result<proxima_core::PurgeDeleted, StorageError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(StorageError::Internal("forget called delete".into()))
    }

    fn store_id(&self) -> &proxima_core::ObjectStoreId {
        static ID: std::sync::LazyLock<proxima_core::ObjectStoreId> =
            std::sync::LazyLock::new(|| {
                proxima_core::ObjectStoreId::parse("s3:test/bucket").expect("test store id")
            });
        &ID
    }
}

fn unit_vector() -> pgvector::Vector {
    let mut values = vec![0.0; 1024];
    values[0] = 1.0;
    pgvector::Vector::from(values)
}

/// `(memory rows, forgotten memory rows, agent notes)` for `t`, read as the
/// table owner, past every policy.
async fn stored(pool: &sqlx::PgPool, t: Uuid) -> (i64, i64, i64) {
    sqlx::query_as(
        "SELECT (SELECT count(*) FROM proxima_core.memory WHERE t = $1),
                (SELECT count(*) FROM proxima_core.memory WHERE t = $1 AND forgotten_at IS NOT NULL),
                (SELECT count(*) FROM proxima_core.agent_note_v1 WHERE t = $1)",
    )
    .bind(t)
    .fetch_one(pool)
    .await
    .expect("stored")
}

async fn note_fact(pg: &crate::PgStorage, permit: &OwnerWritePermit, body_bytes: usize) -> Uuid {
    let insert = format!(
        "INSERT INTO proxima_core.agent_note_v1 (t, note_id, title, body, tags)
         VALUES ($1, $1, 'n', repeat('x', {body_bytes}), ARRAY['tag'])"
    );
    ingest_stamped(
        pg.pool_for_tests(),
        permit,
        &draft(None),
        &[AGENT_NOTE.to_owned()],
        &[insert.as_str()],
    )
    .await
    .expect("note fact")
    .memory_id
    .into_inner()
}

#[tokio::test]
async fn a_fact_with_a_sidecar_row_over_64_kib_forgets() {
    let (pg, _db) = fresh_pg("forget_64k").await;
    let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
    let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
    let t = note_fact(&pg, &permit, 70 * 1024).await;

    MemoryAuthoringPort::forget_memory(&pg, &permit, proxima_core::MemoryId::new(t))
        .await
        .expect("a 70 KiB sidecar row forgets");

    assert_eq!(
        stored(pg.pool_for_tests(), t).await,
        (1, 1, 1),
        "the row and its sidecar stay, marked forgotten"
    );
}

#[tokio::test]
async fn forget_makes_no_object_store_call() {
    let (pg, _db) = fresh_pg("forget_no_cold").await;
    let cold = Arc::new(CountingCold::default());
    let pg = pg.with_cold(cold.clone());
    let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
    let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
    let t = note_fact(&pg, &permit, 16).await;

    MemoryAuthoringPort::forget_memory(&pg, &permit, proxima_core::MemoryId::new(t))
        .await
        .expect("forget succeeds without an object store");

    assert_eq!(cold.calls.load(Ordering::SeqCst), 0, "no object-store call");
    assert_eq!(stored(pg.pool_for_tests(), t).await, (1, 1, 1));
}

#[tokio::test]
async fn forget_deletes_the_embeddings_and_keeps_the_row() {
    let (pg, _db) = fresh_pg("forget_embeddings").await;
    let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
    let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
    let pool = pg.pool_for_tests().clone();
    let t = pg
        .ingest_fact_atomic(&permit, &draft(None), None)
        .await
        .expect("fact")
        .memory_id
        .into_inner();
    sqlx::query("DELETE FROM proxima_core.embedding_jobs WHERE entity_id = $1")
        .bind(t)
        .execute(&pool)
        .await
        .expect("clear jobs");
    sqlx::query(
        "INSERT INTO proxima_core.embeddings
            (entity_id, model_id, dim, embedding_version, chunk_ordinal, vec, owner_id)
         VALUES ($1, 'test-embed', 1024, 1, 0, $3, $2)",
    )
    .bind(t)
    .bind(owner.into_inner())
    .bind(unit_vector())
    .execute(&pool)
    .await
    .expect("embedding");
    sqlx::query(
        "INSERT INTO proxima_core.embedding_heads
            (entity_id, model_id, dim, embedding_version, owner_id)
         VALUES ($1, 'test-embed', 1024, 1, $2)",
    )
    .bind(t)
    .bind(owner.into_inner())
    .execute(&pool)
    .await
    .expect("head");

    MemoryAuthoringPort::forget_memory(&pg, &permit, proxima_core::MemoryId::new(t))
        .await
        .expect("forget");

    let (vectors, heads): (i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM proxima_core.embeddings WHERE entity_id = $1),
                (SELECT count(*) FROM proxima_core.embedding_heads WHERE entity_id = $1)",
    )
    .bind(t)
    .fetch_one(&pool)
    .await
    .expect("count");
    assert_eq!(
        (vectors, heads),
        (0, 0),
        "semantic search has nothing to find"
    );
    let (rows, forgotten, _) = stored(&pool, t).await;
    assert_eq!((rows, forgotten), (1, 1), "the row stays, marked forgotten");
}

#[tokio::test]
async fn forgetting_the_series_head_makes_the_previous_version_head() {
    let (pg, _db) = fresh_pg("forget_head").await;
    let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
    let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
    let first = pg
        .ingest_fact_atomic(&permit, &draft(None), None)
        .await
        .expect("first");
    let mut later = draft(None);
    later.handle = Some(first.handle);
    let second = pg
        .ingest_fact_atomic(&permit, &later, None)
        .await
        .expect("second");

    MemoryAuthoringPort::forget_memory(&pg, &permit, second.memory_id)
        .await
        .expect("forget head");

    let head: Uuid = sqlx::query_scalar("SELECT t FROM proxima_core.memory_head WHERE handle = $1")
        .bind(first.handle)
        .fetch_one(pg.pool_for_tests())
        .await
        .expect("head");
    assert_eq!(head, first.memory_id.into_inner());
    let (rows, forgotten, _) = stored(pg.pool_for_tests(), second.memory_id.into_inner()).await;
    assert_eq!(
        (rows, forgotten),
        (1, 1),
        "the forgotten head stays as a row"
    );
}
