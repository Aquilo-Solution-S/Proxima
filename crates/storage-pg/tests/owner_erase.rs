//! Owner erase against the complete core baseline.
#![allow(clippy::doc_markdown, clippy::too_many_lines)]

use std::sync::Arc;
use std::time::Duration;

use proxima_core::owner_inverse::{
    EraseAuthorization, OwnerEraseOutcome, OwnerEraseRefusal, OwnerEraseTarget, OwnerSurfaces,
};
use proxima_core::storage_ports::FactIngestPort;
use proxima_core::storage_ports::{
    MemoryAuthoringPort, OwnerInversePort, OwnerMembershipAdminPort, OwnerTransferPort,
    OwnerWritePermit,
};
use proxima_core::verbs::fact_ingest::FactWriteCommand;
use proxima_core::verbs::goal_write::GoalState;
use proxima_core::{
    AccessKind, ColdObjectStore, EntityId, GroupId, OwnerId, SchemaId, SchemaVersion, SourceId,
    StorageError, UserId,
};
use proxima_pg_testkit::{db_url, drop_db};
use proxima_storage_pg::test_fixtures::{create_core_db, register_group_for_tests};
use proxima_storage_pg::verbs::forget::MemoryColdStore;
use proxima_storage_pg::verbs::goal_timeseries::{GoalWriteCommand, write_goal};
use proxima_storage_pg::{ColdPurgeRetryOptions, PgStorage};
use sqlx::{PgPool, Postgres};
use uuid::Uuid;

/// The five sidecar legs exactly as the engine assembles them: from the
/// frozen flavor registry. Passing empty slices here would silently skip
/// the owner-pinned leg, which is the difference these tests exist to
/// measure.
fn contract_sidecar_tables() -> OwnerSurfaces {
    OwnerSurfaces::for_registry(&proxima_core::FlavorRegistry::new().freeze_or_panic_for_tests())
}

/// Outstanding cold-object debts. The queue is the debt: a row means the
/// object still exists and the erase that promised to reclaim it has not.
async fn pending_debts(pool: &sqlx::PgPool) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.cold_purge_pending")
        .fetch_one(pool)
        .await
}

const CITED_TABLE: &str = "proxima_core.test_cited_object_v1";
const SPAN_TABLE: &str = "proxima_core.test_cited_span_v1";

#[derive(Default)]
struct RefusingDeleteCold;

#[async_trait::async_trait]
impl ColdObjectStore for RefusingDeleteCold {
    fn store_id(&self) -> &proxima_core::ObjectStoreId {
        static ID: std::sync::LazyLock<proxima_core::ObjectStoreId> =
            std::sync::LazyLock::new(|| {
                proxima_core::ObjectStoreId::parse("s3:test/bucket").expect("test store id")
            });
        &ID
    }

    async fn delete(&self, _key: &str) -> Result<proxima_core::PurgeDeleted, StorageError> {
        Err(StorageError::Unavailable("delete refused".into()))
    }
}

/// A cold store that names the store it writes to, which is what the
/// durable purge queue matches a debt against.
#[derive(Debug)]
struct BucketNamedCold {
    objects: std::sync::Mutex<std::collections::BTreeMap<String, Vec<u8>>>,
    store: proxima_core::ObjectStoreId,
}

impl BucketNamedCold {
    /// The store `s3:test/<bucket>`, the identity the fixtures below record.
    fn new(bucket: &str) -> Self {
        Self {
            objects: std::sync::Mutex::default(),
            store: proxima_core::ObjectStoreId::parse(&format!("s3:test/{bucket}"))
                .expect("test store id"),
        }
    }

    /// Seed bytes as an upload would have left them in the bucket. The purge
    /// store itself only deletes (#488).
    fn put(&self, key: &str, bytes: &[u8]) {
        self.objects
            .lock()
            .expect("objects")
            .insert(key.to_owned(), bytes.to_vec());
    }

    fn get(&self, key: &str) -> Result<Vec<u8>, StorageError> {
        self.objects
            .lock()
            .expect("objects")
            .get(key)
            .cloned()
            .ok_or(StorageError::NotFound)
    }
}

#[async_trait::async_trait]
impl ColdObjectStore for BucketNamedCold {
    fn store_id(&self) -> &proxima_core::ObjectStoreId {
        &self.store
    }

    async fn delete(&self, key: &str) -> Result<proxima_core::PurgeDeleted, StorageError> {
        let removed = self.objects.lock().expect("objects").remove(key);
        Ok(proxima_core::PurgeDeleted::from_versions(u64::from(
            removed.is_some(),
        )))
    }
}

/// The two synthetic surfaces, declared exactly as a flavor would declare
/// them: keyed on a blob under a column of their own naming, carrying no
/// `owner_id` of their own, and tallying into `sidecar_rows`.
fn citation_surfaces() -> proxima_core::owner_inverse::OwnerSurfaces {
    use proxima_core::flavor::{
        CounterRule, EraseRule, ExportRule, ForgetRule, KeyShape, Surface, TransferRule,
    };
    const fn citation(table: &'static str, column: &'static str) -> Surface {
        Surface {
            table,
            key: KeyShape::BlobId { column },
            owner_column: None,
            transfer: TransferRule::StaysOnKey,
            erase: EraseRule::ByKey,
            export: ExportRule::Rows,
            forget: ForgetRule::Keep {
                why: "a citation outlives the Fact that made it",
            },
            lexical_language_column: None,
            counter: CounterRule::Counted("sidecar_rows"),
            completeness: None,
        }
    }
    proxima_core::owner_inverse::OwnerSurfaces::from_surfaces(vec![
        citation(CITED_TABLE, "cited_object_id"),
        citation(SPAN_TABLE, "span_blob_id"),
    ])
}

/// No core payload registers a citation sidecar, so blob-keyed sidecar coverage
/// is a synthetic registration: erase takes the declared surfaces as an
/// argument, exactly as a flavor's frozen registry supplies them.
async fn create_citation_sidecar_tables(pool: &sqlx::PgPool) -> Result<(), sqlx::Error> {
    sqlx::query(
        "CREATE TABLE proxima_core.test_cited_object_v1 (
             cited_object_id uuid PRIMARY KEY REFERENCES proxima_core.blob (blob_id),
             body text NOT NULL
         )",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE TABLE proxima_core.test_cited_span_v1 (
             span_blob_id uuid PRIMARY KEY REFERENCES proxima_core.blob (blob_id),
             page_from integer NOT NULL
         )",
    )
    .execute(pool)
    .await?;
    sqlx::query("GRANT ALL PRIVILEGES ON TABLE proxima_core.test_cited_object_v1, proxima_core.test_cited_span_v1 TO PUBLIC")
        .execute(pool)
        .await?;
    Ok(())
}

/// One cited blob of `owner`, with an upload record and both citation sidecar
/// rows hanging off it, cited by one Fact in `source`.
async fn cite_blob(
    pg: &PgStorage,
    owner: OwnerId,
    source: Option<(&str, &str)>,
    seed: u8,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let pool = pg.pool_for_tests();
    // `blob.owner_id` references `owners`, which ordinary writes materialize.
    sqlx::query(
        "INSERT INTO proxima_core.owners (owner_id, kind)
         VALUES ($1, 'personal') ON CONFLICT (owner_id) DO NOTHING",
    )
    .bind(owner.into_inner())
    .execute(pool)
    .await?;
    let blob_id: Uuid = sqlx::query_scalar(
        "INSERT INTO proxima_core.blob (owner_id, schema_id, content_hash)
         VALUES ($1, 'core/bytes-v1', $2)
         RETURNING blob_id",
    )
    .bind(owner.into_inner())
    .bind(vec![seed; 32])
    .fetch_one(pool)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.blob_uploads
             (owner_id, bucket, object_store, object_key, filename, mime, expected_byte_len,
              status, blob_id, sha256, expires_at, completed_at)
         VALUES ($1, 'bucket', 's3:test/bucket', $2, 'cited.pdf', 'application/pdf', 1,
                 'completed', $3, $4, now() + interval '1 hour', now())",
    )
    .bind(owner.into_inner())
    .bind(format!("objects/{seed}"))
    .bind(blob_id)
    .bind(vec![seed; 32])
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.test_cited_object_v1 (cited_object_id, body)
         VALUES ($1, 'cited body')",
    )
    .bind(blob_id)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.test_cited_span_v1 (span_blob_id, page_from)
         VALUES ($1, 4)",
    )
    .bind(blob_id)
    .execute(pool)
    .await?;
    let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
    let mut cited = draft(source);
    cited.blob_id = Some(blob_id);
    pg.ingest_fact_atomic(&permit, &cited, None).await?;
    Ok(blob_id)
}

/// A completed upload that no admission cites. Source erasure must not infer
/// ownership of this blob merely because it is currently unreferenced.
async fn completed_blob_upload(
    pool: &sqlx::PgPool,
    owner: OwnerId,
    seed: u8,
) -> Result<(Uuid, String), Box<dyn std::error::Error>> {
    let blob_id: Uuid = sqlx::query_scalar(
        "INSERT INTO proxima_core.blob (owner_id, schema_id, content_hash)
         VALUES ($1, 'core/bytes-v1', $2)
         RETURNING blob_id",
    )
    .bind(owner.into_inner())
    .bind(vec![seed; 32])
    .fetch_one(pool)
    .await?;
    let object_key = format!("objects/{seed}");
    sqlx::query(
        "INSERT INTO proxima_core.blob_uploads
             (owner_id, bucket, object_store, object_key, filename, mime, expected_byte_len,
              status, blob_id, sha256, expires_at, completed_at)
         VALUES ($1, 'bucket', 's3:test/bucket', $2, 'unreferenced.pdf', 'application/pdf', 1,
                 'completed', $3, $4, now() + interval '1 hour', now())",
    )
    .bind(owner.into_inner())
    .bind(&object_key)
    .bind(blob_id)
    .bind(vec![seed; 32])
    .execute(pool)
    .await?;
    Ok((blob_id, object_key))
}

/// Every row keyed on one blob: the blob itself, its upload record, and both
/// citation sidecar rows. `(blob, upload, cited_object, cited_span)`.
async fn rows_for_blob(
    pool: &sqlx::PgPool,
    blob_id: Uuid,
) -> Result<(i64, i64, i64, i64), sqlx::Error> {
    sqlx::query_as(
        "SELECT
             (SELECT count(*)::bigint FROM proxima_core.blob
               WHERE blob_id = $1),
             (SELECT count(*)::bigint FROM proxima_core.blob_uploads
               WHERE blob_id = $1),
             (SELECT count(*)::bigint FROM proxima_core.test_cited_object_v1
               WHERE cited_object_id = $1),
             (SELECT count(*)::bigint FROM proxima_core.test_cited_span_v1
               WHERE span_blob_id = $1)",
    )
    .bind(blob_id)
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
        occurred_at: None,
        citation: None,
        additional_references: Vec::new(),
        refs: Vec::new(),
        blob_id: None,
        kind: proxima_core::MemoryKind::Fact,
        request: None,
    }
}

