//! Owner erase against blank `0001_v008.sql`.
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
    AccessKind, ColdObjectStore, EntityId, GroupId, OwnerRef, SchemaId, SchemaVersion, SourceId,
    StorageError, UserId,
};
use proxima_pg_testkit::{create_db, db_url, drop_db};
use proxima_storage_pg::verbs::forget::MemoryColdStore;
use proxima_storage_pg::verbs::goal_timeseries::{GoalWriteCommand, write_goal};
use proxima_storage_pg::verbs::wake_timeseries::{
    WakeConfigDraft, WakeTriggerKind, insert_wake_config, write_armed_goal,
};
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
const MAPPING_TABLE: &str = "proxima_core.test_citation_mapping_v1";

#[derive(Default)]
struct RefusingDeleteCold {
    inner: MemoryColdStore,
}

#[async_trait::async_trait]
impl ColdObjectStore for RefusingDeleteCold {
    async fn put(&self, key: &str, bytes: &[u8]) -> Result<(), StorageError> {
        self.inner.put(key, bytes).await
    }

    async fn get(&self, key: &str) -> Result<Vec<u8>, StorageError> {
        self.inner.get(key).await
    }

    async fn delete(&self, _key: &str) -> Result<(), StorageError> {
        Err(StorageError::Unavailable("delete refused".into()))
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
        citation(MAPPING_TABLE, "citation_mapping_id"),
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
        "CREATE TABLE proxima_core.test_citation_mapping_v1 (
             citation_mapping_id uuid PRIMARY KEY REFERENCES proxima_core.blob (blob_id),
             page_from integer NOT NULL
         )",
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// One cited blob of `owner`, with an upload record and both citation sidecar
/// rows hanging off it, cited by one Fact in `source`.
async fn cite_blob(
    pg: &PgStorage,
    owner: OwnerRef,
    source: Option<(&str, &str)>,
    seed: u8,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let pool = pg.pool_for_tests();
    // `blob.owner_id` references `owners`, which ordinary writes materialize.
    sqlx::query(
        "INSERT INTO proxima_core.owners (owner_id, kind)
         VALUES ($1, 'personal') ON CONFLICT (owner_id) DO NOTHING",
    )
    .bind(owner.stored_owner_id())
    .execute(pool)
    .await?;
    let blob_id: Uuid = sqlx::query_scalar(
        "INSERT INTO proxima_core.blob (owner_id, schema_id, content_hash)
         VALUES ($1, 'core/bytes-v1', $2)
         RETURNING blob_id",
    )
    .bind(owner.stored_owner_id())
    .bind(vec![seed; 32])
    .fetch_one(pool)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.blob_uploads
             (owner_id, bucket, object_key, filename, mime, expected_byte_len,
              status, blob_id, sha256, expires_at, completed_at)
         VALUES ($1, 'bucket', $2, 'cited.pdf', 'application/pdf', 1,
                 'completed', $3, $4, now() + interval '1 hour', now())",
    )
    .bind(owner.stored_owner_id())
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
        "INSERT INTO proxima_core.test_citation_mapping_v1 (citation_mapping_id, page_from)
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
    owner: OwnerRef,
    seed: u8,
) -> Result<(Uuid, String), Box<dyn std::error::Error>> {
    let blob_id: Uuid = sqlx::query_scalar(
        "INSERT INTO proxima_core.blob (owner_id, schema_id, content_hash)
         VALUES ($1, 'core/bytes-v1', $2)
         RETURNING blob_id",
    )
    .bind(owner.stored_owner_id())
    .bind(vec![seed; 32])
    .fetch_one(pool)
    .await?;
    let object_key = format!("objects/{seed}");
    sqlx::query(
        "INSERT INTO proxima_core.blob_uploads
             (owner_id, bucket, object_key, filename, mime, expected_byte_len,
              status, blob_id, sha256, expires_at, completed_at)
         VALUES ($1, 'bucket', $2, 'unreferenced.pdf', 'application/pdf', 1,
                 'completed', $3, $4, now() + interval '1 hour', now())",
    )
    .bind(owner.stored_owner_id())
    .bind(&object_key)
    .bind(blob_id)
    .bind(vec![seed; 32])
    .execute(pool)
    .await?;
    Ok((blob_id, object_key))
}

/// Every row keyed on one blob: the blob itself, its upload record, and both
/// citation sidecar rows. `(blob, upload, cited_object, citation_mapping)`.
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
             (SELECT count(*)::bigint FROM proxima_core.test_citation_mapping_v1
               WHERE citation_mapping_id = $1)",
    )
    .bind(blob_id)
    .fetch_one(pool)
    .await
}

