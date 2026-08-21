//! Compliance owner erase against blank `0001_v008.sql`.
#![allow(clippy::doc_markdown, clippy::too_many_lines)]

use std::sync::Arc;

use proxima_core::compliance::{
    ComplianceEraseOutcome, ComplianceEraseRefusal, ComplianceEraseTarget, EraseAuthorization,
    OwnerSurfaces,
};
use proxima_core::storage_ports::{
    ComplianceErasePort, MemoryAuthoringPort, OwnerMembershipAdminPort, OwnerWritePermit,
};
use proxima_core::verbs::fact_ingest::FactWriteCommand;
use proxima_core::verbs::goal_write::GoalState;
use proxima_core::{
    AccessKind, ColdObjectStore, GroupId, OwnerRef, SchemaId, SchemaVersion, SourceId,
    StorageError, UserId,
};
use proxima_pg_testkit::{create_db, db_url, drop_db};
use proxima_storage_pg::verbs::fact_ingest::ingest_fact_atomic;
use proxima_storage_pg::verbs::forget::MemoryColdStore;
use proxima_storage_pg::verbs::goal_timeseries::{GoalWriteCommand, write_goal};
use proxima_storage_pg::verbs::wake_timeseries::{
    WakeConfigDraft, WakeTriggerKind, insert_wake_config, write_armed_goal,
};
use proxima_storage_pg::{ColdPurgeRetryOptions, PgStorage};
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
fn citation_surfaces() -> proxima_core::compliance::OwnerSurfaces {
    use proxima_core::flavor::{
        EraseRule, ExportRule, ForgetRule, KeyShape, Surface, TransferRule,
    };
    const fn citation(table: &'static str, column: &'static str) -> Surface {
        Surface {
            table,
            key: KeyShape::BlobId { column },
            owner_columns: &[],
            transfer: TransferRule::StaysOnKey,
            erase: EraseRule::ByKey,
            export: ExportRule::Rows,
            forget: ForgetRule::Keep {
                why: "a citation outlives the Fact that made it",
            },
            lexical_language_column: None,
            counter: Some("sidecar_rows"),
            completeness: None,
        }
    }
    proxima_core::compliance::OwnerSurfaces::from_surfaces(vec![
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
    pool: &sqlx::PgPool,
    owner: OwnerRef,
    source: Option<(&str, &str)>,
    seed: u8,
) -> Result<Uuid, Box<dyn std::error::Error>> {
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
    ingest_fact_atomic(pool, &permit, &cited, None).await?;
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
        let written = ingest_fact_atomic(pool, &permit, &draft(Some(("src", "k1"))), None).await?;
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
        let other_written =
            ingest_fact_atomic(pool, &other_permit, &draft(Some(("src", "k-other"))), None).await?;

        let auth = EraseAuthorization::new_for_tests(ComplianceEraseTarget::PersonalOwner {
            user_id: user,
            drop_event_id: "test-drop".into(),
        });
        let outcome = pg
            .erase_personal_owner_if_drop_verified(&auth, user, false, &contract_sidecar_tables())
            .await?;
        let ComplianceEraseOutcome::Completed { counts, .. } = outcome else {
            panic!("expected completed erase, got {outcome:?}");
        };
        assert_eq!(counts.get("memories"), 1);
        assert_eq!(counts.get("embeddings"), 2);
        // `ingest_keys` declares `counter: Some("receipts")`, and the
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
        // declare, no more and no fewer. Fewer is what shipped — `sketches`
        // was tallied and then never read back — and more is what the old
        // struct had, with four fields (`edges`, `source_batches`,
        // `redacted_edge_targets`, `suppressed_keys`) reporting a structural
        // zero for things v0.0.8 does not have.
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
        assert_eq!(erased_heads, 0, "P3: owner erase deletes empty heads");
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
    result.expect("compliance erase failed");
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
        let written = ingest_fact_atomic(pool, &permit, &draft(Some(("src", "k-cooled"))), None).await?;
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

        let auth = EraseAuthorization::new_for_tests(ComplianceEraseTarget::PersonalOwner {
            user_id: user,
            drop_event_id: "test-drop-cooled".into(),
        });
        let outcome = pg
            .erase_personal_owner_if_drop_verified(&auth, user, false, &contract_sidecar_tables())
            .await?;
        assert!(
            matches!(outcome, ComplianceEraseOutcome::Completed { .. }),
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
        write_armed_goal(&mut tx, &owner, "armed goal", "wake-owner", armed).await?;
        tx.commit().await?;

        let other = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let mut tx = pool.begin().await?;
        let other_wake = insert_wake_config(&mut tx, &other, &wake_draft("other prompt")).await?;
        tx.commit().await?;

        let auth = EraseAuthorization::new_for_tests(ComplianceEraseTarget::PersonalOwner {
            user_id: user,
            drop_event_id: "test-drop-wake".into(),
        });
        let outcome = pg
            .erase_personal_owner_if_drop_verified(&auth, user, false, &contract_sidecar_tables())
            .await?;
        let ComplianceEraseOutcome::Completed { counts, .. } = outcome else {
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
        ingest_fact_atomic(pool, &permit, &draft(Some(("src-wake", "k-wake"))), None).await?;

        let mut tx = pool.begin().await?;
        let armed = insert_wake_config(&mut tx, &owner, &wake_draft("armed prompt")).await?;
        let orphan = insert_wake_config(&mut tx, &owner, &wake_draft("orphan prompt")).await?;
        write_armed_goal(&mut tx, &owner, "armed goal", "wake-src", armed).await?;
        tx.commit().await?;

        let auth = EraseAuthorization::new_for_tests(ComplianceEraseTarget::PersonalSourceScope {
            user_id: user,
            source_id: SourceId::new("src-wake"),
            drop_event_id: "test-drop-wake-src".into(),
        });
        let outcome = pg
            .erase_personal_source_scope_if_drop_verified(
                &auth,
                user,
                &SourceId::new("src-wake"),
                &contract_sidecar_tables(),
            )
            .await?;
        let ComplianceEraseOutcome::Completed { counts, .. } = outcome else {
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

/// The bulk erase used to `cold.delete` inside its own transaction: a rollback
/// after that point left `cooled` rows naming destroyed objects. The keys are
/// marked pending in-transaction and destroyed after the commit.
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
        let written =
            ingest_fact_atomic(pool, &permit, &draft(Some(("src", "k-cold"))), None).await?;
        let t = written.memory_id.into_inner();
        MemoryAuthoringPort::forget_memory(&pg, &permit, written.memory_id).await?;
        let key: String =
            sqlx::query_scalar("SELECT object_key FROM proxima_core.cooled WHERE t = $1")
                .bind(t)
                .fetch_one(pool)
                .await?;
        assert!(cold.get(&key).await.is_ok(), "forget wrote the cold object");

        let auth = EraseAuthorization::new_for_tests(ComplianceEraseTarget::PersonalOwner {
            user_id: user,
            drop_event_id: "test-drop-cold".into(),
        });
        let outcome = pg
            .erase_personal_owner_if_drop_verified(&auth, user, false, &contract_sidecar_tables())
            .await?;
        assert!(
            matches!(outcome, ComplianceEraseOutcome::Completed { .. }),
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
        let written = ingest_fact_atomic(pool, &permit, &draft(None), None).await?;
        MemoryAuthoringPort::forget_memory(&pg, &permit, written.memory_id).await?;

        let auth = EraseAuthorization::new_for_tests(ComplianceEraseTarget::PersonalOwner {
            user_id: user,
            drop_event_id: "test-drop-retry".into(),
        });
        let outcome = pg
            .erase_personal_owner_if_drop_verified(&auth, user, false, &contract_sidecar_tables())
            .await?;
        assert!(matches!(
            outcome,
            ComplianceEraseOutcome::Completed {
                cold_object_purge_pending: true,
                cited_object_purge_pending: false,
                ..
            }
        ));
        // The queue IS the debt. It used to also stamp the operation that
        // enqueued the row and mirror a `cold_object_purge_pending` boolean
        // onto a journal row, so two writes had to agree about one fact;
        // now the outstanding debt is a row count, which cannot disagree
        // with itself.
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
        let written =
            ingest_fact_atomic(pool, &permit, &draft(Some(("src", "k-abort"))), None).await?;
        let t = written.memory_id.into_inner();
        MemoryAuthoringPort::forget_memory(&pg, &permit, written.memory_id).await?;
        let key: String =
            sqlx::query_scalar("SELECT object_key FROM proxima_core.cooled WHERE t = $1")
                .bind(t)
                .fetch_one(pool)
                .await?;

        let outsider = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let outsider_permit = OwnerWritePermit::new_for_tests(outsider, AccessKind::Fact);
        ingest_fact_atomic(pool, &outsider_permit, &draft(None), None).await?;
        let mut tx = pool.begin().await?;
        let wake = insert_wake_config(&mut tx, &owner, &wake_draft("held prompt")).await?;
        write_armed_goal(&mut tx, &outsider, "outsider goal", "held", wake).await?;
        tx.commit().await?;

        let auth = EraseAuthorization::new_for_tests(ComplianceEraseTarget::PersonalOwner {
            user_id: user,
            drop_event_id: "test-drop-abort".into(),
        });
        let err = pg
            .erase_personal_owner_if_drop_verified(&auth, user, false, &contract_sidecar_tables())
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
        let blob = cite_blob(pool, owner, Some(("src", "k-blob")), 3).await?;
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
        let neighbour_blob = cite_blob(pool, neighbour, Some(("src", "k-other")), 5).await?;

        let auth = EraseAuthorization::new_for_tests(ComplianceEraseTarget::PersonalOwner {
            user_id: user,
            drop_event_id: "test-drop-blob".into(),
        });
        let outcome = pg
            .erase_personal_owner_if_drop_verified(&auth, user, false, &citation_surfaces())
            .await?;
        let ComplianceEraseOutcome::Completed { counts, .. } = outcome else {
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
        let kept = cite_blob(pool, owner, Some(("src-keep", "k-keep")), 7).await?;
        let shared = cite_blob(pool, owner, Some(("src-drop", "k-shared-drop")), 8).await?;
        let dropped = cite_blob(pool, owner, Some(("src-drop", "k-drop")), 9).await?;
        let (unrelated, unrelated_key) = completed_blob_upload(pool, owner, 10).await?;
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let mut shared_survivor = draft(Some(("src-keep", "k-shared-keep")));
        shared_survivor.blob_id = Some(shared);
        let shared_survivor = ingest_fact_atomic(pool, &permit, &shared_survivor, None).await?;
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
        let auth = EraseAuthorization::new_for_tests(ComplianceEraseTarget::PersonalSourceScope {
            user_id: user,
            source_id: source.clone(),
            drop_event_id: "test-drop-blob-scope".into(),
        });
        let outcome = pg
            .erase_personal_source_scope_if_drop_verified(
                &auth,
                user,
                &source,
                &citation_surfaces(),
            )
            .await?;
        let ComplianceEraseOutcome::Completed { counts, .. } = outcome else {
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
        let written = ingest_fact_atomic(pool, &permit, &draft(Some(("src", "g1"))), None).await?;
        let t = written.memory_id.into_inner();

        let auth = EraseAuthorization::new_for_tests(ComplianceEraseTarget::GroupOwner {
            group_id: group,
        });
        let outcome = pg
            .erase_group_owner_if_abandoned(&auth, group, false, &contract_sidecar_tables())
            .await?;
        let ComplianceEraseOutcome::Refused { reason, .. } = outcome else {
            panic!("expected OwnerNotAbandoned, got {outcome:?}");
        };
        assert_eq!(reason, ComplianceEraseRefusal::OwnerNotAbandoned);

        let remaining: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.memory WHERE t = $1")
                .bind(t)
                .fetch_one(pool)
                .await?;
        assert_eq!(remaining, 1, "live group must keep its memories");
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
        let written =
            ingest_fact_atomic(pool, &permit, &draft(Some(("src", "g-empty"))), None).await?;
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

        let auth = EraseAuthorization::new_for_tests(ComplianceEraseTarget::GroupOwner {
            group_id: group,
        });
        let outcome = pg
            .erase_group_owner_if_abandoned(&auth, group, false, &contract_sidecar_tables())
            .await?;
        let ComplianceEraseOutcome::Completed { counts, .. } = outcome else {
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
        assert_eq!(goal_heads, 0, "P3: owner erase deletes empty goal heads");
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
        let first =
            ingest_fact_atomic(pool, &permit, &draft(Some(("src-old", "k-old"))), None).await?;
        let mut later = draft(Some(("src-new", "k-new")));
        later.handle = Some(first.handle);
        let second = ingest_fact_atomic(pool, &permit, &later, None).await?;
        assert_eq!(second.handle, first.handle);

        let auth = EraseAuthorization::new_for_tests(ComplianceEraseTarget::PersonalSourceScope {
            user_id: user,
            source_id: SourceId::new("src-new"),
            drop_event_id: "test-drop".into(),
        });
        let outcome = pg
            .erase_personal_source_scope_if_drop_verified(
                &auth,
                user,
                &SourceId::new("src-new"),
                &contract_sidecar_tables(),
            )
            .await?;
        let ComplianceEraseOutcome::Completed { counts, .. } = outcome else {
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
        let target =
            ingest_fact_atomic(pool, &permit, &draft(Some(("src-cool", "k-cool"))), None).await?;
        let other =
            ingest_fact_atomic(pool, &permit, &draft(Some(("src-keep", "k-keep"))), None).await?;
        MemoryAuthoringPort::forget_memory(&pg, &permit, target.memory_id).await?;

        let auth = EraseAuthorization::new_for_tests(ComplianceEraseTarget::PersonalSourceScope {
            user_id: user,
            source_id: SourceId::new("src-cool"),
            drop_event_id: "test-drop-cooled-src".into(),
        });
        let outcome = pg
            .erase_personal_source_scope_if_drop_verified(
                &auth,
                user,
                &SourceId::new("src-cool"),
                &contract_sidecar_tables(),
            )
            .await?;
        assert!(
            matches!(outcome, ComplianceEraseOutcome::Completed { .. }),
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
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("source-scope cooled erase failed");
}