fn embed_vector() -> pgvector::Vector {
    let mut values = vec![0.0; 1024];
    values[0] = 1.0;
    pgvector::Vector::from(values)
}

fn owner_fence_key(owner: OwnerId) -> String {
    format!("proxima-owner-fence:{}", owner.into_inner())
}

fn source_fence_key(owner: OwnerId, source: &str) -> String {
    format!("proxima-source-fence:{}:{source}", owner.into_inner())
}

async fn wait_for_ungranted_advisory_label(
    pool: &PgPool,
    label: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let waiting: bool = sqlx::query_scalar(
                "WITH lock_key AS (
                     SELECT hashtextextended($1, 0) AS key
                 )
                 SELECT EXISTS (
                     SELECT 1
                       FROM pg_locks l
                       CROSS JOIN lock_key k
                      WHERE l.locktype = 'advisory'
                        AND NOT l.granted
                        AND l.classid::bigint = ((k.key >> 32) & 4294967295)
                        AND l.objid::bigint = (k.key & 4294967295)
                 )",
            )
            .bind(label)
            .fetch_one(pool)
            .await?;
            if waiting {
                return Ok::<(), sqlx::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await??;
    Ok(())
}

async fn wait_for_ungranted_advisory_key(
    pool: &PgPool,
    key: i64,
) -> Result<(), Box<dyn std::error::Error>> {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let waiting: bool = sqlx::query_scalar(
                "WITH lock_key AS (
                     SELECT $1::bigint AS key
                 )
                 SELECT EXISTS (
                     SELECT 1
                       FROM pg_locks l
                       CROSS JOIN lock_key k
                      WHERE l.locktype = 'advisory'
                        AND NOT l.granted
                        AND l.classid::bigint = ((k.key >> 32) & 4294967295)
                        AND l.objid::bigint = (k.key & 4294967295)
                 )",
            )
            .bind(key)
            .fetch_one(pool)
            .await?;
            if waiting {
                return Ok::<(), sqlx::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await??;
    Ok(())
}

async fn hold_session_advisory_lock(
    pool: &PgPool,
    key: i64,
) -> Result<sqlx::pool::PoolConnection<Postgres>, sqlx::Error> {
    let mut connection = pool.acquire().await?;
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(key)
        .execute(&mut *connection)
        .await?;
    Ok(connection)
}

async fn release_session_advisory_lock(
    connection: &mut sqlx::pool::PoolConnection<Postgres>,
    key: i64,
) -> Result<(), sqlx::Error> {
    let unlocked: bool = sqlx::query_scalar("SELECT pg_advisory_unlock($1)")
        .bind(key)
        .fetch_one(&mut **connection)
        .await?;
    assert!(unlocked, "the test must release its session advisory lock");
    Ok(())
}

async fn fresh_owner_erase_pg() -> (String, PgStorage) {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let pg = PgStorage::connect_for_tests(db_url(&db_name))
        .await
        .expect("connect");
    (db_name, pg)
}

async fn install_memory_insert_gate(pool: &PgPool, key: i64) -> Result<(), sqlx::Error> {
    let sql = format!(
        "CREATE OR REPLACE FUNCTION public.test_owner_erase_memory_insert_gate()
         RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
             PERFORM pg_advisory_xact_lock({key});
             RETURN NEW;
         END
         $$;
         DROP TRIGGER IF EXISTS test_owner_erase_memory_insert_gate
             ON proxima_core.memory;
         CREATE TRIGGER test_owner_erase_memory_insert_gate
         AFTER INSERT ON proxima_core.memory
         FOR EACH ROW EXECUTE FUNCTION public.test_owner_erase_memory_insert_gate();"
    );
    sqlx::raw_sql(sqlx::AssertSqlSafe(sql))
        .execute(pool)
        .await?;
    Ok(())
}

async fn install_goal_insert_gate(pool: &PgPool, key: i64) -> Result<(), sqlx::Error> {
    let sql = format!(
        "CREATE OR REPLACE FUNCTION public.test_owner_erase_goal_insert_gate()
         RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
             PERFORM pg_advisory_xact_lock({key});
             RETURN NEW;
         END
         $$;
         DROP TRIGGER IF EXISTS test_owner_erase_goal_insert_gate
             ON proxima_core.goal;
         CREATE TRIGGER test_owner_erase_goal_insert_gate
         AFTER INSERT ON proxima_core.goal
         FOR EACH ROW EXECUTE FUNCTION public.test_owner_erase_goal_insert_gate();"
    );
    sqlx::raw_sql(sqlx::AssertSqlSafe(sql))
        .execute(pool)
        .await?;
    Ok(())
}

async fn install_transfer_update_gate(pool: &PgPool, key: i64) -> Result<(), sqlx::Error> {
    let sql = format!(
        "CREATE OR REPLACE FUNCTION public.test_owner_erase_transfer_gate()
         RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
             PERFORM pg_advisory_xact_lock({key});
             RETURN NEW;
         END
         $$;
         DROP TRIGGER IF EXISTS test_owner_erase_transfer_gate
             ON proxima_core.memory;
         CREATE TRIGGER test_owner_erase_transfer_gate
         BEFORE UPDATE OF owner_id ON proxima_core.memory
         FOR EACH ROW
         WHEN (OLD.owner_id IS DISTINCT FROM NEW.owner_id)
         EXECUTE FUNCTION public.test_owner_erase_transfer_gate();"
    );
    sqlx::raw_sql(sqlx::AssertSqlSafe(sql))
        .execute(pool)
        .await?;
    Ok(())
}

fn goal_draft(request_id: &str) -> GoalWriteCommand {
    GoalWriteCommand {
        handle: None,
        schema_id: "core/task-goal-v1".into(),
        title: "late goal".into(),
        text: None,
        state: GoalState::Active,
        request_id: request_id.into(),
        close_fact_t: None,
        assignment_t: None,
        dependency_t: vec![],
        evidence_t: vec![],
        mint_write_act: true,
        write_act_t: None,
        parent_handle: None,
    }
}

#[tokio::test]
async fn erase_personal_owner_drops_memory_keys_and_embeddings() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        let pool = pg.pool_for_tests();

        let user = UserId::new(Uuid::now_v7());
        let owner = OwnerId::from(user);
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let written = pg
            .ingest_fact_atomic(&permit, &draft(Some(("src", "k1"))), None)
            .await?;
        let t = written.memory_id.into_inner();
        sqlx::query(
            "INSERT INTO proxima_core.embeddings
                (entity_id, model_id, dim, embedding_version, chunk_ordinal, vec, owner_id)
             SELECT $1, 'test-embed', 1024, version, chunk_ordinal, $2, $3
               FROM generate_series(1, 2) AS version
               CROSS JOIN generate_series(0, 2) AS chunk_ordinal",
        )
        .bind(t)
        .bind(embed_vector())
        .bind(owner.into_inner())
        .execute(pool)
        .await?;
        sqlx::query(
            "INSERT INTO proxima_core.embedding_heads
                (entity_id, model_id, dim, embedding_version, owner_id)
             VALUES ($1, 'test-embed', 1024, 2, $2)",
        )
        .bind(t)
        .bind(owner.into_inner())
        .execute(pool)
        .await?;

        let other = OwnerId::from(UserId::new(Uuid::now_v7()));
        let other_permit = OwnerWritePermit::new_for_tests(other, AccessKind::Fact);
        let other_written = pg
            .ingest_fact_atomic(&other_permit, &draft(Some(("src", "k-other"))), None)
            .await?;

        let auth = EraseAuthorization::new_for_tests(OwnerEraseTarget::PersonalOwner {
            user_id: user,
            drop_event_id: "test-drop".into(),
        });
        let outcome = pg
            .erase_personal_owner(&auth, user, &contract_sidecar_tables())
            .await?;
        let OwnerEraseOutcome::Completed { counts, .. } = outcome else {
            panic!("expected completed erase, got {outcome:?}");
        };
        assert_eq!(counts.get("memories"), 1);
        assert_eq!(
            counts.get("embeddings"),
            7,
            "erase removes both versions' three chunks and their head"
        );
        // `ingest_keys` declares `counter: CounterRule::Counted("receipts")`, and the
        // generated leg tallies whatever its surface declares. The
        // hand-written leg it replaced deleted the same row and counted it
        // nowhere, so `receipts` was structurally zero: a field on the
        // outcome, a column in the journal, and never once a number.
        assert_eq!(
            counts.get("receipts"),
            1,
            "the erased admission's ingest key"
        );

        // The receipt is COMPLETE: exactly the counters the frozen contracts
        // declare, no more and no fewer. A counter that is tallied and never
        // read back, or one reporting a structural zero for a thing this
        // version does not have, are both failures.
        let declared = contract_sidecar_tables().counters();
        let reported: Vec<&str> = counts.iter().map(|(name, _)| name).collect();
        assert_eq!(
            reported, declared,
            "the receipt is the declared counter set"
        );
        assert!(
            declared.contains(&"sketches"),
            "the sketch surface declares a counter, so the receipt carries it"
        );

        let remaining: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.memory WHERE t = $1")
                .bind(t)
                .fetch_one(pool)
                .await?;
        assert_eq!(remaining, 0);
        let witness: Option<String> = sqlx::query_scalar(
            "SELECT kind::text FROM proxima_core.erased_pin_target WHERE t = $1",
        )
        .bind(t)
        .fetch_optional(pool)
        .await?;
        assert_eq!(
            witness.as_deref(),
            Some("fact"),
            "owner erase hard-deletes a hot Memory and records its kind witness"
        );
        let keys: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.ingest_keys WHERE ingest_key = 'k1'",
        )
        .fetch_one(pool)
        .await?;
        assert_eq!(keys, 0);
        let embeddings: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.embeddings WHERE entity_id = $1",
        )
        .bind(t)
        .fetch_one(pool)
        .await?;
        assert_eq!(embeddings, 0);
        let erased_sketches: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.sketch WHERE t = $1")
                .bind(t)
                .fetch_one(pool)
                .await?;
        assert_eq!(
            erased_sketches, 0,
            "owner erase must delete target sketches"
        );
        let other_sketches: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.sketch WHERE t = $1")
                .bind(other_written.memory_id.into_inner())
                .fetch_one(pool)
                .await?;
        assert_eq!(
            other_sketches, 1,
            "owner erase must keep other-owner sketches"
        );

        let other_remaining: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.memory WHERE t = $1")
                .bind(other_written.memory_id.into_inner())
                .fetch_one(pool)
                .await?;
        assert_eq!(other_remaining, 1);
        let other_keys: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.ingest_keys WHERE ingest_key = 'k-other'",
        )
        .fetch_one(pool)
        .await?;
        assert_eq!(other_keys, 1);
        let erased_heads: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.memory_head WHERE handle = $1",
        )
        .bind(written.handle)
        .fetch_one(pool)
        .await?;
        assert_eq!(erased_heads, 0, "owner erase deletes empty heads");
        let other_heads: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.memory_head WHERE handle = $1",
        )
        .bind(other_written.handle)
        .fetch_one(pool)
        .await?;
        assert_eq!(other_heads, 1);
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("owner erase failed");
}