fn wake_draft(prompt: &str) -> WakeConfigDraft {
    WakeConfigDraft {
        trigger_kind: WakeTriggerKind::FactSchema,
        trigger_schema_id: Some("core/test-fact-v1".into()),
        trigger_t: None,
        tool_ids: vec!["core.remember".into()],
        prompt: prompt.to_owned(),
        hard_memory_t: vec![],
    }
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

fn embed_literal() -> String {
    format!(
        "[{}]",
        std::iter::once("1")
            .chain(std::iter::repeat_n("0", 1023))
            .collect::<Vec<_>>()
            .join(",")
    )
}

fn owner_kind(owner: OwnerRef) -> &'static str {
    match owner {
        OwnerRef::Personal(_) => "personal",
        OwnerRef::Group(_) => "group",
    }
}

fn owner_fence_key(owner: OwnerRef) -> String {
    format!(
        "proxima-owner-fence:{}:{}",
        owner_kind(owner),
        owner.stored_owner_id()
    )
}

fn source_fence_key(owner: OwnerRef, source: &str) -> String {
    format!(
        "proxima-source-fence:{}:{}:{source}",
        owner_kind(owner),
        owner.stored_owner_id()
    )
}

/// Wait for the exact advisory lock named by a production fence. Looking at
/// all advisory waiters is racy when another test happens to be busy; the
/// `pg_locks` split representation lets this probe identify only this key.
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
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let pg = PgStorage::connect(&db_url(&db_name))
        .await
        .expect("connect");
    pg.run_migrations().await.expect("migrate");
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
        state: GoalState::Active,
        request_id: request_id.into(),
        close_fact_t: None,
        assignment_t: None,
        dependency_t: vec![],
        evidence_t: vec![],
        wake_id: None,
        mint_write_act: true,
        write_act_t: None,
    }
}

#[tokio::test]
async fn erase_personal_owner_drops_memory_keys_and_embeddings() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let pool = pg.pool_for_tests();

        let user = UserId::new(Uuid::now_v7());
        let owner = OwnerRef::Personal(user);
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let written = pg
            .ingest_fact_atomic(&permit, &draft(Some(("src", "k1"))), None)
            .await?;
        let t = written.memory_id.into_inner();
        sqlx::query(
            "INSERT INTO proxima_core.embeddings
                (entity_id, model_id, embedding_version, vec, owner_id)
             VALUES ($1, 'test-embed', 1, $2::vector, $3)",
        )
        .bind(t)
        .bind(embed_literal())
        .bind(owner.stored_owner_id())
        .execute(pool)
        .await?;
        sqlx::query(
            "INSERT INTO proxima_core.embedding_heads
                (entity_id, model_id, embedding_version, owner_id)
             VALUES ($1, 'test-embed', 1, $2)",
        )
        .bind(t)
        .bind(owner.stored_owner_id())
        .execute(pool)
        .await?;

        let other = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let other_permit = OwnerWritePermit::new_for_tests(other, AccessKind::Fact);
        let other_written = pg
            .ingest_fact_atomic(&other_permit, &draft(Some(("src", "k-other"))), None)
            .await?;

        let auth = EraseAuthorization::new_for_tests(OwnerEraseTarget::PersonalOwner {
            user_id: user,
            drop_event_id: "test-drop".into(),
        });
        let outcome = pg
            .erase_personal_owner(&auth, user, false, &contract_sidecar_tables())
            .await?;
        let OwnerEraseOutcome::Completed { counts, .. } = outcome else {
            panic!("expected completed erase, got {outcome:?}");
        };
        assert_eq!(counts.get("memories"), 1);
        assert_eq!(counts.get("embeddings"), 2);
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

#[tokio::test]
async fn erase_personal_owner_destroys_cooled_and_gcs_content() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let pool = pg.pool_for_tests();
        let user = UserId::new(Uuid::now_v7());
        let owner = OwnerRef::Personal(user);
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let written = pg.ingest_fact_atomic(&permit, &draft(Some(("src", "k-cooled"))), None).await?;
        let t = written.memory_id.into_inner();
        MemoryAuthoringPort::forget_memory(&pg, &permit, written.memory_id).await?;
        let keys_after_forget: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.ingest_keys WHERE ingest_key = 'k-cooled'",
        )
        .fetch_one(pool)
        .await?;
        assert_eq!(keys_after_forget, 1, "forget leaves ingest_keys");
        let cooled_before: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.cooled WHERE t = $1")
                .bind(t)
                .fetch_one(pool)
                .await?;
        assert_eq!(cooled_before, 1);

        let auth = EraseAuthorization::new_for_tests(OwnerEraseTarget::PersonalOwner {
            user_id: user,
            drop_event_id: "test-drop-cooled".into(),
        });
        let outcome = pg
            .erase_personal_owner(&auth, user, false, &contract_sidecar_tables())
            .await?;
        assert!(
            matches!(outcome, OwnerEraseOutcome::Completed { .. }),
            "got {outcome:?}"
        );
        let cooled_after: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.cooled WHERE t = $1")
                .bind(t)
                .fetch_one(pool)
                .await?;
        assert_eq!(cooled_after, 0, "owner erase must delete cooled stubs");
        let content: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.content c
              WHERE NOT EXISTS (SELECT 1 FROM proxima_core.memory m WHERE m.content_id = c.content_id)
                AND NOT EXISTS (SELECT 1 FROM proxima_core.cooled k WHERE k.content_id = c.content_id)",
        )
        .fetch_one(pool)
        .await?;
        assert_eq!(content, 0, "unreferenced Content must be GC'd");
        let keys_after_erase: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.ingest_keys WHERE ingest_key = 'k-cooled'",
        )
        .fetch_one(pool)
        .await?;
        assert_eq!(
            keys_after_erase, 0,
            "owner erase must delete ingest_keys of cooled facts"
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
            "owner erase hard-deletes a cooled Memory and records its kind witness"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("owner erase of cooled failed");
}

/// `wake_config` holds a free-text `prompt`, the armed `tool_ids`, and the
/// hard-context `hard_memory_t` — owner-authored content. Erase deletes the
/// referencing `goal` rows and never deletes the `owners` row, so the
/// `ON DELETE RESTRICT` FK never fires: without an explicit statement the
/// prompt text outlived the owner's erasure silently.
#[tokio::test]
async fn erase_personal_owner_destroys_wake_config() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let pool = pg.pool_for_tests();
        let user = UserId::new(Uuid::now_v7());
        let owner = OwnerRef::Personal(user);

        let mut tx = pool.begin().await?;
        let armed = insert_wake_config(&mut tx, &owner, &wake_draft("armed prompt")).await?;
        let idle = insert_wake_config(&mut tx, &owner, &wake_draft("idle prompt")).await?;
        let goal_handle =
            write_armed_goal(&mut tx, &owner, "armed goal", "wake-owner", armed).await?;
        let goal_t: Uuid = sqlx::query_scalar("SELECT t FROM proxima_core.goal WHERE handle = $1")
            .bind(goal_handle)
            .fetch_one(&mut *tx)
            .await?;
        tx.commit().await?;

        let other = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let mut tx = pool.begin().await?;
        let other_wake = insert_wake_config(&mut tx, &other, &wake_draft("other prompt")).await?;
        tx.commit().await?;

        let auth = EraseAuthorization::new_for_tests(OwnerEraseTarget::PersonalOwner {
            user_id: user,
            drop_event_id: "test-drop-wake".into(),
        });
        let outcome = pg
            .erase_personal_owner(&auth, user, false, &contract_sidecar_tables())
            .await?;
        let OwnerEraseOutcome::Completed { counts, .. } = outcome else {
            panic!("expected completed erase, got {outcome:?}");
        };
        assert_eq!(
            counts.get("wake_configs"),
            2,
            "both wake rows are owner-authored"
        );

        let remaining: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.wake_config WHERE owner_id = $1",
        )
        .bind(owner.stored_owner_id())
        .fetch_one(pool)
        .await?;
        assert_eq!(
            remaining, 0,
            "owner erase must leave no wake_config row for the owner"
        );
        for gone in [armed, idle] {
            let rows: i64 = sqlx::query_scalar(
                "SELECT count(*)::bigint FROM proxima_core.wake_config WHERE wake_id = $1",
            )
            .bind(gone)
            .fetch_one(pool)
            .await?;
            assert_eq!(rows, 0);
        }

        let untouched: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.wake_config WHERE wake_id = $1",
        )
        .bind(other_wake)
        .fetch_one(pool)
        .await?;
        assert_eq!(untouched, 1, "another owner's wake config stays");
        let witness: Option<String> = sqlx::query_scalar(
            "SELECT kind::text FROM proxima_core.erased_pin_target WHERE t = $1",
        )
        .bind(goal_t)
        .fetch_optional(pool)
        .await?;
        assert_eq!(
            witness.as_deref(),
            Some("goal"),
            "owner erase hard-deletes the armed Goal and records a goal witness"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("owner wake_config erase failed");
}