/// #488: forget keeps the row, so an owner erase finds a forgotten memory in
/// `memory` and hard-deletes it with its keys and content like any other.
#[tokio::test]
async fn erase_personal_owner_destroys_a_forgotten_memory_and_gcs_content() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        let pool = pg.pool_for_tests();
        let user = UserId::new(Uuid::now_v7());
        let owner = OwnerId::from(user);
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let written = pg
            .ingest_fact_atomic(&permit, &draft(Some(("src", "k-forgotten"))), None)
            .await?;
        let t = written.memory_id.into_inner();
        MemoryAuthoringPort::forget_memory(&pg, &permit, written.memory_id).await?;
        let keys_after_forget: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.ingest_keys WHERE ingest_key = 'k-forgotten'",
        )
        .fetch_one(pool)
        .await?;
        assert_eq!(keys_after_forget, 1, "forget leaves ingest_keys");
        let forgotten_before: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.memory
              WHERE t = $1 AND forgotten_at IS NOT NULL",
        )
        .bind(t)
        .fetch_one(pool)
        .await?;
        assert_eq!(forgotten_before, 1, "forget keeps the row");

        let auth = EraseAuthorization::new_for_tests(OwnerEraseTarget::PersonalOwner {
            user_id: user,
            drop_event_id: "test-drop-forgotten".into(),
        });
        let outcome = pg
            .erase_personal_owner(&auth, user, &contract_sidecar_tables())
            .await?;
        assert!(
            matches!(outcome, OwnerEraseOutcome::Completed { .. }),
            "got {outcome:?}"
        );
        let rows_after: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.memory WHERE t = $1")
                .bind(t)
                .fetch_one(pool)
                .await?;
        assert_eq!(rows_after, 0, "owner erase must delete a forgotten row");
        let content: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.content c
              WHERE NOT EXISTS (SELECT 1 FROM proxima_core.memory m WHERE m.content_id = c.content_id)",
        )
        .fetch_one(pool)
        .await?;
        assert_eq!(content, 0, "unreferenced Content must be GC'd");
        let keys_after_erase: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.ingest_keys WHERE ingest_key = 'k-forgotten'",
        )
        .fetch_one(pool)
        .await?;
        assert_eq!(
            keys_after_erase, 0,
            "owner erase must delete ingest_keys of forgotten facts"
        );
        assert_eq!(
            pending_debts(pool).await?,
            0,
            "a forgotten memory owes the object store nothing"
        );
        let witness: Option<String> = sqlx::query_scalar(
            "SELECT kind::text FROM proxima_core.erased_pin_target WHERE t = $1",
        )
        .bind(t)
        .fetch_optional(pool)
        .await?;
        assert_eq!(
            witness.as_deref(),
            Some("fact"),
            "owner erase hard-deletes a forgotten Memory and records its kind witness"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("owner erase of a forgotten memory failed");
}

#[tokio::test]
async fn erase_personal_owner_destroys_blobs_uploads_and_citation_sidecars() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        let pool = pg.pool_for_tests();
        create_citation_sidecar_tables(pool).await?;
        let user = UserId::new(Uuid::now_v7());
        let owner = OwnerId::from(user);
        let blob = cite_blob(&pg, owner, Some(("src", "k-blob")), 3).await?;
        // A pending upload names no blob: owner data attributable to no source.
        sqlx::query(
            "INSERT INTO proxima_core.blob_uploads
                 (owner_id, bucket, object_store, object_key, filename, mime, expected_byte_len, expires_at)
             VALUES ($1, 'bucket', 's3:test/bucket', 'pending/1', 'draft.pdf', 'application/pdf', 1,
                     now() + interval '1 hour')",
        )
        .bind(owner.into_inner())
        .execute(pool)
        .await?;

        let neighbour = OwnerId::from(UserId::new(Uuid::now_v7()));
        let neighbour_blob = cite_blob(&pg, neighbour, Some(("src", "k-other")), 5).await?;

        let auth = EraseAuthorization::new_for_tests(OwnerEraseTarget::PersonalOwner {
            user_id: user,
            drop_event_id: "test-drop-blob".into(),
        });
        let outcome = pg
            .erase_personal_owner(&auth, user, &citation_surfaces())
            .await?;
        let OwnerEraseOutcome::Completed { counts, .. } = outcome else {
            panic!("expected completed erase, got {outcome:?}");
        };
        assert_eq!(counts.get("blobs"), 1);
        assert_eq!(counts.get("blob_uploads"), 2, "the pending upload goes too");
        assert_eq!(counts.get("sidecar_rows"), 2, "both citation families");

        assert_eq!(
            rows_for_blob(pool, blob).await?,
            (0, 0, 0, 0),
            "no row keyed on the owner's blob survives"
        );
        let owner_uploads: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.blob_uploads WHERE owner_id = $1",
        )
        .bind(owner.into_inner())
        .fetch_one(pool)
        .await?;
        assert_eq!(owner_uploads, 0, "no upload row of the owner survives");

        assert_eq!(
            rows_for_blob(pool, neighbour_blob).await?,
            (1, 1, 1, 1),
            "another owner's cited blob is untouched"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("owner blob erase failed");
}

/// An erase that aborts after deleting a forgotten row leaves the row, still
/// forgotten, and no witness: the whole erase is one transaction.
#[tokio::test]
async fn an_aborted_owner_erase_keeps_the_forgotten_row() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        let pool = pg.pool_for_tests();
        let user = UserId::new(Uuid::now_v7());
        let owner = OwnerId::from(user);
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let written = pg
            .ingest_fact_atomic(&permit, &draft(Some(("src", "k-abort"))), None)
            .await?;
        let t = written.memory_id.into_inner();
        MemoryAuthoringPort::forget_memory(&pg, &permit, written.memory_id).await?;

        // Refuse the last statement of the erase, after the memory delete,
        // so everything before it must roll back with it.
        sqlx::raw_sql(
            "CREATE FUNCTION public.refuse_memory_delete() RETURNS trigger
             LANGUAGE plpgsql AS $$
             BEGIN
                 RAISE EXCEPTION 'injected erase refusal';
             END
             $$;
             CREATE TRIGGER refuse_memory_delete
             AFTER DELETE ON proxima_core.memory
             FOR EACH ROW EXECUTE FUNCTION public.refuse_memory_delete();",
        )
        .execute(pool)
        .await?;

        let auth = EraseAuthorization::new_for_tests(OwnerEraseTarget::PersonalOwner {
            user_id: user,
            drop_event_id: "test-drop-abort".into(),
        });
        let err = pg
            .erase_personal_owner(&auth, user, &contract_sidecar_tables())
            .await
            .expect_err("the injected refusal aborts the erase");
        assert!(
            err.to_string().contains("injected erase refusal"),
            "got: {err}"
        );

        let kept: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.memory
              WHERE t = $1 AND forgotten_at IS NOT NULL",
        )
        .bind(t)
        .fetch_one(pool)
        .await?;
        assert_eq!(kept, 1, "the forgotten row survives an aborted erase");
        let witness: Option<String> = sqlx::query_scalar(
            "SELECT kind::text FROM proxima_core.erased_pin_target WHERE t = $1",
        )
        .bind(t)
        .fetch_optional(pool)
        .await?;
        assert_eq!(
            witness, None,
            "an aborted owner erase must not leave a historical witness"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("aborted owner erase consistency test failed");
}

/// `blob` holds the owner's content hashes and `blob_uploads` its filenames,
/// buckets and S3 object keys; a registered citation sidecar holds whatever the
/// flavor's citation payload is. Nothing collected any of it — erase deletes no
/// `owners` row, so the FKs never fired and the rows were immortal.
///
/// The committed erase destroys the objects it owes: an owned upload's
/// object. A forgotten memory owes none since #488.
#[tokio::test]
async fn erase_personal_owner_purges_cold_objects_after_commit() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let cold = Arc::new(BucketNamedCold::new("bucket"));
        let pg = PgStorage::connect_for_tests(url.clone())
            .await?
            .with_cold(cold.clone());
        let pool = pg.pool_for_tests();
        let user = UserId::new(Uuid::now_v7());
        let owner = OwnerId::from(user);
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let written = pg
            .ingest_fact_atomic(&permit, &draft(Some(("src", "k-cold"))), None)
            .await?;
        MemoryAuthoringPort::forget_memory(&pg, &permit, written.memory_id).await?;
        let (_blob, key) = completed_blob_upload(pool, owner, 21).await?;
        cold.put(&key, b"owned object");

        let auth = EraseAuthorization::new_for_tests(OwnerEraseTarget::PersonalOwner {
            user_id: user,
            drop_event_id: "test-drop-cold".into(),
        });
        let outcome = pg
            .erase_personal_owner(&auth, user, &contract_sidecar_tables())
            .await?;
        assert!(
            matches!(
                outcome,
                OwnerEraseOutcome::Completed {
                    cold_object_purge_pending: false,
                    ..
                }
            ),
            "got {outcome:?}"
        );
        assert!(
            matches!(cold.get(&key), Err(StorageError::NotFound)),
            "the committed erase destroys the owned object"
        );
        assert_eq!(
            pending_debts(pool).await?,
            0,
            "a destroyed object clears its pending mark, and the forgotten memory owed none"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("owner erase cold purge failed");
}