/// `wake_config` has no source attribution, so source scope cannot select even
/// an unarmed row.
#[tokio::test]
async fn erase_source_scope_keeps_all_wake_configs() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let pool = pg.pool_for_tests();
        let user = UserId::new(Uuid::now_v7());
        let owner = OwnerRef::Personal(user);
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        pg.ingest_fact_atomic(&permit, &draft(Some(("src-wake", "k-wake"))), None)
            .await?;

        let mut tx = pool.begin().await?;
        let armed = insert_wake_config(&mut tx, &owner, &wake_draft("armed prompt")).await?;
        let orphan = insert_wake_config(&mut tx, &owner, &wake_draft("orphan prompt")).await?;
        write_armed_goal(&mut tx, &owner, "armed goal", "wake-src", armed).await?;
        tx.commit().await?;

        let auth = EraseAuthorization::new_for_tests(OwnerEraseTarget::PersonalSourceScope {
            user_id: user,
            source_id: SourceId::new("src-wake"),
            drop_event_id: "test-drop-wake-src".into(),
        });
        let outcome = pg
            .erase_personal_source_scope(
                &auth,
                user,
                &SourceId::new("src-wake"),
                &contract_sidecar_tables(),
            )
            .await?;
        let OwnerEraseOutcome::Completed { counts, .. } = outcome else {
            panic!("expected completed erase, got {outcome:?}");
        };
        assert_eq!(
            counts.get("wake_configs"),
            0,
            "source erase owns no wake rows"
        );

        let armed_rows: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.wake_config WHERE wake_id = $1",
        )
        .bind(armed)
        .fetch_one(pool)
        .await?;
        assert_eq!(
            armed_rows, 1,
            "a wake a surviving goal arms must survive a source-scope erase"
        );
        let orphan_rows: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.wake_config WHERE wake_id = $1",
        )
        .bind(orphan)
        .fetch_one(pool)
        .await?;
        assert_eq!(
            orphan_rows, 1,
            "an unarmed wake row has no source attribution and must survive"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("source-scope wake_config erase failed");
}

/// A `cold.delete` inside the erase transaction leaves `cooled` rows naming
/// destroyed objects when the transaction rolls back. The keys are marked
/// pending in-transaction and destroyed after the commit.
#[tokio::test]
async fn erase_personal_owner_purges_cold_objects_after_commit() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let cold = Arc::new(MemoryColdStore::default());
        let pg = PgStorage::connect(&url).await?.with_cold(cold.clone());
        pg.run_migrations().await?;
        let pool = pg.pool_for_tests();
        let user = UserId::new(Uuid::now_v7());
        let owner = OwnerRef::Personal(user);
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let written = pg
            .ingest_fact_atomic(&permit, &draft(Some(("src", "k-cold"))), None)
            .await?;
        let t = written.memory_id.into_inner();
        MemoryAuthoringPort::forget_memory(&pg, &permit, written.memory_id).await?;
        let key: String =
            sqlx::query_scalar("SELECT object_key FROM proxima_core.cooled WHERE t = $1")
                .bind(t)
                .fetch_one(pool)
                .await?;
        assert!(cold.get(&key).await.is_ok(), "forget wrote the cold object");

        let auth = EraseAuthorization::new_for_tests(OwnerEraseTarget::PersonalOwner {
            user_id: user,
            drop_event_id: "test-drop-cold".into(),
        });
        let outcome = pg
            .erase_personal_owner(&auth, user, false, &contract_sidecar_tables())
            .await?;
        assert!(
            matches!(outcome, OwnerEraseOutcome::Completed { .. }),
            "got {outcome:?}"
        );
        assert!(
            matches!(cold.get(&key).await, Err(StorageError::NotFound)),
            "the committed erase destroys the cold object"
        );
        let pending: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.cold_purge_pending WHERE object_key = $1",
        )
        .bind(&key)
        .fetch_one(pool)
        .await?;
        assert_eq!(pending, 0, "a destroyed object clears its pending mark");
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("owner erase cold purge failed");
}

#[tokio::test]
async fn failed_cold_purge_is_attributed_and_bounded_retry_clears_audit() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let cold = Arc::new(RefusingDeleteCold::default());
        let pg = PgStorage::connect(&url).await?.with_cold(cold.clone());
        pg.run_migrations().await?;
        let pool = pg.pool_for_tests();
        let user = UserId::new(Uuid::now_v7());
        let owner = OwnerRef::Personal(user);
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let written = pg.ingest_fact_atomic(&permit, &draft(None), None).await?;
        MemoryAuthoringPort::forget_memory(&pg, &permit, written.memory_id).await?;

        let auth = EraseAuthorization::new_for_tests(OwnerEraseTarget::PersonalOwner {
            user_id: user,
            drop_event_id: "test-drop-retry".into(),
        });
        let outcome = pg
            .erase_personal_owner(&auth, user, false, &contract_sidecar_tables())
            .await?;
        assert!(matches!(
            outcome,
            OwnerEraseOutcome::Completed {
                cold_object_purge_pending: true,
                cited_object_purge_pending: false,
                ..
            }
        ));
        // The queue IS the debt: outstanding debt is a row count, which cannot
        // disagree with itself the way a queue row and a mirrored boolean on a
        // journal row can.
        assert_eq!(pending_debts(pool).await?, 1);
        sqlx::query(
            "INSERT INTO proxima_core.cold_purge_pending (object_key, owner_id)
             VALUES ('cold/test-second-debt', $1)",
        )
        .bind(owner.stored_owner_id())
        .execute(pool)
        .await?;

        let failed_retry = pg
            .retry_cold_object_purges(ColdPurgeRetryOptions {
                batch_size: 1,
                dry_run: false,
            })
            .await?;
        assert_eq!(
            (
                failed_retry.selected,
                failed_retry.purged,
                failed_retry.failed
            ),
            (1, 0, 1)
        );
        assert_eq!(failed_retry.remaining, 2);

        let retry_pg = PgStorage::connect(&url)
            .await?
            .with_cold(Arc::new(MemoryColdStore::default()));
        let dry_run = retry_pg
            .retry_cold_object_purges(ColdPurgeRetryOptions {
                batch_size: 1,
                dry_run: true,
            })
            .await?;
        assert_eq!(
            (dry_run.selected, dry_run.purged, dry_run.remaining),
            (1, 0, 2)
        );
        let first = retry_pg
            .retry_cold_object_purges(ColdPurgeRetryOptions {
                batch_size: 1,
                dry_run: false,
            })
            .await?;
        assert_eq!((first.selected, first.purged, first.failed), (1, 1, 0));
        assert_eq!(first.remaining, 1);
        assert_eq!(
            pending_debts(pool).await?,
            1,
            "the first of two keys clears its own debt and no more"
        );
        let second = retry_pg
            .retry_cold_object_purges(ColdPurgeRetryOptions {
                batch_size: 1,
                dry_run: false,
            })
            .await?;
        assert_eq!((second.selected, second.purged, second.failed), (1, 1, 0));
        assert_eq!(second.remaining, 0);
        assert_eq!(pending_debts(pool).await?, 0);
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("cold purge retry test failed");
}

/// The rollback half of the same rule, on the bulk path. An outside owner's Goal
/// arming this owner's wake row makes the wake deletion trip the RESTRICT FK
/// *after* the cooled rows are gone from the transaction, so the whole erase
/// aborts. Nothing may have been destroyed in the object store by then: the
/// `cooled` locator is back, so its bytes must be too.
#[tokio::test]
async fn an_aborted_owner_erase_keeps_the_cold_object_and_its_locator() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let cold = Arc::new(MemoryColdStore::default());
        let pg = PgStorage::connect(&url).await?.with_cold(cold.clone());
        pg.run_migrations().await?;
        let pool = pg.pool_for_tests();
        let user = UserId::new(Uuid::now_v7());
        let owner = OwnerRef::Personal(user);
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let written = pg
            .ingest_fact_atomic(&permit, &draft(Some(("src", "k-abort"))), None)
            .await?;
        let t = written.memory_id.into_inner();
        MemoryAuthoringPort::forget_memory(&pg, &permit, written.memory_id).await?;
        let key: String =
            sqlx::query_scalar("SELECT object_key FROM proxima_core.cooled WHERE t = $1")
                .bind(t)
                .fetch_one(pool)
                .await?;

        let outsider = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let outsider_permit = OwnerWritePermit::new_for_tests(outsider, AccessKind::Fact);
        pg.ingest_fact_atomic(&outsider_permit, &draft(None), None)
            .await?;
        let mut tx = pool.begin().await?;
        let wake = insert_wake_config(&mut tx, &owner, &wake_draft("held prompt")).await?;
        write_armed_goal(&mut tx, &outsider, "outsider goal", "held", wake).await?;
        tx.commit().await?;

        let auth = EraseAuthorization::new_for_tests(OwnerEraseTarget::PersonalOwner {
            user_id: user,
            drop_event_id: "test-drop-abort".into(),
        });
        let err = pg
            .erase_personal_owner(&auth, user, false, &contract_sidecar_tables())
            .await
            .expect_err("the RESTRICT FK aborts the erase");
        assert!(err.to_string().contains("wake_config"), "got: {err}");

        assert!(
            cold.get(&key).await.is_ok(),
            "an aborted erase may not have destroyed the object"
        );
        let stub: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.cooled WHERE t = $1")
                .bind(t)
                .fetch_one(pool)
                .await?;
        assert_eq!(stub, 1, "the locator is back, so the bytes must be too");
        let pending: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.cold_purge_pending WHERE object_key = $1",
        )
        .bind(&key)
        .fetch_one(pool)
        .await?;
        assert_eq!(pending, 0, "the pending mark rolls back with the erase");
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
#[tokio::test]
async fn erase_personal_owner_destroys_blobs_uploads_and_citation_sidecars() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let pool = pg.pool_for_tests();
        create_citation_sidecar_tables(pool).await?;
        let user = UserId::new(Uuid::now_v7());
        let owner = OwnerRef::Personal(user);
        let blob = cite_blob(&pg, owner, Some(("src", "k-blob")), 3).await?;
        // A pending upload names no blob: owner data attributable to no source.
        sqlx::query(
            "INSERT INTO proxima_core.blob_uploads
                 (owner_id, bucket, object_key, filename, mime, expected_byte_len, expires_at)
             VALUES ($1, 'bucket', 'pending/1', 'draft.pdf', 'application/pdf', 1,
                     now() + interval '1 hour')",
        )
        .bind(owner.stored_owner_id())
        .execute(pool)
        .await?;

        let neighbour = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let neighbour_blob = cite_blob(&pg, neighbour, Some(("src", "k-other")), 5).await?;

        let auth = EraseAuthorization::new_for_tests(OwnerEraseTarget::PersonalOwner {
            user_id: user,
            drop_event_id: "test-drop-blob".into(),
        });
        let outcome = pg
            .erase_personal_owner(&auth, user, false, &citation_surfaces())
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
        .bind(owner.stored_owner_id())
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