#[tokio::test]
async fn failed_cold_purge_is_attributed_and_bounded_retry_clears_audit() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let cold = Arc::new(RefusingDeleteCold);
        let pg = PgStorage::connect_for_tests(url.clone())
            .await?
            .with_cold(cold.clone());
        let pool = pg.pool_for_tests();
        let user = UserId::new(Uuid::now_v7());
        let owner = OwnerId::from(user);
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        pg.ingest_fact_atomic(&permit, &draft(None), None).await?;
        let (_blob, _key) = completed_blob_upload(pool, owner, 22).await?;

        let auth = EraseAuthorization::new_for_tests(OwnerEraseTarget::PersonalOwner {
            user_id: user,
            drop_event_id: "test-drop-retry".into(),
        });
        let outcome = pg
            .erase_personal_owner(&auth, user, &contract_sidecar_tables())
            .await?;
        assert!(matches!(
            outcome,
            OwnerEraseOutcome::Completed {
                cold_object_purge_pending: true,
                ..
            }
        ));
        // The queue IS the debt: outstanding debt is a row count, which cannot
        // disagree with itself the way a queue row and a mirrored boolean on a
        // journal row can. The fixture records a key that is neither of the
        // keys its upload implies, so the erase owes all three.
        assert_eq!(pending_debts(pool).await?, 3);
        sqlx::query(
            "INSERT INTO proxima_core.cold_purge_pending (object_key, owner_id, object_store)
             VALUES ('objects/test-second-debt', $1, 's3:test/bucket')",
        )
        .bind(owner.into_inner())
        .execute(pool)
        .await?;

        // The erase's own post-commit attempt failed and backed its debt
        // off, so the only due debt is the second one.
        let failed_retry = pg
            .retry_cold_object_purges(ColdPurgeRetryOptions::default().batch_size(1))
            .await?;
        assert_eq!(
            (
                failed_retry.selected,
                failed_retry.purged,
                failed_retry.failed
            ),
            (1, 0, 1)
        );
        assert_eq!((failed_retry.due, failed_retry.deferred), (0, 4));
        assert_eq!(failed_retry.remaining(), 4);
        let attempts: Vec<(i32, Option<String>)> = sqlx::query_as(
            "SELECT attempts, last_error FROM proxima_core.cold_purge_pending ORDER BY object_key",
        )
        .fetch_all(pool)
        .await?;
        assert_eq!(attempts.len(), 4);
        for (count, error) in &attempts {
            assert_eq!(*count, 1, "each failure is recorded once");
            assert!(
                error.as_deref().is_some_and(|e| e.contains("delete refused")),
                "{error:?}"
            );
        }

        let retry_pg = PgStorage::connect_for_tests(url.clone())
            .await?
            .with_cold(Arc::new(BucketNamedCold::new("bucket")));
        let idle = retry_pg
            .retry_cold_object_purges(ColdPurgeRetryOptions::default().batch_size(1))
            .await?;
        assert_eq!(
            (idle.selected, idle.deferred),
            (0, 4),
            "a backed-off debt is not retried early"
        );
        sqlx::query(
            "UPDATE proxima_core.cold_purge_pending SET next_attempt_at = now() - interval '1 second'",
        )
        .execute(pool)
        .await?;
        let dry_run = retry_pg
            .retry_cold_object_purges(ColdPurgeRetryOptions::default().batch_size(1).dry_run(true))
            .await?;
        assert_eq!(
            (dry_run.selected, dry_run.purged, dry_run.remaining()),
            (1, 0, 4)
        );
        let first = retry_pg
            .retry_cold_object_purges(ColdPurgeRetryOptions::default().batch_size(1))
            .await?;
        // These fixtures never wrote bytes, so the keys clear as absent.
        assert_eq!(
            (first.selected, first.purged, first.absent, first.failed),
            (1, 0, 1, 0)
        );
        assert_eq!(first.remaining(), 3);
        assert_eq!(
            pending_debts(pool).await?,
            3,
            "the first key clears its own debt and no more"
        );
        let second = retry_pg
            .retry_cold_object_purges(ColdPurgeRetryOptions::default().batch_size(10))
            .await?;
        assert_eq!(
            (second.selected, second.purged, second.absent, second.failed),
            (3, 0, 3, 0)
        );
        assert_eq!(second.remaining(), 0);
        assert_eq!(pending_debts(pool).await?, 0);
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("cold purge retry test failed");
}

/// A deadlock inside the erase transaction is retried, not surfaced.
///
/// The erase takes object-key fences before its row deletes while a stage or
/// an abort takes its upload row first, so the two orders cross and `40P01`
/// is a normal outcome of correct code rather than a fault. An operator
/// draining a departed owner must not have to re-run the verb by hand
/// because someone uploaded at the wrong moment.
///
/// The injection is a real Postgres deadlock error raised by a trigger on the
/// first DELETE statement against `memory` and never again — the sequence is
/// non-transactional, so it survives the rollback the raise causes. The
/// counter reading past one IS the retry: without the bounded loop the erase
/// returns the error and the statement runs exactly once.

#[tokio::test]
async fn an_injected_deadlock_inside_owner_erase_is_retried() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone())
            .await?
            .with_cold(Arc::new(MemoryColdStore));
        let pool = pg.pool_for_tests();
        let user = UserId::new(Uuid::now_v7());
        let owner = OwnerId::from(user);
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        pg.ingest_fact_atomic(&permit, &draft(None), None).await?;

        sqlx::raw_sql(
            "CREATE SEQUENCE public.erase_deadlock_probe;
             CREATE FUNCTION public.inject_erase_deadlock() RETURNS trigger
             LANGUAGE plpgsql AS $$
             BEGIN
                 IF nextval('public.erase_deadlock_probe') = 1 THEN
                     RAISE EXCEPTION 'injected deadlock' USING ERRCODE = '40P01';
                 END IF;
                 RETURN NULL;
             END
             $$;
             CREATE TRIGGER inject_erase_deadlock
             AFTER DELETE ON proxima_core.memory
             FOR EACH STATEMENT
             EXECUTE FUNCTION public.inject_erase_deadlock();",
        )
        .execute(pool)
        .await?;
        sqlx::query(
            "GRANT USAGE, SELECT, UPDATE ON SEQUENCE public.erase_deadlock_probe TO PUBLIC",
        )
        .execute(pool)
        .await?;

        let auth = EraseAuthorization::new_for_tests(OwnerEraseTarget::PersonalOwner {
            user_id: user,
            drop_event_id: "retried-drop".into(),
        });
        let outcome = pg
            .erase_personal_owner(&auth, user, &contract_sidecar_tables())
            .await?;
        assert!(
            matches!(outcome, OwnerEraseOutcome::Completed { .. }),
            "the retried attempt commits: {outcome:?}"
        );

        let attempts: i64 =
            sqlx::query_scalar("SELECT last_value FROM public.erase_deadlock_probe")
                .fetch_one(pool)
                .await?;
        assert!(
            attempts >= 2,
            "the whole transaction was re-run after the deadlock, not just reported (fired \
             {attempts} times)"
        );
        let rows: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.memory WHERE owner_id = $1",
        )
        .bind(owner.into_inner())
        .fetch_one(pool)
        .await?;
        assert_eq!(rows, 0, "and the retry finished the erase");
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("erase retry test failed");
}

/// The drain leaves debts owed to another object store untouched, names
/// them, and still drains its own.
///
/// Before the queue named its store, store and queue were matched by boot
/// convention alone: point a process at the right database and the wrong
/// bucket and every drain "succeeded", deleting nothing and clearing the
/// debt, so bytes an erase promised to destroy survived with no record that
/// they did. A foreign debt is never selected, attempted or cleared; the
/// outcome counts and names it so the mis-wiring is loud, and the wired
/// store's own debts are not held hostage by it.

#[tokio::test]
async fn a_drain_skips_and_names_debts_owed_by_another_store() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let cold = Arc::new(BucketNamedCold::new("wired-bucket"));
        let pg = PgStorage::connect_for_tests(url.clone())
            .await?
            .with_cold(cold.clone());
        let pool = pg.pool_for_tests();
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        // Mint the owner row the queue's FK needs.
        pg.ingest_fact_atomic(&permit, &draft(None), None).await?;

        cold.put("cold/mine", b"wired bytes");
        sqlx::query(
            "INSERT INTO proxima_core.cold_purge_pending (object_key, owner_id, object_store)
             VALUES ('cold/mine', $1, 's3:test/wired-bucket'),
                    ('cold/theirs', $1, 's3:test/other-bucket')",
        )
        .bind(owner.into_inner())
        .execute(pool)
        .await?;

        let foreign_before: (i32, time::OffsetDateTime, time::OffsetDateTime) = sqlx::query_as(
            "SELECT attempts, next_attempt_at, enqueued_at FROM proxima_core.cold_purge_pending
              WHERE object_store = 's3:test/other-bucket'",
        )
        .fetch_one(pool)
        .await?;
        let drained = pg
            .retry_cold_object_purges(ColdPurgeRetryOptions::default().batch_size(10))
            .await?;
        assert_eq!(
            (drained.selected, drained.purged, drained.failed),
            (1, 1, 0),
            "the wired store's own debt drains"
        );
        assert_eq!(drained.foreign, 1);
        assert_eq!(
            drained
                .foreign_stores
                .iter()
                .map(|(store, count)| (store.as_str().to_owned(), *count))
                .collect::<Vec<_>>(),
            vec![("s3:test/other-bucket".to_owned(), 1)],
            "the operator is told which store is owed"
        );
        assert_eq!(drained.remaining(), 0, "nothing else is owed to the wired store");
        let foreign_after: (i32, time::OffsetDateTime, time::OffsetDateTime) = sqlx::query_as(
            "SELECT attempts, next_attempt_at, enqueued_at FROM proxima_core.cold_purge_pending
              WHERE object_store = 's3:test/other-bucket'",
        )
        .fetch_one(pool)
        .await?;
        assert_eq!(foreign_after, foreign_before, "the foreign debt is untouched");
        sqlx::query("DELETE FROM proxima_core.cold_purge_pending WHERE object_store = 's3:test/other-bucket'")
            .execute(pool)
            .await?;
        assert_eq!(pending_debts(pool).await?, 0);
        assert!(
            matches!(cold.get("cold/mine"), Err(StorageError::NotFound)),
            "the matching debt is discharged against the wired store"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("store-matched drain test failed");
}