/// Source scope can delete only blobs captured from its selected admissions.
/// Surviving hot or cooled citations protect shared blobs, while an unrelated
/// unreferenced completed upload is outside the source scope entirely.
#[tokio::test]
async fn erase_source_scope_deletes_only_unshared_selected_blobs_and_objects() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let cold = Arc::new(MemoryColdStore::default());
        let pg = PgStorage::connect(&url).await?.with_cold(cold.clone());
        pg.run_migrations().await?;
        let pool = pg.pool_for_tests();
        create_citation_sidecar_tables(pool).await?;
        let user = UserId::new(Uuid::now_v7());
        let owner = OwnerRef::Personal(user);
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
        let shared_cold_key: String =
            sqlx::query_scalar("SELECT object_key FROM proxima_core.cooled WHERE t = $1")
                .bind(shared_survivor.memory_id.into_inner())
                .fetch_one(pool)
                .await?;
        cold.put("objects/8", b"shared cited object").await?;
        cold.put("objects/9", b"target cited object").await?;
        cold.put(&unrelated_key, b"unrelated object").await?;
        sqlx::query(
            "INSERT INTO proxima_core.blob_uploads
                 (owner_id, bucket, object_key, filename, mime, expected_byte_len, expires_at)
             VALUES ($1, 'bucket', 'pending/2', 'draft.pdf', 'application/pdf', 1,
                     now() + interval '1 hour')",
        )
        .bind(owner.stored_owner_id())
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
            "a surviving cooled citation must protect its shared blob"
        );
        assert_eq!(
            rows_for_blob(pool, unrelated).await?,
            (1, 1, 0, 0),
            "an unrelated unreferenced completed blob is not source-owned"
        );
        assert!(
            matches!(cold.get("objects/9").await, Err(StorageError::NotFound)),
            "the selected upload's exact object key must be purged"
        );
        assert!(
            cold.get("objects/8").await.is_ok(),
            "the shared blob's object must survive"
        );
        assert!(
            cold.get(&unrelated_key).await.is_ok(),
            "the unrelated upload's object must survive"
        );
        assert!(
            cold.get(&shared_cold_key).await.is_ok(),
            "the surviving cooled admission's object must survive"
        );
        let pending_uploads: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.blob_uploads
              WHERE owner_id = $1 AND blob_id IS NULL",
        )
        .bind(owner.stored_owner_id())
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