/// Source scope can delete only blobs captured from its selected admissions.
/// Surviving live or forgotten citations protect shared blobs, while an
/// unrelated unreferenced completed upload is outside the source scope
/// entirely.
#[tokio::test]
async fn erase_source_scope_deletes_only_unshared_selected_blobs_and_objects() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        // The cited-upload fixtures below explicitly name this bucket.
        let cold = Arc::new(BucketNamedCold::new("bucket"));
        let pg = PgStorage::connect_for_tests(url.clone())
            .await?
            .with_cold(cold.clone());
        let pool = pg.pool_for_tests();
        create_citation_sidecar_tables(pool).await?;
        let user = UserId::new(Uuid::now_v7());
        let owner = OwnerId::from(user);
        let kept = cite_blob(&pg, owner, Some(("src-keep", "k-keep")), 7).await?;
        let shared = cite_blob(&pg, owner, Some(("src-drop", "k-shared-drop")), 8).await?;
        let dropped = cite_blob(&pg, owner, Some(("src-drop", "k-drop")), 9).await?;
        let (unrelated, unrelated_key) = completed_blob_upload(pool, owner, 10).await?;
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let mut shared_survivor = draft(Some(("src-keep", "k-shared-keep")));
        shared_survivor.blob_id = Some(shared);
        let shared_survivor = pg
            .ingest_fact_atomic(&permit, &shared_survivor, None)
            .await?;
        MemoryAuthoringPort::forget_memory(&pg, &permit, shared_survivor.memory_id).await?;
        cold.put("objects/8", b"shared cited object");
        cold.put("objects/9", b"target cited object");
        cold.put(&unrelated_key, b"unrelated object");
        sqlx::query(
            "INSERT INTO proxima_core.blob_uploads
                 (owner_id, bucket, object_store, object_key, filename, mime, expected_byte_len, expires_at)
             VALUES ($1, 'bucket', 's3:test/bucket', 'pending/2', 'draft.pdf', 'application/pdf', 1,
                     now() + interval '1 hour')",
        )
        .bind(owner.into_inner())
        .execute(pool)
        .await?;

        let source = SourceId::new("src-drop");
        let auth = EraseAuthorization::new_for_tests(OwnerEraseTarget::PersonalSourceScope {
            user_id: user,
            source_id: source.clone(),
            drop_event_id: "test-drop-blob-scope".into(),
        });
        let outcome = pg
            .erase_personal_source_scope(&auth, user, &source, &citation_surfaces())
            .await?;
        let OwnerEraseOutcome::Completed { counts, .. } = outcome else {
            panic!("expected completed erase, got {outcome:?}");
        };
        assert_eq!(counts.get("blobs"), 1, "only the unreferenced blob goes");
        assert_eq!(counts.get("blob_uploads"), 1, "its upload row goes with it");
        assert_eq!(
            counts.get("sidecar_rows"),
            2,
            "its two citation sidecar rows"
        );

        assert_eq!(
            rows_for_blob(pool, dropped).await?,
            (0, 0, 0, 0),
            "the blob no surviving Fact cites goes, with everything keyed on it"
        );
        assert_eq!(
            rows_for_blob(pool, kept).await?,
            (1, 1, 1, 1),
            "a blob a surviving Fact cites must survive a source-scope erase"
        );
        assert_eq!(
            rows_for_blob(pool, shared).await?,
            (1, 1, 1, 1),
            "a surviving forgotten citation must protect its shared blob"
        );
        assert_eq!(
            rows_for_blob(pool, unrelated).await?,
            (1, 1, 0, 0),
            "an unrelated unreferenced completed blob is not source-owned"
        );
        assert!(
            matches!(cold.get("objects/9"), Err(StorageError::NotFound)),
            "the selected upload's exact object key must be purged"
        );
        assert!(
            cold.get("objects/8").is_ok(),
            "the shared blob's object must survive"
        );
        assert!(
            cold.get(&unrelated_key).is_ok(),
            "the unrelated upload's object must survive"
        );
        let pending_uploads: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.blob_uploads
              WHERE owner_id = $1 AND blob_id IS NULL",
        )
        .bind(owner.into_inner())
        .fetch_one(pool)
        .await?;
        assert_eq!(
            pending_uploads, 1,
            "an upload naming no blob is owner-level and no source's to erase"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("source-scope blob erase failed");
}

/// The registry, not the request's shape, decides what an erase needs: a
/// user's id named through the group path, with no members and no drop
/// proof, is refused, and its memories stay.
#[tokio::test]
async fn erasing_a_users_id_through_the_group_path_without_the_drop_proof_is_refused() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        let pool = pg.pool_for_tests();

        let user = UserId::new(Uuid::now_v7());
        let owner = OwnerId::from(user);
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let written = pg
            .ingest_fact_atomic(&permit, &draft(Some(("src", "u1"))), None)
            .await?;
        let t = written.memory_id.into_inner();
        let as_group = owner.as_group();

        let auth =
            EraseAuthorization::new_for_tests(OwnerEraseTarget::GroupOwner { group_id: as_group });
        let outcome = pg
            .erase_group_owner(&auth, as_group, &contract_sidecar_tables())
            .await?;
        let OwnerEraseOutcome::Refused { reason, .. } = outcome else {
            panic!("expected PersonalDropNotVerified, got {outcome:?}");
        };
        assert_eq!(reason, OwnerEraseRefusal::PersonalDropNotVerified);

        let source = SourceId::new("src");
        let auth = EraseAuthorization::new_for_tests(OwnerEraseTarget::GroupSourceScope {
            group_id: as_group,
            source_id: source.clone(),
        });
        let outcome = pg
            .erase_group_source_scope(&auth, as_group, &source, &contract_sidecar_tables())
            .await?;
        let OwnerEraseOutcome::Refused { reason, .. } = outcome else {
            panic!("expected PersonalDropNotVerified, got {outcome:?}");
        };
        assert_eq!(reason, OwnerEraseRefusal::PersonalDropNotVerified);

        let remaining: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.memory WHERE t = $1")
                .bind(t)
                .fetch_one(pool)
                .await?;
        assert_eq!(remaining, 1, "a refused erase keeps the user's memories");
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("user id through the group path");
}

#[tokio::test]
async fn erase_group_owner_refuses_while_membership_rows_exist() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        let pool = pg.pool_for_tests();

        let group = GroupId::new(Uuid::now_v7());
        let admin = UserId::new(Uuid::now_v7());
        pg.bootstrap_group_admin(group, admin, admin.into_inner())
            .await?;

        let owner = OwnerId::from(group);
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let written = pg
            .ingest_fact_atomic(&permit, &draft(Some(("src", "g1"))), None)
            .await?;
        let t = written.memory_id.into_inner();

        let auth =
            EraseAuthorization::new_for_tests(OwnerEraseTarget::GroupOwner { group_id: group });
        let outcome = pg
            .erase_group_owner(&auth, group, &contract_sidecar_tables())
            .await?;
        let OwnerEraseOutcome::Refused { reason, .. } = outcome else {
            panic!("expected OwnerNotAbandoned, got {outcome:?}");
        };
        assert_eq!(reason, OwnerEraseRefusal::OwnerNotAbandoned);

        let remaining: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.memory WHERE t = $1")
                .bind(t)
                .fetch_one(pool)
                .await?;
        assert_eq!(remaining, 1, "live group must keep its memories");
        let witness: Option<String> = sqlx::query_scalar(
            "SELECT kind::text FROM proxima_core.erased_pin_target WHERE t = $1",
        )
        .bind(t)
        .fetch_optional(pool)
        .await?;
        assert_eq!(
            witness, None,
            "a refused group erase must not write a witness"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("group erase refuse failed");
}

#[tokio::test]
async fn erase_group_owner_completes_when_abandoned() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        let pool = pg.pool_for_tests();

        let group = GroupId::new(Uuid::now_v7());
        let owner = OwnerId::from(group);
        register_group_for_tests(pool, group).await?;
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let written = pg
            .ingest_fact_atomic(&permit, &draft(Some(("src", "g-empty"))), None)
            .await?;
        let t = written.memory_id.into_inner();
        let mut gtx = pool.begin().await?;
        let goal = write_goal(
            &mut gtx,
            &owner,
            proxima_storage_pg::test_fixtures::writer(),
            &GoalWriteCommand {
                handle: None,
                schema_id: "core/task-goal-v1".into(),
                title: "erase me".into(),
                text: None,
                state: GoalState::Active,
                request_id: "erase-g".into(),
                close_fact_t: None,
                assignment_t: None,
                dependency_t: vec![],
                evidence_t: vec![],
                mint_write_act: false,
                write_act_t: None,
                parent_handle: None,
            },
            1,
        )
        .await?;
        gtx.commit().await?;

        let auth =
            EraseAuthorization::new_for_tests(OwnerEraseTarget::GroupOwner { group_id: group });
        let outcome = pg
            .erase_group_owner(&auth, group, &contract_sidecar_tables())
            .await?;
        let OwnerEraseOutcome::Completed { counts, .. } = outcome else {
            panic!("expected completed erase, got {outcome:?}");
        };
        assert_eq!(counts.get("memories"), 1);
        assert_eq!(counts.get("goals"), 1);

        let remaining: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.memory WHERE t = $1")
                .bind(t)
                .fetch_one(pool)
                .await?;
        assert_eq!(remaining, 0);
        let goal_heads: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.goal_head WHERE handle = $1",
        )
        .bind(goal.handle)
        .fetch_one(pool)
        .await?;
        assert_eq!(goal_heads, 0, "owner erase deletes empty goal heads");
        let heads: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.memory_head WHERE handle = $1",
        )
        .bind(written.handle)
        .fetch_one(pool)
        .await?;
        assert_eq!(heads, 0);
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("abandoned group erase failed");
}

#[tokio::test]
async fn erase_source_scope_rewinds_head_to_newest_live_remaining_t() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        let pool = pg.pool_for_tests();
        let user = UserId::new(Uuid::now_v7());
        let owner = OwnerId::from(user);
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let first = pg
            .ingest_fact_atomic(&permit, &draft(Some(("src-old", "k-old"))), None)
            .await?;
        // A forgotten version between them must not become the head: owner
        // reads skip it, so the series would read as absent although `first`
        // is live (#488 "the head is the newest live version").
        let mut middle = draft(Some(("src-old", "k-mid")));
        middle.handle = Some(first.handle);
        let middle = pg.ingest_fact_atomic(&permit, &middle, None).await?;
        let mut later = draft(Some(("src-new", "k-new")));
        later.handle = Some(first.handle);
        let second = pg.ingest_fact_atomic(&permit, &later, None).await?;
        assert_eq!(second.handle, first.handle);
        MemoryAuthoringPort::forget_memory(&pg, &permit, middle.memory_id).await?;

        let auth = EraseAuthorization::new_for_tests(OwnerEraseTarget::PersonalSourceScope {
            user_id: user,
            source_id: SourceId::new("src-new"),
            drop_event_id: "test-drop".into(),
        });
        let outcome = pg
            .erase_personal_source_scope(
                &auth,
                user,
                &SourceId::new("src-new"),
                &contract_sidecar_tables(),
            )
            .await?;
        let OwnerEraseOutcome::Completed { counts, .. } = outcome else {
            panic!("expected completed erase, got {outcome:?}");
        };
        assert_eq!(counts.get("memories"), 1);
        let remaining: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.memory WHERE t = $1")
                .bind(first.memory_id.into_inner())
                .fetch_one(pool)
                .await?;
        assert_eq!(remaining, 1);
        let gone: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.memory WHERE t = $1")
                .bind(second.memory_id.into_inner())
                .fetch_one(pool)
                .await?;
        assert_eq!(gone, 0);
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
    result.expect("source-scope rewind failed");
}

#[tokio::test]
async fn erase_source_scope_destroys_forgotten_from_that_source() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        let pool = pg.pool_for_tests();
        let user = UserId::new(Uuid::now_v7());
        let owner = OwnerId::from(user);
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let target = pg
            .ingest_fact_atomic(&permit, &draft(Some(("src-forgotten", "k-forgotten-src"))), None)
            .await?;
        let other = pg
            .ingest_fact_atomic(&permit, &draft(Some(("src-keep", "k-keep"))), None)
            .await?;
        MemoryAuthoringPort::forget_memory(&pg, &permit, target.memory_id).await?;
        let foreign_owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let foreign_permit = OwnerWritePermit::new_for_tests(foreign_owner, AccessKind::Fact);
        let mut foreign_source = draft(Some(("foreign", "k-foreign")));
        foreign_source.refs = vec![target.memory_id.into_inner()];
        let foreign = pg
            .ingest_fact_atomic(&foreign_permit, &foreign_source, None)
            .await?;

        let auth = EraseAuthorization::new_for_tests(OwnerEraseTarget::PersonalSourceScope {
            user_id: user,
            source_id: SourceId::new("src-forgotten"),
            drop_event_id: "test-drop-forgotten-src".into(),
        });
        let outcome = pg
            .erase_personal_source_scope(
                &auth,
                user,
                &SourceId::new("src-forgotten"),
                &contract_sidecar_tables(),
            )
            .await?;
        assert!(
            matches!(outcome, OwnerEraseOutcome::Completed { .. }),
            "got {outcome:?}"
        );
        let forgotten: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.memory WHERE t = $1")
                .bind(target.memory_id.into_inner())
                .fetch_one(pool)
                .await?;
        assert_eq!(
            forgotten, 0,
            "source-scope erase must select a forgotten row by source_id"
        );
        let kept: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.memory WHERE t = $1")
                .bind(other.memory_id.into_inner())
                .fetch_one(pool)
                .await?;
        assert_eq!(kept, 1, "other source must remain hot");
        let keys: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.ingest_keys WHERE ingest_key = 'k-forgotten-src'",
        )
        .fetch_one(pool)
        .await?;
        assert_eq!(keys, 0);
        let target_witness: Option<String> = sqlx::query_scalar(
            "SELECT kind::text FROM proxima_core.erased_pin_target WHERE t = $1",
        )
        .bind(target.memory_id.into_inner())
        .fetch_optional(pool)
        .await?;
        assert_eq!(target_witness.as_deref(), Some("fact"));
        let other_witness: Option<String> = sqlx::query_scalar(
            "SELECT kind::text FROM proxima_core.erased_pin_target WHERE t = $1",
        )
        .bind(other.memory_id.into_inner())
        .fetch_optional(pool)
        .await?;
        assert_eq!(
            other_witness, None,
            "source-scope erase must not witness the other source"
        );
        let foreign_refs: Vec<Uuid> =
            sqlx::query_scalar("SELECT refs FROM proxima_core.memory WHERE t = $1")
                .bind(foreign.memory_id.into_inner())
                .fetch_one(pool)
                .await?;
        assert_eq!(
            foreign_refs,
            vec![target.memory_id.into_inner()],
            "a cross-owner source retains its exact refs after target erase"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("source-scope forgotten erase failed");
}

/// Specialized person erasure removes their written relations and projected
/// roles, preserving other members. Generic surface erasure stays excluded;
/// access metadata remains outside corpus counters.
#[tokio::test]
async fn erasing_a_member_removes_its_relations_and_preserves_other_members() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        let pool = pg.pool_for_tests();

        let user = UserId::new(Uuid::now_v7());
        let owner = OwnerId::from(user);
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        // A memory, so the erase has something of its own to do and the
        // survival below is not the survival of an empty operation.
        pg.ingest_fact_atomic(&permit, &draft(Some(("src", "k1"))), None)
            .await?;

        let group_id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO proxima_core.owners (owner_id, kind)
             VALUES ($1, 'group'::proxima_core.owner_kind) ON CONFLICT DO NOTHING",
        )
        .bind(group_id)
        .execute(pool)
        .await?;
        sqlx::query(
            "INSERT INTO proxima_core.group_memberships (group_id, member_user_id, relation)
             VALUES ($1, $2, 'editor'::proxima_core.membership_relation)",
        )
        .bind(group_id)
        .bind(user.into_inner())
        .execute(pool)
        .await?;

        let bystander = UserId::new(Uuid::now_v7());
        sqlx::query("INSERT INTO proxima_core.group_memberships(group_id,member_user_id,relation) VALUES($1,$2,'viewer')")
            .bind(group_id).bind(bystander.into_inner()).execute(pool).await?;

        let auth = EraseAuthorization::new_for_tests(OwnerEraseTarget::PersonalOwner {
            user_id: user,
            drop_event_id: "test-drop".into(),
        });
        let outcome = pg
            .erase_personal_owner(&auth, user, &contract_sidecar_tables())
            .await?;
        let OwnerEraseOutcome::Completed { counts, .. } = outcome else {
            panic!("expected completed erase, got {outcome:?}");
        };

        let surviving: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.group_memberships
              WHERE group_id = $1 AND member_user_id = $2",
        )
        .bind(group_id)
        .bind(user.into_inner())
        .fetch_one(pool)
        .await?;
        assert_eq!(
            surviving, 0,
            "erasing a person removes their relations so the reference climb cannot restore their roles"
        );

        let remaining: i64 = sqlx::query_scalar("SELECT count(*) FROM proxima_core.group_memberships WHERE group_id=$1 AND member_user_id=$2")
            .bind(group_id).bind(bystander.into_inner()).fetch_one(pool).await?;
        assert_eq!(remaining, 1, "other members keep their written relations");
        let projected: Vec<Uuid> = sqlx::query_scalar("SELECT member_user_id FROM proxima_core.group_role_projection WHERE group_id=$1")
            .bind(group_id).fetch_all(pool).await?;
        assert_eq!(projected, vec![bystander.into_inner()], "only the bystander keeps a projected role");

        assert!(
            counts.get("memories") >= 1,
            "the erase did its own work, so the survival above is not the \
             survival of a no-op"
        );

        // Tie the behaviour back to the declaration it witnesses, so that
        // changing the position without changing this test is a failure
        // rather than a silently stale assertion. `Uncounted` is why the
        // receipt above names no counter for this table: the erase's only
        // membership cleanup belongs to specialized owner erasure, outside
        // the generic surface sweep.
        let surface = proxima_core::FLAVOR_0
            .all_surfaces()
            .find(|surface| surface.table == "proxima_core.group_memberships")
            .expect("flavor #0 declares the membership surface");
        assert!(
            matches!(surface.erase, proxima_core::flavor::EraseRule::Never { .. }),
            "the row survived because the declaration says it must, not by \
             accident of which statements the erase happens to run"
        );
        assert!(
            matches!(
                surface.counter,
                proxima_core::flavor::CounterRule::Uncounted { .. }
            ),
            "and nothing is destroyed here, so there is no counter to carry"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("erasing_a_member_removes_its_relations_and_preserves_other_members failed");
}

/// An admission that crossed the shared owner fence before an owner erase
/// asked for its exclusive fence commits as a whole, and is then inside the
/// scope the erase selects once it holds the fence. The erase waits for the
/// writer rather than refusing, so it always makes progress.
///
/// The insert trigger holds the writer transaction after the real Memory row
/// is materialized, avoiding any dependence on advisory-lock waiter fairness
/// when the erase queues its exclusive request.
#[tokio::test]
async fn owner_erase_includes_a_fact_that_committed_before_its_fence() {
    const ADMISSION_GATE: i64 = 7_514_001;

    let (db_name, pg) = fresh_owner_erase_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pool = pg.pool_for_tests();
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let seed = pg
            .ingest_fact_atomic(&permit, &draft(Some(("same-source", "seed-owner"))), None)
            .await?;
        let seed_t = seed.memory_id.into_inner();
        install_memory_insert_gate(pool, ADMISSION_GATE).await?;

        let mut admission_gate = hold_session_advisory_lock(pool, ADMISSION_GATE).await?;
        let late_pg = pg.clone();
        let late_permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let late = tokio::spawn(async move {
            late_pg
                .ingest_fact_atomic(
                    &late_permit,
                    &draft(Some(("same-source", "late-owner"))),
                    None,
                )
                .await
        });
        wait_for_ungranted_advisory_key(pool, ADMISSION_GATE).await?;

        let erase_pg = pg.clone();
        let erase = tokio::spawn(async move {
            let auth = EraseAuthorization::new_for_tests(OwnerEraseTarget::PersonalOwner {
                user_id: owner.as_user(),
                drop_event_id: "owner-late-fact".into(),
            });
            erase_pg
                .erase_personal_owner(&auth, owner.as_user(), &contract_sidecar_tables())
                .await
        });
        wait_for_ungranted_advisory_label(pool, &owner_fence_key(owner)).await?;
        assert!(!erase.is_finished(), "the owner fence must still be held");

        release_session_advisory_lock(&mut admission_gate, ADMISSION_GATE).await?;
        drop(admission_gate);
        let late_outcome = tokio::time::timeout(Duration::from_secs(10), late).await???;
        assert!(!late_outcome.idempotent_replay);
        let late_t = late_outcome.memory_id.into_inner();

        let erase_result = tokio::time::timeout(Duration::from_secs(10), erase).await??;
        assert!(
            erase_result.is_ok(),
            "the erase must wait for the in-flight admission, not refuse: {erase_result:?}"
        );

        // The late Fact committed before the erase took its fence, so it is
        // inside the scope the erase then selected and goes with it. The
        // writer is still all-or-nothing: it committed whole, and was then
        // erased whole.
        let memories: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.memory WHERE t IN ($1, $2)",
        )
        .bind(seed_t)
        .bind(late_t)
        .fetch_one(pool)
        .await?;
        assert_eq!(memories, 0, "both hot Memory rows are erased");
        let keys: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.ingest_keys
              WHERE ingest_key IN ('seed-owner', 'late-owner')",
        )
        .fetch_one(pool)
        .await?;
        assert_eq!(keys, 0, "neither ingest key survives the erase");
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("owner erase vs late same-owner Fact failed");
}