#[tokio::test]
async fn erase_group_owner_refuses_while_membership_rows_exist() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let pool = pg.pool_for_tests();

        let group = GroupId::new(Uuid::now_v7());
        let admin = UserId::new(Uuid::now_v7());
        pg.bootstrap_group_admin(group, admin, admin.into_inner())
            .await?;

        let owner = OwnerRef::Group(group);
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let written = pg
            .ingest_fact_atomic(&permit, &draft(Some(("src", "g1"))), None)
            .await?;
        let t = written.memory_id.into_inner();

        let auth =
            EraseAuthorization::new_for_tests(OwnerEraseTarget::GroupOwner { group_id: group });
        let outcome = pg
            .erase_group_owner(&auth, group, false, &contract_sidecar_tables())
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
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let pool = pg.pool_for_tests();

        let group = GroupId::new(Uuid::now_v7());
        let owner = OwnerRef::Group(group);
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let written = pg
            .ingest_fact_atomic(&permit, &draft(Some(("src", "g-empty"))), None)
            .await?;
        let t = written.memory_id.into_inner();
        let mut gtx = pool.begin().await?;
        let goal = write_goal(
            &mut gtx,
            &owner,
            &GoalWriteCommand {
                handle: None,
                schema_id: "core/task-goal-v1".into(),
                title: "erase me".into(),
                state: GoalState::Active,
                request_id: "erase-g".into(),
                close_fact_t: None,
                assignment_t: None,
                dependency_t: vec![],
                evidence_t: vec![],
                wake_id: None,
                mint_write_act: false,
                write_act_t: None,
            },
        )
        .await?;
        gtx.commit().await?;

        let auth =
            EraseAuthorization::new_for_tests(OwnerEraseTarget::GroupOwner { group_id: group });
        let outcome = pg
            .erase_group_owner(&auth, group, false, &contract_sidecar_tables())
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
async fn erase_source_scope_rewinds_head_to_remaining_t() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let pool = pg.pool_for_tests();
        let user = UserId::new(Uuid::now_v7());
        let owner = OwnerRef::Personal(user);
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let first = pg
            .ingest_fact_atomic(&permit, &draft(Some(("src-old", "k-old"))), None)
            .await?;
        let mut later = draft(Some(("src-new", "k-new")));
        later.handle = Some(first.handle);
        let second = pg.ingest_fact_atomic(&permit, &later, None).await?;
        assert_eq!(second.handle, first.handle);

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
async fn erase_source_scope_destroys_cooled_from_that_source() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let pool = pg.pool_for_tests();
        let user = UserId::new(Uuid::now_v7());
        let owner = OwnerRef::Personal(user);
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let target = pg
            .ingest_fact_atomic(&permit, &draft(Some(("src-cool", "k-cool"))), None)
            .await?;
        let other = pg
            .ingest_fact_atomic(&permit, &draft(Some(("src-keep", "k-keep"))), None)
            .await?;
        MemoryAuthoringPort::forget_memory(&pg, &permit, target.memory_id).await?;
        let foreign_owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let foreign_permit = OwnerWritePermit::new_for_tests(foreign_owner, AccessKind::Fact);
        let mut foreign_source = draft(Some(("foreign", "k-foreign")));
        foreign_source.refs = vec![target.memory_id.into_inner()];
        let foreign = pg
            .ingest_fact_atomic(&foreign_permit, &foreign_source, None)
            .await?;

        let auth = EraseAuthorization::new_for_tests(OwnerEraseTarget::PersonalSourceScope {
            user_id: user,
            source_id: SourceId::new("src-cool"),
            drop_event_id: "test-drop-cooled-src".into(),
        });
        let outcome = pg
            .erase_personal_source_scope(
                &auth,
                user,
                &SourceId::new("src-cool"),
                &contract_sidecar_tables(),
            )
            .await?;
        assert!(
            matches!(outcome, OwnerEraseOutcome::Completed { .. }),
            "got {outcome:?}"
        );
        let cooled: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.cooled WHERE t = $1")
                .bind(target.memory_id.into_inner())
                .fetch_one(pool)
                .await?;
        assert_eq!(
            cooled, 0,
            "source-scope erase must select cooled by source_id"
        );
        let kept: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.memory WHERE t = $1")
                .bind(other.memory_id.into_inner())
                .fetch_one(pool)
                .await?;
        assert_eq!(kept, 1, "other source must remain hot");
        let keys: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.ingest_keys WHERE ingest_key = 'k-cool'",
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
    result.expect("source-scope cooled erase failed");
}