/// The source fence is narrower than the owner fence. A same-source writer
/// already inside that shared lane must finish before source erase can acquire
/// its exclusive fence; the failed erase leaves both admissions and all
/// lifecycle evidence untouched.
#[tokio::test]
async fn source_erase_includes_a_fact_that_committed_before_its_fence() {
    const ADMISSION_GATE: i64 = 7_514_002;

    let (db_name, pg) = fresh_owner_erase_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pool = pg.pool_for_tests();
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let seed = pg
            .ingest_fact_atomic(&permit, &draft(Some(("source-race", "seed-source"))), None)
            .await?;
        let seed_t = seed.memory_id.into_inner();
        install_memory_insert_gate(pool, ADMISSION_GATE).await?;

        let mut admission_gate = hold_session_advisory_lock(pool, ADMISSION_GATE).await?;
        let late_pg = pg.clone();
        let late_permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let late = tokio::spawn(async move {
            late_pg
                .ingest_fact_atomic(
                    &late_permit,
                    &draft(Some(("source-race", "late-source"))),
                    None,
                )
                .await
        });
        wait_for_ungranted_advisory_key(pool, ADMISSION_GATE).await?;

        let erase_pg = pg.clone();
        let erase = tokio::spawn(async move {
            let auth = EraseAuthorization::new_for_tests(OwnerEraseTarget::PersonalSourceScope {
                user_id: owner.as_user(),
                source_id: SourceId::new("source-race"),
                drop_event_id: "source-late-fact".into(),
            });
            erase_pg
                .erase_personal_source_scope(
                    &auth,
                    owner.as_user(),
                    &SourceId::new("source-race"),
                    &contract_sidecar_tables(),
                )
                .await
        });
        wait_for_ungranted_advisory_label(pool, &source_fence_key(owner, "source-race")).await?;
        assert!(!erase.is_finished(), "the source fence must still be held");

        release_session_advisory_lock(&mut admission_gate, ADMISSION_GATE).await?;
        drop(admission_gate);
        let late_outcome = tokio::time::timeout(Duration::from_secs(10), late).await???;
        assert!(!late_outcome.idempotent_replay);
        let late_t = late_outcome.memory_id.into_inner();

        let erase_result = tokio::time::timeout(Duration::from_secs(10), erase).await??;
        assert!(
            erase_result.is_ok(),
            "the source erase must wait for the in-flight admission: {erase_result:?}"
        );

        let memories: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.memory WHERE t IN ($1, $2)",
        )
        .bind(seed_t)
        .bind(late_t)
        .fetch_one(pool)
        .await?;
        assert_eq!(memories, 0, "both same-source rows are erased");
        let keys: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.ingest_keys
              WHERE ingest_key IN ('seed-source', 'late-source')",
        )
        .fetch_one(pool)
        .await?;
        assert_eq!(keys, 0, "neither source ingest key survives the erase");
        // A hard erase records a kind witness per entity it removes, so both
        // the seed and the late admission are witnessed. Under the old
        // refuse-and-retry contract this count was 0 only because nothing was
        // erased at all.
        let witnesses: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.erased_pin_target
              WHERE t IN ($1, $2)",
        )
        .bind(seed_t)
        .bind(late_t)
        .fetch_one(pool)
        .await?;
        assert_eq!(witnesses, 2, "both erased same-source rows are witnessed");
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("source erase vs late same-source Fact failed");
}

/// Source-A erase holds its owner fence shared and source-A fence exclusive
/// while waiting on one selected lifecycle target. Source-B admission uses a
/// different source and handle, so it commits before the lifecycle gate is
/// released and survives the completed source-A erase.
#[tokio::test]
async fn source_erase_allows_different_source_admission_while_lifecycle_locked() {
    let (db_name, pg) = fresh_owner_erase_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pool = pg.pool_for_tests();
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let erased_admission = pg
            .ingest_fact_atomic(&permit, &draft(Some(("source-a", "key-a"))), None)
            .await?;
        let erased_t = erased_admission.memory_id.into_inner();

        let mut lifecycle_gate = pool.begin().await?;
        sqlx::query(
            "SELECT pg_advisory_xact_lock(hashtextextended('proxima-forget:' || $1::text, 0))",
        )
        .bind(erased_t)
        .execute(&mut *lifecycle_gate)
        .await?;

        let erase_pg = pg.clone();
        let erase = tokio::spawn(async move {
            let auth = EraseAuthorization::new_for_tests(OwnerEraseTarget::PersonalSourceScope {
                user_id: owner.as_user(),
                source_id: SourceId::new("source-a"),
                drop_event_id: "source-a-erase".into(),
            });
            erase_pg
                .erase_personal_source_scope(
                    &auth,
                    owner.as_user(),
                    &SourceId::new("source-a"),
                    &contract_sidecar_tables(),
                )
                .await
        });
        wait_for_ungranted_advisory_label(pool, &format!("proxima-forget:{erased_t}")).await?;

        let survivor_pg = pg.clone();
        let survivor_permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let survivor = tokio::spawn(async move {
            survivor_pg
                .ingest_fact_atomic(&survivor_permit, &draft(Some(("source-b", "key-b"))), None)
                .await
        });
        let surviving_admission =
            tokio::time::timeout(Duration::from_secs(10), survivor).await???;
        assert!(!surviving_admission.idempotent_replay);
        let surviving_t = surviving_admission.memory_id.into_inner();
        assert_ne!(erased_admission.handle, surviving_admission.handle);

        lifecycle_gate.commit().await?;
        let erase_result = tokio::time::timeout(Duration::from_secs(10), erase).await??;
        assert!(
            matches!(erase_result, Ok(OwnerEraseOutcome::Completed { .. })),
            "source-A erase should complete after its lifecycle gate opens: {erase_result:?}"
        );

        let erased_rows: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.memory WHERE t = $1")
                .bind(erased_t)
                .fetch_one(pool)
                .await?;
        assert_eq!(erased_rows, 0, "source-A's selected row is erased");
        let surviving_rows: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.memory WHERE t = $1")
                .bind(surviving_t)
                .fetch_one(pool)
                .await?;
        assert_eq!(surviving_rows, 1, "source-B admission survives");
        let erased_witness: Option<String> = sqlx::query_scalar(
            "SELECT kind::text FROM proxima_core.erased_pin_target WHERE t = $1",
        )
        .bind(erased_t)
        .fetch_optional(pool)
        .await?;
        assert_eq!(erased_witness.as_deref(), Some("fact"));
        let surviving_witness: Option<String> = sqlx::query_scalar(
            "SELECT kind::text FROM proxima_core.erased_pin_target WHERE t = $1",
        )
        .bind(surviving_t)
        .fetch_optional(pool)
        .await?;
        assert_eq!(surviving_witness, None, "source-B gets no erase witness");
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("source erase different-source barrier failed");
}

/// Transfer is an exclusive owner-fence participant. The update trigger pauses it
/// after the real transfer has acquired both owner fences and the series
/// lifecycle lock; owner erase then snapshots the old owner and queues its
/// exclusive fence. Releasing the trigger lets transfer commit, so erase
/// returns Retryable without deleting the destination's hot series.
#[tokio::test]
async fn owner_erase_excludes_a_memory_transferred_away_before_its_fence() {
    const TRANSFER_GATE: i64 = 7_514_004;

    let (db_name, pg) = fresh_owner_erase_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pool = pg.pool_for_tests();
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let destination = OwnerId::from(GroupId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let written = pg
            .ingest_fact_atomic(
                &permit,
                &draft(Some(("transfer-source", "transfer-key"))),
                None,
            )
            .await?;
        let t = written.memory_id.into_inner();
        install_transfer_update_gate(pool, TRANSFER_GATE).await?;
        let mut transfer_gate = hold_session_advisory_lock(pool, TRANSFER_GATE).await?;

        let transfer_pg = pg.clone();
        let transfer = tokio::spawn(async move {
            transfer_pg
                .transfer_to_owner(
                    &OwnerWritePermit::transfer_for_tests(owner, AccessKind::Fact, destination),
                    EntityId::Memory(proxima_core::MemoryId::new(t)),
                    destination,
                    &contract_sidecar_tables(),
                    &[],
                )
                .await
        });
        wait_for_ungranted_advisory_key(pool, TRANSFER_GATE).await?;

        let erase_pg = pg.clone();
        let erase = tokio::spawn(async move {
            let user_id = owner.as_user();
            let auth = EraseAuthorization::new_for_tests(OwnerEraseTarget::PersonalOwner {
                user_id,
                drop_event_id: "owner-transfer-race".into(),
            });
            erase_pg
                .erase_personal_owner(&auth, user_id, &contract_sidecar_tables())
                .await
        });
        wait_for_ungranted_advisory_label(pool, &owner_fence_key(owner)).await?;
        assert!(
            !erase.is_finished(),
            "the transfer must still hold its fence"
        );

        release_session_advisory_lock(&mut transfer_gate, TRANSFER_GATE).await?;
        drop(transfer_gate);
        let transferred = tokio::time::timeout(Duration::from_secs(10), transfer).await??;
        assert!(
            transferred?,
            "the transfer should commit after its trigger opens"
        );

        let erase_result = tokio::time::timeout(Duration::from_secs(10), erase).await??;
        // The transfer holds both endpoint fences when the erase queues, so
        // the erase waits and then selects a scope the Memory has already left.
        // It succeeds having erased nothing of the moved series.
        assert!(
            erase_result.is_ok(),
            "owner erase must complete once the transfer releases its fences: {erase_result:?}"
        );

        let moved_owner: Uuid =
            sqlx::query_scalar("SELECT owner_id FROM proxima_core.memory WHERE t = $1")
                .bind(t)
                .fetch_one(pool)
                .await?;
        assert_eq!(moved_owner, destination.into_inner());
        let destination_head: Uuid =
            sqlx::query_scalar("SELECT owner_id FROM proxima_core.memory_head WHERE handle = $1")
                .bind(written.handle)
                .fetch_one(pool)
                .await?;
        assert_eq!(destination_head, destination.into_inner());
        let witness: Option<String> = sqlx::query_scalar(
            "SELECT kind::text FROM proxima_core.erased_pin_target WHERE t = $1",
        )
        .bind(t)
        .fetch_optional(pool)
        .await?;
        assert_eq!(
            witness, None,
            "an erase ordered after the transfer leaves no witness"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("owner erase vs transfer barrier failed");
}

/// The destination half of transfer's two-owner fence is observable too. An
/// initially empty destination erase queues after transfer has minted the
/// destination owner and acquired its exclusive fence; transfer then commits the
/// moved series, making the destination erase's empty snapshot stale.
#[tokio::test]
async fn destination_erase_includes_a_memory_transferred_in_before_its_fence() {
    const TRANSFER_GATE: i64 = 7_514_006;

    let (db_name, pg) = fresh_owner_erase_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pool = pg.pool_for_tests();
        let source = OwnerId::from(UserId::new(Uuid::now_v7()));
        let destination = OwnerId::from(GroupId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(source, AccessKind::Fact);
        let written = pg
            .ingest_fact_atomic(
                &permit,
                &draft(Some(("destination-race", "destination-key"))),
                None,
            )
            .await?;
        let t = written.memory_id.into_inner();
        install_transfer_update_gate(pool, TRANSFER_GATE).await?;
        let mut transfer_gate = hold_session_advisory_lock(pool, TRANSFER_GATE).await?;

        let transfer_pg = pg.clone();
        let transfer = tokio::spawn(async move {
            transfer_pg
                .transfer_to_owner(
                    &OwnerWritePermit::transfer_for_tests(source, AccessKind::Fact, destination),
                    EntityId::Memory(proxima_core::MemoryId::new(t)),
                    destination,
                    &contract_sidecar_tables(),
                    &[],
                )
                .await
        });
        wait_for_ungranted_advisory_key(pool, TRANSFER_GATE).await?;

        let erase_pg = pg.clone();
        let erase = tokio::spawn(async move {
            let group_id = destination.as_group();
            let auth = EraseAuthorization::new_for_tests(OwnerEraseTarget::GroupOwner { group_id });
            erase_pg
                .erase_group_owner(&auth, group_id, &contract_sidecar_tables())
                .await
        });
        wait_for_ungranted_advisory_label(pool, &owner_fence_key(destination)).await?;
        assert!(
            !erase.is_finished(),
            "the destination erase must still be fenced"
        );

        release_session_advisory_lock(&mut transfer_gate, TRANSFER_GATE).await?;
        drop(transfer_gate);
        let transferred = tokio::time::timeout(Duration::from_secs(10), transfer).await??;
        assert!(
            transferred?,
            "the transfer should commit after its trigger opens"
        );

        let erase_result = tokio::time::timeout(Duration::from_secs(10), erase).await??;
        // The erase acquires the destination fence only after the transfer
        // commits, so the moved Memory is outside the scope it then selects.
        assert!(
            erase_result.is_ok(),
            "destination erase must complete after the transfer commits: {erase_result:?}"
        );
        // The transfer committed before the erase reached its fence, so the
        // arriving Memory is part of the destination scope the erase then
        // selected, and goes with it — head included.
        let rows: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.memory WHERE t = $1")
                .bind(t)
                .fetch_one(pool)
                .await?;
        assert_eq!(rows, 0, "the transferred-in Memory is erased");
        let heads: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.memory_head WHERE handle = $1",
        )
        .bind(written.handle)
        .fetch_one(pool)
        .await?;
        assert_eq!(heads, 0, "its head is erased with it");
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("destination erase vs transfer barrier failed");
}

/// A transfer also changes the destination's source scope. Holding both owner
/// boundaries exclusively makes the source-scope erase wait until the move
/// commits; it then selects a scope that already contains the arriving series
/// and erases it, rather than acting on a snapshot taken before the move.
#[tokio::test]
async fn destination_source_erase_includes_a_series_transferred_in_before_its_fence() {
    const TRANSFER_GATE: i64 = 7_514_007;

    let (db_name, pg) = fresh_owner_erase_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pool = pg.pool_for_tests();
        let source = OwnerId::from(UserId::new(Uuid::now_v7()));
        let destination_id = GroupId::new(Uuid::now_v7());
        let destination = OwnerId::from(destination_id);
        let permit = OwnerWritePermit::new_for_tests(source, AccessKind::Fact);
        let source_id = SourceId::new("destination-source-race");
        let written = pg
            .ingest_fact_atomic(
                &permit,
                &draft(Some((source_id.as_str(), "destination-source-key"))),
                None,
            )
            .await?;
        let t = written.memory_id.into_inner();
        install_transfer_update_gate(pool, TRANSFER_GATE).await?;
        let mut transfer_gate = hold_session_advisory_lock(pool, TRANSFER_GATE).await?;

        let transfer_pg = pg.clone();
        let transfer = tokio::spawn(async move {
            transfer_pg
                .transfer_to_owner(
                    &OwnerWritePermit::transfer_for_tests(source, AccessKind::Fact, destination),
                    EntityId::Memory(proxima_core::MemoryId::new(t)),
                    destination,
                    &contract_sidecar_tables(),
                    &[],
                )
                .await
        });
        wait_for_ungranted_advisory_key(pool, TRANSFER_GATE).await?;

        let erase_pg = pg.clone();
        let erase_source_id = source_id.clone();
        let erase = tokio::spawn(async move {
            let auth = EraseAuthorization::new_for_tests(OwnerEraseTarget::GroupSourceScope {
                group_id: destination_id,
                source_id: erase_source_id.clone(),
            });
            erase_pg
                .erase_group_source_scope(
                    &auth,
                    destination_id,
                    &erase_source_id,
                    &contract_sidecar_tables(),
                )
                .await
        });
        wait_for_ungranted_advisory_label(pool, &owner_fence_key(destination)).await?;
        assert!(
            !erase.is_finished(),
            "the destination source erase must wait at the transfer boundary"
        );

        release_session_advisory_lock(&mut transfer_gate, TRANSFER_GATE).await?;
        drop(transfer_gate);
        assert!(tokio::time::timeout(Duration::from_secs(10), transfer).await???);
        let erase_result = tokio::time::timeout(Duration::from_secs(10), erase).await??;
        assert!(
            erase_result.is_ok(),
            "destination source erase must complete after the transfer commits: {erase_result:?}"
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*)::bigint FROM proxima_core.memory WHERE t = $1",
            )
            .bind(t)
            .fetch_one(pool)
            .await?,
            0,
            "the series transferred into the erased source scope is erased"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("destination source erase vs transfer barrier failed");
}

/// Goal admission shares the owner fence just like Memory admission. This is
/// the compact owner-level proof that a Goal committing while a whole-owner
/// erase waits for its fence is inside that erase, together with the lifecycle
/// write-act Fact it minted — neither is left behind.
#[tokio::test]
async fn owner_erase_includes_a_goal_that_committed_before_its_fence() {
    const ADMISSION_GATE: i64 = 7_514_005;

    let (db_name, pg) = fresh_owner_erase_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pool = pg.pool_for_tests();
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let seed = pg
            .ingest_fact_atomic(&permit, &draft(Some(("goal-seed", "goal-seed-key"))), None)
            .await?;
        let seed_t = seed.memory_id.into_inner();
        install_goal_insert_gate(pool, ADMISSION_GATE).await?;

        let mut admission_gate = hold_session_advisory_lock(pool, ADMISSION_GATE).await?;
        let goal_pool = pool.clone();
        let goal_owner = owner;
        let goal = tokio::spawn(async move {
            let mut tx = goal_pool.begin().await?;
            let outcome = write_goal(
                &mut tx,
                &goal_owner,
                proxima_storage_pg::test_fixtures::writer(),
                &goal_draft("late-goal-request"),
                1,
            )
            .await?;
            tx.commit().await?;
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>(outcome)
        });
        wait_for_ungranted_advisory_key(pool, ADMISSION_GATE).await?;

        let erase_pg = pg.clone();
        let erase = tokio::spawn(async move {
            let user_id = owner.as_user();
            let auth = EraseAuthorization::new_for_tests(OwnerEraseTarget::PersonalOwner {
                user_id,
                drop_event_id: "owner-late-goal".into(),
            });
            erase_pg
                .erase_personal_owner(&auth, user_id, &contract_sidecar_tables())
                .await
        });
        wait_for_ungranted_advisory_label(pool, &owner_fence_key(owner)).await?;

        release_session_advisory_lock(&mut admission_gate, ADMISSION_GATE).await?;
        drop(admission_gate);
        let goal_outcome = tokio::time::timeout(Duration::from_secs(10), goal).await??;
        let goal_outcome =
            goal_outcome.map_err(|error| std::io::Error::other(error.to_string()))?;
        let goal_t = goal_outcome.t;
        let write_act_t = goal_outcome
            .write_act_t
            .expect("the Goal admission must return its lifecycle write-act");

        let erase_result = tokio::time::timeout(Duration::from_secs(10), erase).await??;
        assert!(
            erase_result.is_ok(),
            "the erase must wait for the in-flight Goal write: {erase_result:?}"
        );

        // The Goal and its lifecycle write-act Fact commit together under the
        // shared owner fence, so the erase sees both or neither. Here it saw
        // both, and took both.
        let goal_rows: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.goal WHERE t = $1")
                .bind(goal_t)
                .fetch_one(pool)
                .await?;
        assert_eq!(goal_rows, 0, "the late Goal is erased");
        let seed_rows: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.memory WHERE t = $1")
                .bind(seed_t)
                .fetch_one(pool)
                .await?;
        assert_eq!(seed_rows, 0, "the preexisting Memory is erased");
        let write_act_rows: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.memory WHERE t = $1")
                .bind(write_act_t)
                .fetch_one(pool)
                .await?;
        assert_eq!(
            write_act_rows, 0,
            "the Goal's write-act Fact is erased with it"
        );
        let witnesses: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.erased_pin_target
              WHERE t IN ($1, $2, $3)",
        )
        .bind(seed_t)
        .bind(goal_t)
        .bind(write_act_t)
        .fetch_one(pool)
        .await?;
        assert_eq!(
            witnesses, 3,
            "the seed, the late Goal and its write-act Fact are each witnessed"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("owner erase vs late Goal barrier failed");
}