/// Erasing a member does not shrink the groups it belonged to.
///
/// `group_memberships` is a `Surface` with `EraseRule::Never { why }`, and
/// nothing else in the tree asserts the position that declaration takes. The
/// differential harness cannot: it enumerates relations from
/// `information_schema` rather than from the contract, and its corpus never
/// writes a membership naming an erased member, so it reports a row here as
/// present either way. That is the difference between "we did not change this"
/// and "this is what we hold".
///
/// Two halves, because `EraseRule::Never` makes two claims. The row is still
/// there — a group does not silently lose a member row when the member is
/// dropped, and a host that must remove a departed user calls
/// `remove_group_member` first, deliberately. And the receipt says nothing
/// about it: the surface declares `CounterRule::Uncounted`, so the erase
/// reports no counter for this table at all rather than reporting zero.
#[tokio::test]
async fn erasing_a_member_leaves_the_memberships_that_name_it() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let pool = pg.pool_for_tests();

        let user = UserId::new(Uuid::now_v7());
        let owner = OwnerRef::Personal(user);
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

        let auth = EraseAuthorization::new_for_tests(OwnerEraseTarget::PersonalOwner {
            user_id: user,
            drop_event_id: "test-drop".into(),
        });
        let outcome = pg
            .erase_personal_owner(&auth, user, false, &contract_sidecar_tables())
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
            surviving, 1,
            "a membership is a relation between two owners; erasing one of \
             them does not make it the other's to lose"
        );

        assert!(
            counts.get("memories") >= 1,
            "the erase did its own work, so the survival above is not the \
             survival of a no-op"
        );

        // Tie the behaviour back to the declaration it witnesses, so that
        // changing the position without changing this test is a failure
        // rather than a silently stale assertion. `Uncounted` is why the
        // receipt above names no counter for this table: the erase's only
        // interaction with it is the abandonment precondition's COUNT.
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
    result.expect("erasing_a_member_leaves_the_memberships_that_name_it failed");
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
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
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
                user_id: match owner {
                    OwnerRef::Personal(user_id) => user_id,
                    OwnerRef::Group(_) => unreachable!(),
                },
                drop_event_id: "owner-late-fact".into(),
            });
            erase_pg
                .erase_personal_owner(
                    &auth,
                    match owner {
                        OwnerRef::Personal(user_id) => user_id,
                        OwnerRef::Group(_) => unreachable!(),
                    },
                    false,
                    &contract_sidecar_tables(),
                )
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
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
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
                user_id: match owner {
                    OwnerRef::Personal(user_id) => user_id,
                    OwnerRef::Group(_) => unreachable!(),
                },
                source_id: SourceId::new("source-race"),
                drop_event_id: "source-late-fact".into(),
            });
            erase_pg
                .erase_personal_source_scope(
                    &auth,
                    match owner {
                        OwnerRef::Personal(user_id) => user_id,
                        OwnerRef::Group(_) => unreachable!(),
                    },
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
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
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
                user_id: match owner {
                    OwnerRef::Personal(user_id) => user_id,
                    OwnerRef::Group(_) => unreachable!(),
                },
                source_id: SourceId::new("source-a"),
                drop_event_id: "source-a-erase".into(),
            });
            erase_pg
                .erase_personal_source_scope(
                    &auth,
                    match owner {
                        OwnerRef::Personal(user_id) => user_id,
                        OwnerRef::Group(_) => unreachable!(),
                    },
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
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let destination = OwnerRef::Group(GroupId::new(Uuid::now_v7()));
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
                    &OwnerWritePermit::new_for_tests(owner, AccessKind::Fact),
                    EntityId::Memory(proxima_core::MemoryId::new(t)),
                    destination,
                    &contract_sidecar_tables(),
                )
                .await
        });
        wait_for_ungranted_advisory_key(pool, TRANSFER_GATE).await?;

        let erase_pg = pg.clone();
        let erase = tokio::spawn(async move {
            let user_id = match owner {
                OwnerRef::Personal(user_id) => user_id,
                OwnerRef::Group(_) => unreachable!(),
            };
            let auth = EraseAuthorization::new_for_tests(OwnerEraseTarget::PersonalOwner {
                user_id,
                drop_event_id: "owner-transfer-race".into(),
            });
            erase_pg
                .erase_personal_owner(&auth, user_id, false, &contract_sidecar_tables())
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
        assert_eq!(moved_owner, destination.stored_owner_id());
        let destination_head: Uuid =
            sqlx::query_scalar("SELECT owner_id FROM proxima_core.memory_head WHERE handle = $1")
                .bind(written.handle)
                .fetch_one(pool)
                .await?;
        assert_eq!(destination_head, destination.stored_owner_id());
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
        let source = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let destination = OwnerRef::Group(GroupId::new(Uuid::now_v7()));
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
                    &OwnerWritePermit::new_for_tests(source, AccessKind::Fact),
                    EntityId::Memory(proxima_core::MemoryId::new(t)),
                    destination,
                    &contract_sidecar_tables(),
                )
                .await
        });
        wait_for_ungranted_advisory_key(pool, TRANSFER_GATE).await?;

        let erase_pg = pg.clone();
        let erase = tokio::spawn(async move {
            let group_id = match destination {
                OwnerRef::Group(group_id) => group_id,
                OwnerRef::Personal(_) => unreachable!(),
            };
            let auth = EraseAuthorization::new_for_tests(OwnerEraseTarget::GroupOwner { group_id });
            erase_pg
                .erase_group_owner(&auth, group_id, false, &contract_sidecar_tables())
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
        let source = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let destination_id = GroupId::new(Uuid::now_v7());
        let destination = OwnerRef::Group(destination_id);
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
                    &OwnerWritePermit::new_for_tests(source, AccessKind::Fact),
                    EntityId::Memory(proxima_core::MemoryId::new(t)),
                    destination,
                    &contract_sidecar_tables(),
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
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
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
            let outcome =
                write_goal(&mut tx, &goal_owner, &goal_draft("late-goal-request")).await?;
            tx.commit().await?;
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>(outcome)
        });
        wait_for_ungranted_advisory_key(pool, ADMISSION_GATE).await?;

        let erase_pg = pg.clone();
        let erase = tokio::spawn(async move {
            let user_id = match owner {
                OwnerRef::Personal(user_id) => user_id,
                OwnerRef::Group(_) => unreachable!(),
            };
            let auth = EraseAuthorization::new_for_tests(OwnerEraseTarget::PersonalOwner {
                user_id,
                drop_event_id: "owner-late-goal".into(),
            });
            erase_pg
                .erase_personal_owner(&auth, user_id, false, &contract_sidecar_tables())
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
