//! Forget / hydrate / erase.
#![allow(clippy::doc_markdown, clippy::too_many_lines)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::PgStorage;
use crate::core_pg_sidecars;
use crate::verbs::forget::{
    COLD_FORMAT_VERSION, ColdRecord, ColdRejection, MemoryColdStore, cold_object_key,
    commit_forget, decode_record, encode_record, erase_memory, erase_memory_series,
    erase_memory_series_after_snapshot, forget_memory, forget_memory_oneshot, hydrate_one_in_tx,
    lock_admissions_for_erase, lock_lifecycle_targets_tx, lock_memory_handles_tx,
    purge_cold_objects_after_commit, snapshot_hot, snapshot_series_for_erase_tx,
};
use crate::verbs::goal_timeseries::{GoalWriteCommand, write_goal};
use crate::verbs::memory_timeseries::ingest_fact_timeseries;
use proxima_core::storage_ports::FactIngestPort;
use proxima_core::storage_ports::{MemoryAuthoringPort, OwnerTransferPort, OwnerWritePermit};
use proxima_core::verbs::fact_ingest::{CitationSpec, FactWriteCommand};
use proxima_core::verbs::goal_write::GoalState;
use proxima_core::{
    AccessKind, ColdObjectStore, EdgeEndpoint, EntityId, EntityKind, GroupId,
    MemoryHydrationStatus, OwnerRef, SchemaId, SchemaVersion, StorageError, UserId,
};
use proxima_pg_testkit::{create_db, db_url, drop_db};
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

/// The hydrate's registry-resolved embedding answer, same reason.
fn non_embeddable_schemas() -> Vec<String> {
    proxima_core::FlavorRegistry::new()
        .freeze_or_panic_for_tests()
        .non_embeddable_schema_ids()
        .to_vec()
}

/// Read the database-only historical identity witness without exposing it
/// through a production storage API. The lifecycle tests use this one helper
/// for both positive exact-kind assertions and negative transition checks.
async fn erased_pin_target_kind(
    pool: &sqlx::PgPool,
    t: Uuid,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar("SELECT kind::text FROM proxima_core.erased_pin_target WHERE t = $1")
        .bind(t)
        .fetch_optional(pool)
        .await
}

async fn wait_for_advisory_waiters(
    pool: &sqlx::PgPool,
    expected: i64,
) -> Result<(), Box<dyn std::error::Error>> {
    for _ in 0..100 {
        let waiting: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM pg_locks
              WHERE locktype = 'advisory' AND NOT granted
                AND database = (SELECT oid FROM pg_database
                                 WHERE datname = current_database())",
        )
        .fetch_one(pool)
        .await?;
        if waiting >= expected {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    Err(format!("expected {expected} advisory waiters").into())
}

const AGENT_NOTE: &str = "proxima_core.agent_note_v1";
const WRITE_ACT: &str = "proxima_core.write_act_v1";
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
    let goal_ids: Vec<Uuid> =
        sqlx::query_scalar("SELECT t FROM proxima_core.goal WHERE t = ANY($1::uuid[])")
            .bind(&draft.refs)
            .fetch_all(&mut *tx)
            .await
            .map_err(|err| StorageError::Internal(err.to_string()))?;
    let references = draft
        .refs
        .iter()
        .copied()
        .map(|id| {
            if goal_ids.contains(&id) {
                EdgeEndpoint::goal(proxima_core::GoalId::new(id))
            } else {
                EdgeEndpoint::memory(EntityKind::Fact, proxima_core::MemoryId::new(id))
            }
        })
        .collect::<Vec<_>>();
    let outcome = ingest_fact_timeseries(
        &mut tx,
        permit.owner(),
        draft,
        &draft.derived_from,
        &references,
        tables,
        None,
    )
    .await?;
    tx.commit()
        .await
        .map_err(|err| StorageError::Internal(err.to_string()))?;
    Ok(outcome)
}

async fn append_in_own_transaction(
    pool: &sqlx::PgPool,
    owner: OwnerRef,
    draft: FactWriteCommand,
) -> Result<proxima_core::verbs::fact_ingest::FactIngestOutcome, StorageError> {
    let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
    let mut tx = pool
        .begin()
        .await
        .map_err(|err| StorageError::Internal(err.to_string()))?;
    let outcome =
        ingest_fact_timeseries(&mut tx, permit.owner(), &draft, &[], &[], &[], None).await;
    match outcome {
        Ok(outcome) => {
            tx.commit()
                .await
                .map_err(|err| StorageError::Internal(err.to_string()))?;
            Ok(outcome)
        }
        Err(err) => {
            tx.rollback()
                .await
                .map_err(|rollback| StorageError::Internal(rollback.to_string()))?;
            Err(err)
        }
    }
}

async fn sidecar_tables_for(pool: &sqlx::PgPool, t: Uuid) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar("SELECT sidecar_tables FROM proxima_core.memory WHERE t = $1")
        .bind(t)
        .fetch_one(pool)
        .await
}

async fn cool_one(
    pool: &sqlx::PgPool,
    owner: &OwnerRef,
    cold: &MemoryColdStore,
    t: Uuid,
    key: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut tx = pool.begin().await?;
    forget_memory(
        &mut tx,
        &core_pg_sidecars(),
        &surfaces(),
        cold,
        key,
        t,
        owner.stored_owner_id(),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

async fn replace_cold_record(
    pool: &sqlx::PgPool,
    cold: &MemoryColdStore,
    t: Uuid,
    key: &str,
    record: &ColdRecord,
) -> Result<(), Box<dyn std::error::Error>> {
    let bytes = encode_record(record)?;
    replace_cold_bytes(pool, cold, t, key, &bytes).await
}

async fn replace_cold_bytes(
    pool: &sqlx::PgPool,
    cold: &MemoryColdStore,
    t: Uuid,
    key: &str,
    bytes: &[u8],
) -> Result<(), Box<dyn std::error::Error>> {
    cold.put(key, bytes).await?;
    let mut tx = pool.begin().await?;
    // Test-only object mutation must retain the digest witness so the
    // following assertion reaches the intended identity/payload gate. The
    // production append-only trigger correctly rejects witness rewrites.
    sqlx::query("ALTER TABLE proxima_core.cooled DISABLE TRIGGER cooled_append_only")
        .execute(tx.as_mut())
        .await?;
    sqlx::query("UPDATE proxima_core.cooled SET cold_digest = $2 WHERE t = $1")
        .bind(t)
        .bind(super::cold_digest(bytes))
        .execute(tx.as_mut())
        .await?;
    sqlx::query("ALTER TABLE proxima_core.cooled ENABLE TRIGGER cooled_append_only")
        .execute(tx.as_mut())
        .await?;
    tx.commit().await?;
    Ok(())
}

struct BarrierColdStore {
    inner: Arc<MemoryColdStore>,
    first_gets: AtomicUsize,
    barrier: tokio::sync::Barrier,
}

#[async_trait::async_trait]
impl ColdObjectStore for BarrierColdStore {
    async fn put(&self, key: &str, bytes: &[u8]) -> Result<(), StorageError> {
        self.inner.put(key, bytes).await
    }

    async fn get(&self, key: &str) -> Result<Vec<u8>, StorageError> {
        // Each batch preflights its two candidates serially. Reuse a
        // two-party barrier for both rounds so reversed batches reach the
        // transaction-wide union-lock phase together without waiting for an
        // impossible four-party first round.
        if self.first_gets.fetch_add(1, Ordering::SeqCst) < 4 {
            self.barrier.wait().await;
            tokio::task::yield_now().await;
        }
        self.inner.get(key).await
    }

    async fn delete(&self, key: &str) -> Result<(), StorageError> {
        self.inner.delete(key).await
    }
}

/// Encode the v5 layout used by a pre-stamp cold object. This is deliberately
/// test-only: production writers always emit the current format, while the
/// hydration gate must prove that an older object cannot reconstruct a stamp
/// from attacker-controlled dump names.
fn encode_v5_without_sidecar_stamp(rec: &ColdRecord) -> Result<Vec<u8>, StorageError> {
    let mut out = vec![5_u8];
    super::write_uuid(&mut out, rec.row.handle);
    super::write_uuid(&mut out, rec.row.t);
    super::write_str(&mut out, &rec.row.kind)?;
    super::write_uuid(&mut out, rec.row.owner_id);
    super::write_opt_str(&mut out, rec.row.source_id.as_deref())?;
    super::write_opt_str(&mut out, rec.row.ingest_key.as_deref())?;
    super::write_opt_uuid(&mut out, rec.row.blob_id);
    super::write_uuid_list(&mut out, &rec.row.origins)?;
    super::write_uuid_list(&mut out, &rec.row.refs)?;
    super::write_uuid_list(&mut out, &rec.row.goal_refs)?;
    super::write_str(&mut out, &rec.schema_id)?;
    super::write_count(&mut out, rec.sidecar_dumps.len())?;
    for (table, json) in &rec.sidecar_dumps {
        super::write_str(&mut out, table)?;
        super::write_str(&mut out, json)?;
    }
    super::write_str_list(&mut out, &rec.embed_models)?;
    super::write_opt_str(&mut out, rec.sketch.as_deref())?;
    Ok(out)
}

/// Fixture-only simulation of a pre-0003 cooled locator. The production
/// append-only trigger is disabled and restored in this same transaction so a
/// legacy NULL-array row cannot escape into ordinary runtime writes.
async fn make_legacy_cooled(
    pool: &sqlx::PgPool,
    t: Uuid,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut tx = pool.begin().await?;
    sqlx::query("ALTER TABLE proxima_core.cooled DISABLE TRIGGER cooled_append_only")
        .execute(tx.as_mut())
        .await?;
    sqlx::query(
        "UPDATE proxima_core.cooled
            SET origins = NULL, refs = NULL, goal_refs = NULL
          WHERE t = $1",
    )
    .bind(t)
    .execute(tx.as_mut())
    .await?;
    sqlx::query("ALTER TABLE proxima_core.cooled ENABLE TRIGGER cooled_append_only")
        .execute(tx.as_mut())
        .await?;
    tx.commit().await?;
    Ok(())
}

/// Fixture-only deletion that intentionally leaves no historical witness, to
/// prove hydrate rejects an unknown decoded pin and rolls back atomically.
async fn delete_memory_without_witness(
    pool: &sqlx::PgPool,
    t: Uuid,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut tx = pool.begin().await?;
    sqlx::query("ALTER TABLE proxima_core.memory DISABLE TRIGGER memory_erased_pin_target")
        .execute(tx.as_mut())
        .await?;
    sqlx::query("DELETE FROM proxima_core.memory WHERE t = $1")
        .bind(t)
        .execute(tx.as_mut())
        .await?;
    sqlx::query("ALTER TABLE proxima_core.memory ENABLE TRIGGER memory_erased_pin_target")
        .execute(tx.as_mut())
        .await?;
    tx.commit().await?;
    Ok(())
}

/// Fixture-only corruption of a witness kind. The insert/delete guard stays
/// enabled; only the witness immutability trigger is disabled for this one
/// UPDATE and restored before the transaction commits.
async fn change_witness_kind(
    pool: &sqlx::PgPool,
    t: Uuid,
    kind: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut tx = pool.begin().await?;
    sqlx::query(
        "ALTER TABLE proxima_core.erased_pin_target
         DISABLE TRIGGER erased_pin_target_append_only",
    )
    .execute(tx.as_mut())
    .await?;
    sqlx::query("UPDATE proxima_core.erased_pin_target SET kind = $2::proxima_core.pin_target_kind WHERE t = $1")
        .bind(t)
        .bind(kind)
        .execute(tx.as_mut())
        .await?;
    sqlx::query(
        "ALTER TABLE proxima_core.erased_pin_target
         ENABLE TRIGGER erased_pin_target_append_only",
    )
    .execute(tx.as_mut())
    .await?;
    tx.commit().await?;
    Ok(())
}

async fn insert_unassigned_goal(
    pool: &sqlx::PgPool,
    owner: OwnerRef,
    request_id: &str,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let mut tx = pool.begin().await?;
    let goal = write_goal(
        &mut tx,
        &owner,
        &GoalWriteCommand {
            handle: None,
            schema_id: "core/task-goal-v1".into(),
            title: request_id.into(),
            state: GoalState::Active,
            request_id: request_id.into(),
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
    tx.commit().await?;
    Ok(goal.t)
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
        let written = pg.ingest_fact_atomic(&permit, &sourced, None).await?;
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
        assert_eq!(
            erased_pin_target_kind(pool, t).await?,
            None,
            "forget is a reversible cooling transition, not an erase"
        );
        let heads: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.memory_head WHERE handle = $1",
        )
        .bind(written.handle)
        .fetch_one(pool)
        .await?;
        assert_eq!(heads, 0, "last-t forget deletes the head");
        let keys: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.ingest_keys WHERE ingest_key = 'k1'",
        )
        .fetch_one(pool)
        .await?;
        assert_eq!(keys, 1, "forget does not touch ingest_keys");

        let mut tx = pool.begin().await?;
        hydrate_one_in_tx(
            &mut tx,
            &core_pg_sidecars(),
            &surfaces(),
            &cold,
            t,
            owner.stored_owner_id(),
            &non_embeddable_schemas(),
        )
        .await?
        .expect("the record restores");
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
        assert_eq!(head_t, t, "hydrate recreates head at the same t");
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
        assert_eq!(
            erased_pin_target_kind(pool, t).await?,
            None,
            "hydration restores the row and must not mint a witness"
        );

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
        assert_eq!(
            erased_pin_target_kind(pool, t).await?,
            Some("fact".to_owned()),
            "the final hard erase records the target kind"
        );

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
        let first = pg
            .ingest_fact_atomic(&permit, &draft(Some(("src", "e1"))), None)
            .await?;
        let mut second_draft = draft(Some(("src", "e2")));
        second_draft.handle = Some(first.handle);
        let second = pg.ingest_fact_atomic(&permit, &second_draft, None).await?;
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

        let origin = pg.ingest_fact_atomic(&permit, &draft(None), None).await?;
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
        hydrate_one_in_tx(
            &mut tx,
            pg.sidecars(),
            &surfaces(),
            cold.as_ref(),
            t,
            owner.stored_owner_id(),
            &non_embeddable_schemas(),
        )
        .await?
        .expect("the record restores");
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
        let first = pg.ingest_fact_atomic(&permit, &draft(None), None).await?;
        let mut later = draft(None);
        later.handle = Some(first.handle);
        let second = pg.ingest_fact_atomic(&permit, &later, None).await?;
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

#[tokio::test]
async fn series_erase_includes_hot_append_that_wins_the_handle_lock_first() {
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
        let first = pg.ingest_fact_atomic(&permit, &draft(None), None).await?;
        let mut second_draft = draft(None);
        second_draft.handle = Some(first.handle);
        let second = pg.ingest_fact_atomic(&permit, &second_draft, None).await?;
        let first_t = first.memory_id.into_inner();

        // Hold the series lock while the erase transaction captures the seed
        // and waits. The append is committed in the holder transaction, so
        // the erase's post-lock expansion must include its new version.
        let mut append_holder = pool.begin().await?;
        lock_memory_handles_tx(&mut append_holder, &[first.handle]).await?;
        let erase_pool = pool.clone();
        let erase_owner = owner;
        let erase = tokio::spawn(async move {
            let mut tx = erase_pool
                .begin()
                .await
                .map_err(|err| StorageError::Internal(format!("begin series erase: {err}")))?;
            let result = erase_memory_series(
                &mut tx,
                &core_pg_sidecars(),
                &surfaces(),
                &erase_owner,
                &[first_t],
            )
            .await?;
            tx.commit()
                .await
                .map_err(|err| StorageError::Internal(format!("commit series erase: {err}")))?;
            Ok::<_, StorageError>(result)
        });
        wait_for_advisory_waiters(pool, 1).await?;

        let mut append_draft = draft(None);
        append_draft.handle = Some(first.handle);
        let append = ingest_fact_timeseries(
            &mut append_holder,
            permit.owner(),
            &append_draft,
            &[],
            &[],
            &[],
            None,
        )
        .await?;
        append_holder.commit().await?;
        let (erased, _) =
            tokio::time::timeout(std::time::Duration::from_secs(10), erase).await???;
        assert_eq!(
            erased, 3,
            "the append committed before erase must be included"
        );

        let rows: i64 = sqlx::query_scalar::<_, i64>(
            "SELECT count(*)::bigint FROM proxima_core.memory
              WHERE handle = $1
             UNION ALL
             SELECT count(*)::bigint FROM proxima_core.cooled
              WHERE handle = $1",
        )
        .bind(first.handle)
        .fetch_all(pool)
        .await?
        .into_iter()
        .sum();
        assert_eq!(rows, 0);
        let heads: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.memory_head WHERE handle = $1",
        )
        .bind(first.handle)
        .fetch_one(pool)
        .await?;
        assert_eq!(heads, 0);
        assert_ne!(append.memory_id, second.memory_id);
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("hot append-before-erase linearization failed");
}

#[tokio::test]
async fn series_erase_wins_hot_handle_and_append_retries_then_survives() {
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
        let first = pg.ingest_fact_atomic(&permit, &draft(None), None).await?;
        let first_t = first.memory_id.into_inner();

        let mut erase_holder = pool.begin().await?;
        lock_memory_handles_tx(&mut erase_holder, &[first.handle]).await?;
        let append_pool = pool.clone();
        let append_owner = owner;
        let mut append_draft = draft(None);
        append_draft.handle = Some(first.handle);
        let append = tokio::spawn(async move {
            append_in_own_transaction(&append_pool, append_owner, append_draft).await
        });
        wait_for_advisory_waiters(pool, 1).await?;

        let (erased, _) = erase_memory_series(
            &mut erase_holder,
            &core_pg_sidecars(),
            &surfaces(),
            &owner,
            &[first_t],
        )
        .await?;
        assert_eq!(erased, 1);
        erase_holder.commit().await?;
        let append_result = tokio::time::timeout(std::time::Duration::from_secs(10), append)
            .await?
            .expect("the record restores");
        let append_err = append_result.expect_err("stale append must retry");
        assert!(
            matches!(append_err, StorageError::Retryable(_)),
            "append was prepared against the erased head: {append_err:?}"
        );

        let mut retry_draft = draft(None);
        retry_draft.handle = Some(first.handle);
        let survivor = pg.ingest_fact_atomic(&permit, &retry_draft, None).await?;
        let head_t: Uuid =
            sqlx::query_scalar("SELECT t FROM proxima_core.memory_head WHERE handle = $1")
                .bind(first.handle)
                .fetch_one(pool)
                .await?;
        assert_eq!(head_t, survivor.memory_id.into_inner());
        let rows: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.memory WHERE handle = $1",
        )
        .bind(first.handle)
        .fetch_one(pool)
        .await?;
        assert_eq!(rows, 1);
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("hot erase-before-append linearization failed");
}

#[tokio::test]
async fn series_erase_linearizes_with_fully_cooled_headless_series() {
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
        let first = pg.ingest_fact_atomic(&permit, &draft(None), None).await?;
        let mut second_draft = draft(None);
        second_draft.handle = Some(first.handle);
        let second = pg.ingest_fact_atomic(&permit, &second_draft, None).await?;
        let first_t = first.memory_id.into_inner();
        let second_t = second.memory_id.into_inner();
        cool_one(pool, &owner, &cold, first_t, &cold_object_key(first_t)).await?;
        cool_one(pool, &owner, &cold, second_t, &cold_object_key(second_t)).await?;
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*)::bigint FROM proxima_core.memory_head WHERE handle = $1",
            )
            .bind(first.handle)
            .fetch_one(pool)
            .await?,
            0,
            "a fully cooled series has no head"
        );

        // Append wins: the series erase captures the cooled handle and waits;
        // the append recreates a head, and expansion after the lock must see
        // both cooled versions plus the new hot version.
        let mut append_holder = pool.begin().await?;
        lock_memory_handles_tx(&mut append_holder, &[first.handle]).await?;
        let erase_pool = pool.clone();
        let erase_owner = owner;
        let erase = tokio::spawn(async move {
            let mut tx = erase_pool.begin().await.map_err(|err| {
                StorageError::Internal(format!("begin cooled series erase: {err}"))
            })?;
            let result = erase_memory_series(
                &mut tx,
                &core_pg_sidecars(),
                &surfaces(),
                &erase_owner,
                &[first_t],
            )
            .await?;
            tx.commit().await.map_err(|err| {
                StorageError::Internal(format!("commit cooled series erase: {err}"))
            })?;
            Ok::<_, StorageError>(result)
        });
        wait_for_advisory_waiters(pool, 1).await?;
        let mut append_draft = draft(None);
        append_draft.handle = Some(first.handle);
        ingest_fact_timeseries(
            &mut append_holder,
            permit.owner(),
            &append_draft,
            &[],
            &[],
            &[],
            None,
        )
        .await?;
        append_holder.commit().await?;
        let (erased, _) =
            tokio::time::timeout(std::time::Duration::from_secs(10), erase).await???;
        assert_eq!(erased, 3);

        // Recreate a fully cooled, headless series for the opposite
        // linearization. A prepared append may have observed the empty head;
        // if erase wins the handle it is allowed to survive after the erase.
        let first = pg.ingest_fact_atomic(&permit, &draft(None), None).await?;
        let mut second_draft = draft(None);
        second_draft.handle = Some(first.handle);
        let second = pg.ingest_fact_atomic(&permit, &second_draft, None).await?;
        let first_t = first.memory_id.into_inner();
        let second_t = second.memory_id.into_inner();
        cool_one(pool, &owner, &cold, first_t, &cold_object_key(first_t)).await?;
        cool_one(pool, &owner, &cold, second_t, &cold_object_key(second_t)).await?;
        let mut erase_holder = pool.begin().await?;
        lock_memory_handles_tx(&mut erase_holder, &[first.handle]).await?;
        let append_pool = pool.clone();
        let append_owner = owner;
        let mut append_draft = draft(None);
        append_draft.handle = Some(first.handle);
        let append = tokio::spawn(async move {
            append_in_own_transaction(&append_pool, append_owner, append_draft).await
        });
        wait_for_advisory_waiters(pool, 1).await?;
        let (erased, _) = erase_memory_series(
            &mut erase_holder,
            &core_pg_sidecars(),
            &surfaces(),
            &owner,
            &[first_t],
        )
        .await?;
        assert_eq!(erased, 2);
        erase_holder.commit().await?;
        let survivor = tokio::time::timeout(std::time::Duration::from_secs(10), append).await???;
        let head_t: Uuid =
            sqlx::query_scalar("SELECT t FROM proxima_core.memory_head WHERE handle = $1")
                .bind(first.handle)
                .fetch_one(pool)
                .await?;
        assert_eq!(head_t, survivor.memory_id.into_inner());
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*)::bigint FROM proxima_core.memory WHERE handle = $1",
            )
            .bind(first.handle)
            .fetch_one(pool)
            .await?,
            1
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("headless cooled series linearization failed");
}

#[tokio::test]
async fn series_erase_does_not_cross_one_reused_handle_in_a_batch() {
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
        let reused = pg.ingest_fact_atomic(&permit, &draft(None), None).await?;
        let untouched = pg.ingest_fact_atomic(&permit, &draft(None), None).await?;
        let reused_t = reused.memory_id.into_inner();
        let untouched_t = untouched.memory_id.into_inner();

        // Pause the first erase at its real discovery/lock seam. Another
        // transaction can erase every observed version and then a writer can
        // reuse the now-empty handle before this transaction asks for its
        // advisory lock. The saved membership must distinguish that
        // replacement from a late append to the original series. Keeping a
        // second original handle in the batch proves the witness is checked
        // per handle rather than as one global version-set intersection.
        let mut waiting = pool.begin().await?;
        let before = snapshot_series_for_erase_tx(
            &mut waiting,
            owner.stored_owner_id(),
            &[reused_t, untouched_t],
        )
        .await?;

        let mut replacement_erase = pool.begin().await?;
        let (erased, plan) = erase_memory_series(
            &mut replacement_erase,
            &core_pg_sidecars(),
            &surfaces(),
            &owner,
            &[reused_t],
        )
        .await?;
        assert_eq!(erased, 1);
        assert!(plan.is_empty());
        replacement_erase.commit().await?;

        let mut replacement_draft = draft(None);
        replacement_draft.handle = Some(reused.handle);
        let replacement = pg
            .ingest_fact_atomic(&permit, &replacement_draft, None)
            .await?;

        let err = erase_memory_series_after_snapshot(
            &mut waiting,
            &core_pg_sidecars(),
            &surfaces(),
            &owner,
            before,
        )
        .await
        .expect_err("a reused handle is a replacement series");
        assert!(
            matches!(err, StorageError::Retryable(_)),
            "replacement detection must retry the stale erase: {err:?}"
        );
        waiting.rollback().await?;

        let head_t: Uuid =
            sqlx::query_scalar("SELECT t FROM proxima_core.memory_head WHERE handle = $1")
                .bind(reused.handle)
                .fetch_one(pool)
                .await?;
        assert_eq!(head_t, replacement.memory_id.into_inner());
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*)::bigint FROM proxima_core.memory WHERE handle = $1",
            )
            .bind(reused.handle)
            .fetch_one(pool)
            .await?,
            1
        );
        let untouched_head: Uuid =
            sqlx::query_scalar("SELECT t FROM proxima_core.memory_head WHERE handle = $1")
                .bind(untouched.handle)
                .fetch_one(pool)
                .await?;
        assert_eq!(untouched_head, untouched_t);
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("series erase crossed a complete handle reuse in a batch");
}

#[tokio::test]
async fn non_head_erase_racing_append_preserves_the_greatest_head() {
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
        let first = pg.ingest_fact_atomic(&permit, &draft(None), None).await?;
        let mut second_draft = draft(None);
        second_draft.handle = Some(first.handle);
        let second = pg.ingest_fact_atomic(&permit, &second_draft, None).await?;
        let mut append_holder = pool.begin().await?;
        lock_memory_handles_tx(&mut append_holder, &[first.handle]).await?;
        let erase_pool = pool.clone();
        let erase_owner = owner;
        let erase_t = first.memory_id.into_inner();
        let erase = tokio::spawn(async move {
            let mut tx = erase_pool
                .begin()
                .await
                .map_err(|err| StorageError::Internal(format!("begin non-head erase: {err}")))?;
            let plan = erase_memory(
                &mut tx,
                &core_pg_sidecars(),
                &surfaces(),
                &erase_owner,
                erase_t,
            )
            .await?;
            tx.commit()
                .await
                .map_err(|err| StorageError::Internal(format!("commit non-head erase: {err}")))?;
            Ok::<_, StorageError>(plan)
        });
        wait_for_advisory_waiters(pool, 1).await?;

        let mut append_draft = draft(None);
        append_draft.handle = Some(first.handle);
        let append = ingest_fact_timeseries(
            &mut append_holder,
            permit.owner(),
            &append_draft,
            &[],
            &[],
            &[],
            None,
        )
        .await?;
        append_holder.commit().await?;
        let plan = tokio::time::timeout(std::time::Duration::from_secs(10), erase).await???;
        assert!(plan.is_empty());
        let appended = append;
        let head_t: Uuid =
            sqlx::query_scalar("SELECT t FROM proxima_core.memory_head WHERE handle = $1")
                .bind(first.handle)
                .fetch_one(pool)
                .await?;
        assert_eq!(head_t, appended.memory_id.into_inner());
        assert_ne!(head_t, first.memory_id.into_inner());
        assert_ne!(head_t, second.memory_id.into_inner());
        let remaining: Vec<Uuid> =
            sqlx::query_scalar("SELECT t FROM proxima_core.memory WHERE handle = $1 ORDER BY t")
                .bind(first.handle)
                .fetch_all(pool)
                .await?;
        assert_eq!(remaining, vec![second.memory_id.into_inner(), head_t]);
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("non-head erase raced append incorrectly");
}

#[tokio::test]
async fn hydrate_of_older_cooled_version_preserves_newer_head() {
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
        let first = pg.ingest_fact_atomic(&permit, &draft(None), None).await?;
        let mut later = draft(None);
        later.handle = Some(first.handle);
        let second = pg.ingest_fact_atomic(&permit, &later, None).await?;

        let cold = MemoryColdStore::default();
        let first_t = first.memory_id.into_inner();
        let first_key = cold_object_key(first_t);
        let mut tx = pool.begin().await?;
        forget_memory(
            &mut tx,
            &core_pg_sidecars(),
            &surfaces(),
            &cold,
            &first_key,
            first_t,
            owner.stored_owner_id(),
        )
        .await?;
        tx.commit().await?;

        let mut tx = pool.begin().await?;
        hydrate_one_in_tx(
            &mut tx,
            &core_pg_sidecars(),
            &surfaces(),
            &cold,
            first_t,
            owner.stored_owner_id(),
            &non_embeddable_schemas(),
        )
        .await?
        .expect("the record restores");
        tx.commit().await?;

        let head_t: Uuid =
            sqlx::query_scalar("SELECT t FROM proxima_core.memory_head WHERE handle = $1")
                .bind(first.handle)
                .fetch_one(pool)
                .await?;
        assert_eq!(
            head_t,
            second.memory_id.into_inner(),
            "hydrating an older cooled version must not rewind the newer head"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("older cooled hydration head preservation failed");
}

#[tokio::test]
async fn historical_restore_may_reuse_a_closed_handle_but_new_pins_may_not() {
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
        let target = pg.ingest_fact_atomic(&permit, &draft(None), None).await?;
        let source = pg
            .ingest_fact_atomic(
                &permit,
                &derived_abstraction(EntityKind::Fact, target.memory_id.into_inner()),
                None,
            )
            .await?;
        let source_t = source.memory_id.into_inner();
        let cold = MemoryColdStore::default();
        let mut tx = pool.begin().await?;
        forget_memory(
            &mut tx,
            &core_pg_sidecars(),
            &surfaces(),
            &cold,
            &cold_object_key(source_t),
            source_t,
            owner.stored_owner_id(),
        )
        .await?;
        tx.commit().await?;

        // The target remains live, but its series is closed. Restoring the
        // source must reuse its exact historical pin; it is not a new edge.
        sqlx::query("INSERT INTO proxima_core.closed_handle (handle) VALUES ($1)")
            .bind(target.handle)
            .execute(pool)
            .await?;
        let mut tx = pool.begin().await?;
        hydrate_one_in_tx(
            &mut tx,
            &core_pg_sidecars(),
            &surfaces(),
            &cold,
            source_t,
            owner.stored_owner_id(),
            &non_embeddable_schemas(),
        )
        .await?
        .expect("the record restores");
        tx.commit().await?;
        let restored_refs: Vec<Uuid> =
            sqlx::query_scalar("SELECT origins || refs FROM proxima_core.memory WHERE t = $1")
                .bind(source_t)
                .fetch_one(pool)
                .await?;
        assert_eq!(restored_refs, vec![target.memory_id.into_inner()]);

        let err = pg
            .ingest_fact_atomic(
                &permit,
                &derived_abstraction(EntityKind::Fact, target.memory_id.into_inner()),
                None,
            )
            .await
            .expect_err("an ordinary new pin to a closed handle must be refused");
        assert!(
            err.to_string().contains("closed_handle") || err.to_string().contains("23514"),
            "got: {err}"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("closed-handle restoration test failed");
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
        let written = pg.ingest_fact_atomic(&permit, &draft(None), None).await?;
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
        first.await?.expect("the record restores");
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
        let written = pg.ingest_fact_atomic(&permit, &draft(None), None).await?;
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
/// The dump cannot be made to differ with an `UPDATE` of the sidecar's `body`:
/// `agent_note_v1` is append-only WITH the projection, because the projection
/// row is derived once and an in-place text edit would leave the vector
/// describing text that is gone. A row that lands BETWEEN the snapshot and the
/// lock is the only real way for the dump to move, and it is the case the
/// function's doc comment names ("late sidecar").
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

        sqlx::query(
            "INSERT INTO proxima_core.agent_note_v1 (t, note_id, title, body, tags)
             VALUES ($1, $1, 'n', 'snapshot body', ARRAY['tag'])",
        )
        .bind(t)
        .execute(pool)
        .await?;

        // The unlocked snapshot is taken, and then the record changes under
        // it. A sidecar row cannot supply that change — sidecars are
        // append-only and a stamped table with no row no longer cools at all
        // — so the drift comes from the vector set the record also carries.
        let mut conn = pool.acquire().await?;
        let snapshot = snapshot_hot(&mut conn, &core_pg_sidecars(), &surfaces(), t).await?;
        drop(conn);

        sqlx::query(
            "INSERT INTO proxima_core.embeddings
                (entity_id, model_id, embedding_version, vec, owner_id)
             VALUES ($1, 'late-embed', 1, $3::vector, $2)",
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
            "locked dump must re-PUT after the record changed under the snapshot"
        );

        let mut tx = pool.begin().await?;
        hydrate_one_in_tx(
            &mut tx,
            &core_pg_sidecars(),
            &surfaces(),
            &cold,
            t,
            owner.stored_owner_id(),
            &non_embeddable_schemas(),
        )
        .await?
        .expect("the record restores");
        tx.commit().await?;
        let note: String =
            sqlx::query_scalar("SELECT body FROM proxima_core.agent_note_v1 WHERE t = $1")
                .bind(t)
                .fetch_one(pool)
                .await?;
        assert_eq!(
            note, "snapshot body",
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
        hydrate_one_in_tx(
            &mut tx,
            &sidecars,
            &surfaces(),
            &cold,
            t,
            owner.stored_owner_id(),
            &non_embeddable_schemas(),
        )
        .await?
        .expect("the record restores");
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
        hydrate_one_in_tx(
            &mut tx,
            &core_pg_sidecars(),
            &surfaces(),
            &cold,
            t,
            owner.stored_owner_id(),
            &non_embeddable_schemas(),
        )
        .await?
        .expect("the record restores");
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
        // TABLE, so selecting the statement by table alone writes a row per
        // stamped extra, each claiming a schema this memory is not.
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

fn derived_perspective(origin: Uuid) -> FactWriteCommand {
    let mut cmd = draft(None);
    cmd.kind = "perspective".into();
    cmd.derived_from = vec![EdgeEndpoint::memory(
        EntityKind::Abstraction,
        proxima_core::MemoryId::new(origin),
    )];
    cmd
}

#[tokio::test]
async fn hard_erase_witnesses_each_hot_memory_kind() {
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
        let fact = pg.ingest_fact_atomic(&permit, &draft(None), None).await?;
        let abstraction = pg
            .ingest_fact_atomic(
                &permit,
                &derived_abstraction(EntityKind::Fact, fact.memory_id.into_inner()),
                None,
            )
            .await?;
        let perspective = pg
            .ingest_fact_atomic(
                &permit,
                &derived_perspective(abstraction.memory_id.into_inner()),
                None,
            )
            .await?;
        let cold = MemoryColdStore::default();

        for (t, expected_kind) in [
            (perspective.memory_id.into_inner(), "perspective"),
            (abstraction.memory_id.into_inner(), "abstraction"),
            (fact.memory_id.into_inner(), "fact"),
        ] {
            assert_eq!(erased_pin_target_kind(pool, t).await?, None);
            let mut tx = pool.begin().await?;
            erase_memory(&mut tx, &core_pg_sidecars(), &surfaces(), &owner, t).await?;
            tx.commit().await?;
            assert_eq!(
                erased_pin_target_kind(pool, t).await?,
                Some(expected_kind.to_owned()),
                "hard erase records the exact memory kind"
            );
        }

        // A forget transition is not abandonment. Its cooled row remains the
        // source of truth and therefore must not mint a witness until the
        // later hard erase deletes that row.
        let cooled = pg.ingest_fact_atomic(&permit, &draft(None), None).await?;
        let cooled_t = cooled.memory_id.into_inner();
        let cooled_key = cold_object_key(cooled_t);
        let mut tx = pool.begin().await?;
        forget_memory(
            &mut tx,
            &core_pg_sidecars(),
            &surfaces(),
            &cold,
            &cooled_key,
            cooled_t,
            owner.stored_owner_id(),
        )
        .await?;
        tx.commit().await?;
        assert_eq!(erased_pin_target_kind(pool, cooled_t).await?, None);
        let mut tx = pool.begin().await?;
        let plan =
            erase_memory(&mut tx, &core_pg_sidecars(), &surfaces(), &owner, cooled_t).await?;
        tx.commit().await?;
        assert_eq!(
            erased_pin_target_kind(pool, cooled_t).await?,
            Some("fact".to_owned())
        );
        purge_cold_objects_after_commit(pool, &cold, &plan).await;

        // The Goal trigger has its own vocabulary and does not depend on the
        // memory erase path. Delete a real Goal row to exercise that trigger.
        let mut tx = pool.begin().await?;
        let goal = write_goal(
            &mut tx,
            &owner,
            &GoalWriteCommand {
                handle: None,
                schema_id: "core/task-goal-v1".into(),
                title: "witness me".into(),
                state: GoalState::Active,
                request_id: "witness-goal".into(),
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
        tx.commit().await?;
        assert_eq!(erased_pin_target_kind(pool, goal.t).await?, None);
        sqlx::query("DELETE FROM proxima_core.goal WHERE t = $1")
            .bind(goal.t)
            .execute(pool)
            .await?;
        assert_eq!(
            erased_pin_target_kind(pool, goal.t).await?,
            Some("goal".to_owned())
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("hard erase witness matrix failed");
}

#[tokio::test]
async fn exact_hydrate_restores_witnessed_sole_fact_origin() {
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
        let fact = pg.ingest_fact_atomic(&permit, &draft(None), None).await?;
        let fact_t = fact.memory_id.into_inner();
        let abstraction = pg
            .ingest_fact_atomic(
                &permit,
                &derived_abstraction(EntityKind::Fact, fact_t),
                None,
            )
            .await?;
        let abstraction_t = abstraction.memory_id.into_inner();
        let cold = MemoryColdStore::default();
        let abstraction_key = cold_object_key(abstraction_t);
        cool_one(pool, &owner, &cold, abstraction_t, &abstraction_key).await?;

        let mut tx = pool.begin().await?;
        erase_memory(&mut tx, &core_pg_sidecars(), &surfaces(), &owner, fact_t).await?;
        tx.commit().await?;
        assert_eq!(
            erased_pin_target_kind(pool, fact_t).await?,
            Some("fact".to_owned()),
            "hard erase preserves the sole origin's closed kind"
        );

        let before_memory: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.memory")
                .fetch_one(pool)
                .await?;
        let before_heads: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.memory_head")
                .fetch_one(pool)
                .await?;
        let err = pg
            .ingest_fact_atomic(
                &permit,
                &derived_abstraction(EntityKind::Fact, fact_t),
                None,
            )
            .await
            .expect_err("ordinary derivation cannot use an erased witness");
        assert!(
            err.to_string().contains("does not exist")
                || err.to_string().contains("23503")
                || err.to_string().contains("grounding")
                || err.to_string().contains("non-fact"),
            "got: {err}"
        );
        let after_memory: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.memory")
                .fetch_one(pool)
                .await?;
        let after_heads: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.memory_head")
                .fetch_one(pool)
                .await?;
        assert_eq!(after_memory, before_memory, "failed admission is atomic");
        assert_eq!(after_heads, before_heads, "failed admission leaves no head");

        let mut tx = pool.begin().await?;
        hydrate_one_in_tx(
            &mut tx,
            &core_pg_sidecars(),
            &surfaces(),
            &cold,
            abstraction_t,
            owner.stored_owner_id(),
            &non_embeddable_schemas(),
        )
        .await?
        .expect("the record restores");
        tx.commit().await?;
        let restored: (Vec<Uuid>, Vec<Uuid>) =
            sqlx::query_as("SELECT origins, refs FROM proxima_core.memory WHERE t = $1")
                .bind(abstraction_t)
                .fetch_one(pool)
                .await?;
        assert_eq!(restored.0, vec![fact_t]);
        assert!(restored.1.is_empty());
        assert_eq!(
            erased_pin_target_kind(pool, fact_t).await?,
            Some("fact".to_owned())
        );
        assert_eq!(
            erased_pin_target_kind(pool, abstraction_t).await?,
            None,
            "hydrate deletes no witness and creates none"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("exact witnessed abstraction hydration failed");
}

/// Two cooled versions may recreate an empty series head concurrently. Both
/// inserts must succeed: the unique-conflict loser re-reads the compatible
/// winner instead of reporting a false identity mismatch.
#[tokio::test]
async fn concurrent_hydrates_recreate_an_empty_memory_head() {
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
        let cold = Arc::new(MemoryColdStore::default());

        let first = pg.ingest_fact_atomic(&permit, &draft(None), None).await?;
        let mut second_draft = draft(None);
        second_draft.handle = Some(first.handle);
        second_draft.payload = vec![1];
        let second = pg.ingest_fact_atomic(&permit, &second_draft, None).await?;
        assert_ne!(first.memory_id, second.memory_id);
        let first_t = first.memory_id.into_inner();
        let second_t = second.memory_id.into_inner();
        cool_one(
            &pool,
            &owner,
            cold.as_ref(),
            first_t,
            &cold_object_key(first_t),
        )
        .await?;
        cool_one(
            &pool,
            &owner,
            cold.as_ref(),
            second_t,
            &cold_object_key(second_t),
        )
        .await?;
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*)::bigint FROM proxima_core.memory_head WHERE handle = $1",
            )
            .bind(first.handle)
            .fetch_one(&pool)
            .await?,
            0,
            "cooling the complete series removes its head"
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*)::bigint FROM proxima_core.cooled WHERE t IN ($1, $2)",
            )
            .bind(first_t)
            .bind(second_t)
            .fetch_one(&pool)
            .await?,
            2,
            "both versions are cooled before the concurrent restore"
        );

        // The trigger is created in this disposable test database only. Its
        // transaction-scoped advisory wait forces both hydrations through the
        // empty-head read before either INSERT can win the unique key.
        sqlx::query(
            "CREATE OR REPLACE FUNCTION proxima_core.test_memory_head_insert_barrier()
             RETURNS trigger
             LANGUAGE plpgsql
             AS $$
             BEGIN
                 PERFORM pg_advisory_xact_lock(
                     hashtextextended('proxima-test-memory-head-empty-recreate', 0)
                 );
                 RETURN NEW;
             END;
             $$",
        )
        .execute(&pool)
        .await?;
        sqlx::query(
            "CREATE TRIGGER test_memory_head_insert_barrier
             BEFORE INSERT ON proxima_core.memory_head
             FOR EACH ROW
             EXECUTE FUNCTION proxima_core.test_memory_head_insert_barrier()",
        )
        .execute(&pool)
        .await?;

        let mut gate = pool.begin().await?;
        sqlx::query(
            "SELECT pg_advisory_xact_lock(
                 hashtextextended('proxima-test-memory-head-empty-recreate', 0)
             )",
        )
        .execute(&mut *gate)
        .await?;

        let older_pool = pool.clone();
        let older_cold = Arc::clone(&cold);
        let mut older_hydrate = tokio::spawn(async move {
            let mut tx = older_pool
                .begin()
                .await
                .map_err(|error| StorageError::Internal(error.to_string()))?;
            match hydrate_one_in_tx(
                &mut tx,
                &core_pg_sidecars(),
                &surfaces(),
                older_cold.as_ref(),
                first_t,
                owner.stored_owner_id(),
                &non_embeddable_schemas(),
            )
            .await
            {
                Ok(Ok(_)) => tx
                    .commit()
                    .await
                    .map_err(|error| StorageError::Internal(error.to_string())),
                Ok(Err(rejection)) => {
                    let _ = tx.rollback().await;
                    Err(StorageError::ConstraintViolation(format!("{rejection:?}")))
                }
                Err(error) => {
                    let _ = tx.rollback().await;
                    Err(error)
                }
            }
        });
        let newer_pool = pool.clone();
        let newer_cold = Arc::clone(&cold);
        let mut newer_hydrate = tokio::spawn(async move {
            let mut tx = newer_pool
                .begin()
                .await
                .map_err(|error| StorageError::Internal(error.to_string()))?;
            match hydrate_one_in_tx(
                &mut tx,
                &core_pg_sidecars(),
                &surfaces(),
                newer_cold.as_ref(),
                second_t,
                owner.stored_owner_id(),
                &non_embeddable_schemas(),
            )
            .await
            {
                Ok(Ok(_)) => tx
                    .commit()
                    .await
                    .map_err(|error| StorageError::Internal(error.to_string())),
                Ok(Err(rejection)) => {
                    let _ = tx.rollback().await;
                    Err(StorageError::ConstraintViolation(format!("{rejection:?}")))
                }
                Err(error) => {
                    let _ = tx.rollback().await;
                    Err(error)
                }
            }
        });

        wait_for_advisory_waiters(&pool, 2).await?;
        assert!(!older_hydrate.is_finished() && !newer_hydrate.is_finished());
        gate.rollback().await?;

        let joined = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            tokio::join!(&mut older_hydrate, &mut newer_hydrate)
        })
        .await;
        let Ok((first_result, second_result)) = joined else {
            older_hydrate.abort();
            newer_hydrate.abort();
            let _ = older_hydrate.await;
            let _ = newer_hydrate.await;
            return Err("concurrent hydration did not complete".into());
        };
        first_result??;
        second_result??;

        // Remove the fixture before checking the result; the disposable DB is
        // also dropped below, so no test trigger can escape its database.
        sqlx::query(
            "DROP TRIGGER test_memory_head_insert_barrier
               ON proxima_core.memory_head",
        )
        .execute(&pool)
        .await?;
        sqlx::query("DROP FUNCTION proxima_core.test_memory_head_insert_barrier()")
            .execute(&pool)
            .await?;

        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*)::bigint FROM proxima_core.cooled WHERE t IN ($1, $2)",
            )
            .bind(first_t)
            .bind(second_t)
            .fetch_one(&pool)
            .await?,
            0,
            "both cooled locators are consumed"
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*)::bigint FROM proxima_core.memory WHERE handle = $1",
            )
            .bind(first.handle)
            .fetch_one(&pool)
            .await?,
            2,
            "both cooled versions are hot after hydration"
        );
        let head_t: Uuid =
            sqlx::query_scalar("SELECT t FROM proxima_core.memory_head WHERE handle = $1")
                .bind(first.handle)
                .fetch_one(&pool)
                .await?;
        let greatest_t: Uuid = sqlx::query_scalar(
            "SELECT t FROM proxima_core.memory
              WHERE handle = $1
              ORDER BY t DESC
              LIMIT 1",
        )
        .bind(first.handle)
        .fetch_one(&pool)
        .await?;
        assert_eq!(head_t, greatest_t, "the recreated head is the greatest t");
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("concurrent empty-head hydration test failed");
}

/// Reversed overlapping batches must acquire one transaction-wide handle and
/// lifecycle union. The barrier releases both preflights together, which
/// makes the old per-item lock extension reach opposite handles concurrently.
#[tokio::test]
async fn reversed_overlapping_hydration_batches_do_not_deadlock() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let base_cold = Arc::new(MemoryColdStore::default());
        let pg = PgStorage::connect(&url).await?.with_cold(base_cold.clone());
        pg.run_migrations().await?;
        let pool = pg.pool_for_tests().clone();
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let first = pg.ingest_fact_atomic(&permit, &draft(None), None).await?;
        let second = pg.ingest_fact_atomic(&permit, &draft(None), None).await?;
        cool_one(
            &pool,
            &owner,
            base_cold.as_ref(),
            first.memory_id.into_inner(),
            &cold_object_key(first.memory_id.into_inner()),
        )
        .await?;
        cool_one(
            &pool,
            &owner,
            base_cold.as_ref(),
            second.memory_id.into_inner(),
            &cold_object_key(second.memory_id.into_inner()),
        )
        .await?;

        let gated = Arc::new(BarrierColdStore {
            inner: base_cold,
            first_gets: AtomicUsize::new(0),
            barrier: tokio::sync::Barrier::new(2),
        });
        let hydration_pg = pg.clone().with_cold(gated);
        let first_id = first.memory_id;
        let second_id = second.memory_id;
        let left_pg = hydration_pg.clone();
        let right_pg = hydration_pg;
        let left_permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let right_permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let left = tokio::spawn(async move {
            MemoryAuthoringPort::hydrate_memories(&left_pg, &left_permit, &[first_id, second_id])
                .await
        });
        let right = tokio::spawn(async move {
            MemoryAuthoringPort::hydrate_memories(&right_pg, &right_permit, &[second_id, first_id])
                .await
        });
        let joined = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            tokio::join!(left, right)
        })
        .await
        .map_err(|_| "reversed hydration batches deadlocked")?;
        let left = joined.0??;
        let right = joined.1??;
        assert!(left.committed && right.committed);
        let hydrated = left
            .outcomes
            .iter()
            .chain(&right.outcomes)
            .filter(|outcome| outcome.status == MemoryHydrationStatus::Hydrated)
            .count();
        assert_eq!(hydrated, 2, "exactly one batch performs both restores");
        assert!(left.outcomes.iter().chain(&right.outcomes).all(|outcome| {
            matches!(
                outcome.status,
                MemoryHydrationStatus::AlreadyHot | MemoryHydrationStatus::Hydrated
            )
        }));
        let cooled: i64 = sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.cooled")
            .fetch_one(&pool)
            .await?;
        assert_eq!(cooled, 0, "the successful batches leave no cooled rows");
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("reversed hydration batches failed");
}

/// The cold bytes are verified before the lifecycle wait. If an operator
/// changes the database witness while the hydrate is waiting, the post-lock
/// re-read must reject the attempt rather than restoring against the stale
/// digest it initially observed.
#[tokio::test]
async fn hydrate_retries_when_cold_digest_changes_under_lock() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let cold = Arc::new(MemoryColdStore::default());
        let pg = PgStorage::connect(&url).await?.with_cold(cold.clone());
        pg.run_migrations().await?;
        let pool = pg.pool_for_tests().clone();
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let source = pg.ingest_fact_atomic(&permit, &draft(None), None).await?;
        let source_t = source.memory_id.into_inner();
        cool_one(
            &pool,
            &owner,
            cold.as_ref(),
            source_t,
            &cold_object_key(source_t),
        )
        .await?;

        let mut gate = pool.begin().await?;
        lock_lifecycle_targets_tx(&mut gate, &[source_t]).await?;
        let hydrate_pool = pool.clone();
        let hydrate_cold = Arc::clone(&cold);
        let hydrate_task = tokio::spawn(async move {
            let mut tx = hydrate_pool
                .begin()
                .await
                .map_err(|error| StorageError::Internal(error.to_string()))?;
            let outcome = hydrate_one_in_tx(
                &mut tx,
                &core_pg_sidecars(),
                &surfaces(),
                hydrate_cold.as_ref(),
                source_t,
                owner.stored_owner_id(),
                &non_embeddable_schemas(),
            )
            .await;
            let _ = tx.rollback().await;
            outcome
        });
        wait_for_advisory_waiters(&pool, 1).await?;

        let mut witness_update = pool.begin().await?;
        sqlx::query("ALTER TABLE proxima_core.cooled DISABLE TRIGGER cooled_append_only")
            .execute(witness_update.as_mut())
            .await?;
        sqlx::query("UPDATE proxima_core.cooled SET cold_digest = $2 WHERE t = $1")
            .bind(source_t)
            .bind(vec![0_u8; 32])
            .execute(witness_update.as_mut())
            .await?;
        sqlx::query("ALTER TABLE proxima_core.cooled ENABLE TRIGGER cooled_append_only")
            .execute(witness_update.as_mut())
            .await?;
        witness_update.commit().await?;
        gate.rollback().await?;

        let outcome = hydrate_task.await?;
        assert!(
            matches!(outcome, Err(StorageError::Retryable(_))),
            "stale digest witness must be retried, got {outcome:?}"
        );
        let cooled: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.cooled WHERE t = $1")
                .bind(source_t)
                .fetch_one(&pool)
                .await?;
        assert_eq!(cooled, 1, "digest race leaves the source cooled");
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("cold digest race test failed");
}

#[tokio::test]
async fn exact_hydrate_restores_memory_and_goal_witness_refs() {
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
        let memory_target = pg.ingest_fact_atomic(&permit, &draft(None), None).await?;
        let memory_t = memory_target.memory_id.into_inner();
        let goal_t = insert_unassigned_goal(pool, owner, "witness-ref-goal").await?;

        let mut source_draft = draft(None);
        source_draft.refs = vec![memory_t, goal_t];
        let source = ingest_stamped(pool, &permit, &source_draft, &[]).await?;
        let source_t = source.memory_id.into_inner();
        let cold = MemoryColdStore::default();
        let source_key = cold_object_key(source_t);
        cool_one(pool, &owner, &cold, source_t, &source_key).await?;

        let mut tx = pool.begin().await?;
        erase_memory(&mut tx, &core_pg_sidecars(), &surfaces(), &owner, memory_t).await?;
        tx.commit().await?;
        sqlx::query("DELETE FROM proxima_core.goal WHERE t = $1")
            .bind(goal_t)
            .execute(pool)
            .await?;
        assert_eq!(
            erased_pin_target_kind(pool, memory_t).await?,
            Some("fact".to_owned())
        );
        assert_eq!(
            erased_pin_target_kind(pool, goal_t).await?,
            Some("goal".to_owned())
        );

        let mut tx = pool.begin().await?;
        hydrate_one_in_tx(
            &mut tx,
            &core_pg_sidecars(),
            &surfaces(),
            &cold,
            source_t,
            owner.stored_owner_id(),
            &non_embeddable_schemas(),
        )
        .await?
        .expect("the record restores");
        tx.commit().await?;
        let (restored_refs, restored_goal_refs): (Vec<Uuid>, Vec<Uuid>) =
            sqlx::query_as("SELECT refs, goal_refs FROM proxima_core.memory WHERE t = $1")
                .bind(source_t)
                .fetch_one(pool)
                .await?;
        assert_eq!(restored_refs, vec![memory_t]);
        assert_eq!(restored_goal_refs, vec![goal_t]);
        assert_eq!(
            erased_pin_target_kind(pool, memory_t).await?,
            Some("fact".to_owned()),
            "hydration must not remove the Memory witness"
        );
        assert_eq!(
            erased_pin_target_kind(pool, goal_t).await?,
            Some("goal".to_owned()),
            "hydration must not remove the Goal witness"
        );
        assert_eq!(erased_pin_target_kind(pool, source_t).await?, None);
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("exact witnessed Memory/Goal reference hydration failed");
}

#[tokio::test]
async fn hydrate_rejects_unknown_and_wrong_kind_witnesses_atomically() {
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

        // Fixture corruption removes the Fact without its DELETE witness. The
        // exact cooled Abstraction remains, but hydrate must reject its
        // unknown decoded origin and leave the locator/object untouched.
        let unknown_target = pg.ingest_fact_atomic(&permit, &draft(None), None).await?;
        let unknown_t = unknown_target.memory_id.into_inner();
        let unknown_source = pg
            .ingest_fact_atomic(
                &permit,
                &derived_abstraction(EntityKind::Fact, unknown_t),
                None,
            )
            .await?;
        let unknown_source_t = unknown_source.memory_id.into_inner();
        let unknown_key = cold_object_key(unknown_source_t);
        cool_one(pool, &owner, &cold, unknown_source_t, &unknown_key).await?;
        delete_memory_without_witness(pool, unknown_t).await?;
        assert_eq!(erased_pin_target_kind(pool, unknown_t).await?, None);
        let unknown_object = cold.get(&unknown_key).await?;
        let mut tx = pool.begin().await?;
        let err = hydrate_one_in_tx(
            &mut tx,
            &core_pg_sidecars(),
            &surfaces(),
            &cold,
            unknown_source_t,
            owner.stored_owner_id(),
            &non_embeddable_schemas(),
        )
        .await?
        .expect_err("unknown decoded origin must not be restored");
        tx.rollback().await?;
        // The record names an origin the live spine cannot admit and no
        // witness excuses. That is the record's own content being refused.
        assert_eq!(err, ColdRejection::InvalidObject);
        let unknown_cooled: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.cooled WHERE t = $1")
                .bind(unknown_source_t)
                .fetch_one(pool)
                .await?;
        assert_eq!(
            unknown_cooled, 1,
            "failed hydrate leaves the cooled locator"
        );
        assert_eq!(cold.get(&unknown_key).await?, unknown_object);
        assert_eq!(erased_pin_target_kind(pool, unknown_source_t).await?, None);

        // A real Abstraction witness is corrupted to Fact by disabling only
        // witness immutability. A cooled Perspective whose origin requires an
        // Abstraction must reject that exact-but-wrong kind atomically.
        let fact = pg.ingest_fact_atomic(&permit, &draft(None), None).await?;
        let abstraction = pg
            .ingest_fact_atomic(
                &permit,
                &derived_abstraction(EntityKind::Fact, fact.memory_id.into_inner()),
                None,
            )
            .await?;
        let perspective = pg
            .ingest_fact_atomic(
                &permit,
                &derived_perspective(abstraction.memory_id.into_inner()),
                None,
            )
            .await?;
        let perspective_t = perspective.memory_id.into_inner();
        let abstraction_t = abstraction.memory_id.into_inner();
        let perspective_key = cold_object_key(perspective_t);
        cool_one(pool, &owner, &cold, perspective_t, &perspective_key).await?;
        let mut tx = pool.begin().await?;
        erase_memory(
            &mut tx,
            &core_pg_sidecars(),
            &surfaces(),
            &owner,
            abstraction_t,
        )
        .await?;
        tx.commit().await?;
        assert_eq!(
            erased_pin_target_kind(pool, abstraction_t).await?,
            Some("abstraction".to_owned())
        );
        change_witness_kind(pool, abstraction_t, "fact").await?;
        assert_eq!(
            erased_pin_target_kind(pool, abstraction_t).await?,
            Some("fact".to_owned()),
            "fixture corruption changed only the witness kind"
        );
        let wrong_kind_object = cold.get(&perspective_key).await?;
        let mut tx = pool.begin().await?;
        let err = hydrate_one_in_tx(
            &mut tx,
            &core_pg_sidecars(),
            &surfaces(),
            &cold,
            perspective_t,
            owner.stored_owner_id(),
            &non_embeddable_schemas(),
        )
        .await?
        .expect_err("wrong-kind origin witness must not be restored");
        tx.rollback().await?;
        assert_eq!(err, ColdRejection::InvalidObject);
        let perspective_cooled: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.cooled WHERE t = $1")
                .bind(perspective_t)
                .fetch_one(pool)
                .await?;
        assert_eq!(perspective_cooled, 1);
        assert_eq!(cold.get(&perspective_key).await?, wrong_kind_object);
        assert_eq!(
            erased_pin_target_kind(pool, abstraction_t).await?,
            Some("fact".to_owned()),
            "failed hydrate neither repairs nor deletes the witness"
        );
        assert_eq!(erased_pin_target_kind(pool, perspective_t).await?, None);

        // The inverse mismatch is equally invalid: a Memory `refs` entry may
        // not be rescued by a witness whose closed kind says Goal.
        let memory_ref_target = pg.ingest_fact_atomic(&permit, &draft(None), None).await?;
        let memory_ref_target_t = memory_ref_target.memory_id.into_inner();
        let mut memory_ref_source_draft = draft(None);
        memory_ref_source_draft.refs = vec![memory_ref_target_t];
        let memory_ref_source =
            ingest_stamped(pool, &permit, &memory_ref_source_draft, &[]).await?;
        let memory_ref_source_t = memory_ref_source.memory_id.into_inner();
        let memory_ref_source_key = cold_object_key(memory_ref_source_t);
        cool_one(
            pool,
            &owner,
            &cold,
            memory_ref_source_t,
            &memory_ref_source_key,
        )
        .await?;
        let mut tx = pool.begin().await?;
        erase_memory(
            &mut tx,
            &core_pg_sidecars(),
            &surfaces(),
            &owner,
            memory_ref_target_t,
        )
        .await?;
        tx.commit().await?;
        assert_eq!(
            erased_pin_target_kind(pool, memory_ref_target_t).await?,
            Some("fact".to_owned())
        );
        change_witness_kind(pool, memory_ref_target_t, "goal").await?;
        let mut tx = pool.begin().await?;
        let err = hydrate_one_in_tx(
            &mut tx,
            &core_pg_sidecars(),
            &surfaces(),
            &cold,
            memory_ref_source_t,
            owner.stored_owner_id(),
            &non_embeddable_schemas(),
        )
        .await?
        .expect_err("Goal witness must not satisfy a Memory refs target");
        tx.rollback().await?;
        assert_eq!(err, ColdRejection::InvalidObject);
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*)::bigint FROM proxima_core.cooled WHERE t = $1",
            )
            .bind(memory_ref_source_t)
            .fetch_one(pool)
            .await?,
            1,
            "failed Memory-reference hydration leaves its cooled locator"
        );
        assert_eq!(
            erased_pin_target_kind(pool, memory_ref_target_t).await?,
            Some("goal".to_owned()),
            "failed hydration does not repair the wrong-kind witness"
        );

        // A Goal witness is a different closed kind. Corrupting it to a
        // Memory kind must not let an exact cooled Goal reference hydrate.
        let goal_t = insert_unassigned_goal(pool, owner, "wrong-kind-goal").await?;
        let mut goal_source_draft = draft(None);
        goal_source_draft.refs = vec![goal_t];
        let goal_source = ingest_stamped(pool, &permit, &goal_source_draft, &[]).await?;
        let goal_source_t = goal_source.memory_id.into_inner();
        let goal_source_key = cold_object_key(goal_source_t);
        cool_one(pool, &owner, &cold, goal_source_t, &goal_source_key).await?;
        sqlx::query("DELETE FROM proxima_core.goal WHERE t = $1")
            .bind(goal_t)
            .execute(pool)
            .await?;
        assert_eq!(
            erased_pin_target_kind(pool, goal_t).await?,
            Some("goal".to_owned())
        );
        change_witness_kind(pool, goal_t, "fact").await?;
        let mut tx = pool.begin().await?;
        let err = hydrate_one_in_tx(
            &mut tx,
            &core_pg_sidecars(),
            &surfaces(),
            &cold,
            goal_source_t,
            owner.stored_owner_id(),
            &non_embeddable_schemas(),
        )
        .await?
        .expect_err("wrong-kind Goal witness must not be restored");
        tx.rollback().await?;
        assert_eq!(err, ColdRejection::InvalidObject);
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*)::bigint FROM proxima_core.cooled WHERE t = $1",
            )
            .bind(goal_source_t)
            .fetch_one(pool)
            .await?,
            1,
            "failed Goal hydration leaves its cooled locator"
        );
        assert_eq!(
            erased_pin_target_kind(pool, goal_t).await?,
            Some("fact".to_owned()),
            "failed hydration does not repair the corrupted witness"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("unknown/wrong-kind witness rejection failed");
}

/// The stamp is what a cold record must carry a dump for, so a stamped table
/// with no row is a divergence between the memory row's account of itself and
/// the physical state. Cooling it would mint an object whose dump list can
/// never equal its stamp — hydratable never, discoverable only much later.
/// The forget refuses it instead, while the memory is still whole.
#[tokio::test]
async fn a_stamped_sidecar_with_no_row_stops_the_forget() {
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

        // Stamped, never written: exactly the state the writer and the
        // verifier used to disagree about.
        let written = ingest_stamped(pool, &permit, &draft(None), &[AGENT_NOTE.to_owned()]).await?;
        let t = written.memory_id.into_inner();

        let cold = MemoryColdStore::default();
        let key = cold_object_key(t);
        let mut tx = pool.begin().await?;
        let err = forget_memory(
            &mut tx,
            &core_pg_sidecars(),
            &surfaces(),
            &cold,
            &key,
            t,
            owner.stored_owner_id(),
        )
        .await
        .expect_err("a stamped table with no row cannot be dumped");
        tx.rollback().await?;
        assert!(
            matches!(&err, StorageError::ConstraintViolation(message)
                if message.contains(AGENT_NOTE) && message.contains("has no row")),
            "got {err:?}"
        );

        let still_hot: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.memory WHERE t = $1")
                .bind(t)
                .fetch_one(pool)
                .await?;
        assert_eq!(still_hot, 1, "the refused forget leaves the memory hot");
        assert!(
            cold.get(&key).await.is_err(),
            "the refused forget writes no cold object"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("stamped-without-row forget test failed");
}

/// A locator written before the integrity witness existed carries neither a
/// digest nor the split pin arrays, and its object predates the stamped cold
/// format. Nothing can prove those bytes belong to this admission, so the
/// plan refuses it as unsupported and leaves the locator and the object
/// exactly as it found them. The three tests this replaces asserted the
/// permissive legacy admission that refusal removed.
#[tokio::test]
async fn a_legacy_cooled_locator_is_unsupported_and_untouched() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let cold = Arc::new(MemoryColdStore::default());
        let pg = PgStorage::connect(&url).await?.with_cold(cold.clone());
        pg.run_migrations().await?;
        let pool = pg.pool_for_tests().clone();
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let source = pg.ingest_fact_atomic(&permit, &draft(None), None).await?;
        let source_t = source.memory_id.into_inner();
        cool_one(
            &pool,
            &owner,
            cold.as_ref(),
            source_t,
            &cold_object_key(source_t),
        )
        .await?;
        let object = cold.get(&cold_object_key(source_t)).await?;
        make_legacy_cooled(&pool, source_t).await?;
        let locator: (Option<Vec<Uuid>>, Option<Vec<u8>>) =
            sqlx::query_as("SELECT origins, cold_digest FROM proxima_core.cooled WHERE t = $1")
                .bind(source_t)
                .fetch_one(&pool)
                .await?;

        let mut tx = pool.begin().await?;
        let rejection = hydrate_one_in_tx(
            &mut tx,
            &core_pg_sidecars(),
            &surfaces(),
            cold.as_ref(),
            source_t,
            owner.stored_owner_id(),
            &non_embeddable_schemas(),
        )
        .await?
        .expect_err("a legacy locator carries no integrity witness");
        tx.rollback().await?;
        assert_eq!(rejection, ColdRejection::UnsupportedObject);

        let after: (Option<Vec<Uuid>>, Option<Vec<u8>>) =
            sqlx::query_as("SELECT origins, cold_digest FROM proxima_core.cooled WHERE t = $1")
                .bind(source_t)
                .fetch_one(&pool)
                .await?;
        assert_eq!(after, locator, "a refusal leaves the locator alone");
        assert_eq!(
            cold.get(&cold_object_key(source_t)).await?,
            object,
            "a refusal leaves the cold object alone"
        );
        let hot: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.memory WHERE t = $1")
                .bind(source_t)
                .fetch_one(&pool)
                .await?;
        assert_eq!(hot, 0, "nothing is restored");
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("legacy cooled locator test failed");
}

/// Transfer changes the cooled source owner but not its sealed declaration.
/// Once that source's referenced target is hard-erased, the receiving owner
/// must still be able to perform the exact historical hydration through the
/// database-only witness.
#[tokio::test]
async fn transferred_cooled_source_hydrates_after_target_erase() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let destination = OwnerRef::Group(GroupId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let pool = pg.pool_for_tests();
        let cold = MemoryColdStore::default();

        let target = pg.ingest_fact_atomic(&permit, &draft(None), None).await?;
        let target_t = target.memory_id.into_inner();
        let mut source_draft = draft(Some(("transferred", "source")));
        source_draft.refs = vec![target_t];
        let source = pg.ingest_fact_atomic(&permit, &source_draft, None).await?;
        let source_t = source.memory_id.into_inner();
        let source_key = cold_object_key(source_t);
        cool_one(pool, &owner, &cold, source_t, &source_key).await?;
        let mut successor_draft = draft(Some(("transferred", "successor")));
        successor_draft.handle = Some(source.handle);
        let successor = pg
            .ingest_fact_atomic(&permit, &successor_draft, None)
            .await?;

        assert!(
            pg.transfer_to_owner(
                &permit,
                EntityId::Memory(successor.memory_id),
                destination,
                &transfer_surfaces(),
            )
            .await?
        );
        let transferred_owner: Uuid =
            sqlx::query_scalar("SELECT owner_id FROM proxima_core.cooled WHERE t = $1")
                .bind(source_t)
                .fetch_one(pool)
                .await?;
        assert_eq!(transferred_owner, destination.stored_owner_id());

        // The source is now owned by the destination, while its target is
        // still owned by the original owner. Hard erase leaves the exact
        // kind witness needed by the source's restoration seal.
        let mut tx = pool.begin().await?;
        erase_memory(&mut tx, &core_pg_sidecars(), &surfaces(), &owner, target_t).await?;
        tx.commit().await?;
        assert_eq!(
            erased_pin_target_kind(pool, target_t).await?,
            Some("fact".to_owned())
        );

        let mut tx = pool.begin().await?;
        hydrate_one_in_tx(
            &mut tx,
            &core_pg_sidecars(),
            &surfaces(),
            &cold,
            source_t,
            // The series changed hands while cold: the fence is the
            // DESTINATION's, and a stale source-owner permit must not reach it.
            destination.stored_owner_id(),
            &non_embeddable_schemas(),
        )
        .await?
        .expect("the record restores");
        tx.commit().await?;
        let restored: (Uuid, Vec<Uuid>) =
            sqlx::query_as("SELECT owner_id, refs FROM proxima_core.memory WHERE t = $1")
                .bind(source_t)
                .fetch_one(pool)
                .await?;
        assert_eq!(restored.0, destination.stored_owner_id());
        assert_eq!(restored.1, vec![target_t]);
        assert_eq!(
            erased_pin_target_kind(pool, target_t).await?,
            Some("fact".to_owned()),
            "hydration preserves the target witness"
        );
        assert_eq!(erased_pin_target_kind(pool, source_t).await?, None);
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("transferred cooled-source hydration test failed");
}

#[tokio::test]
async fn hydrate_rejects_cold_identity_and_sealed_pin_mismatch() {
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

        let identity = pg.ingest_fact_atomic(&permit, &draft(None), None).await?;
        let identity_t = identity.memory_id.into_inner();
        let identity_key = cold_object_key(identity_t);
        cool_one(pool, &owner, &cold, identity_t, &identity_key).await?;
        let mut identity_record = decode_record(&cold.get(&identity_key).await?)?;
        identity_record.row.t = Uuid::now_v7();
        let identity_bytes = encode_record(&identity_record)?;
        cold.put(&identity_key, &identity_bytes).await?;
        replace_cold_record(pool, &cold, identity_t, &identity_key, &identity_record).await?;
        let cooled_locator: (Uuid, String, Option<Vec<Uuid>>, Option<Vec<Uuid>>) = sqlx::query_as(
            "SELECT t, object_key, origins, refs FROM proxima_core.cooled WHERE t = $1",
        )
        .bind(identity_t)
        .fetch_one(pool)
        .await?;
        let mut tx = pool.begin().await?;
        let err = hydrate_one_in_tx(
            &mut tx,
            &core_pg_sidecars(),
            &surfaces(),
            &cold,
            identity_t,
            owner.stored_owner_id(),
            &non_embeddable_schemas(),
        )
        .await?
        .expect_err("a cold object with a different t must not hydrate");
        tx.rollback().await?;
        assert_eq!(err, ColdRejection::InvalidObject);
        let unchanged_locator: (Uuid, String, Option<Vec<Uuid>>, Option<Vec<Uuid>>) =
            sqlx::query_as(
                "SELECT t, object_key, origins, refs FROM proxima_core.cooled WHERE t = $1",
            )
            .bind(identity_t)
            .fetch_one(pool)
            .await?;
        assert_eq!(unchanged_locator, cooled_locator);
        assert_eq!(cold.get(&identity_key).await?, identity_bytes);
        assert_eq!(erased_pin_target_kind(pool, identity_t).await?, None);

        let live_target = pg.ingest_fact_atomic(&permit, &draft(None), None).await?;
        let live_target_t = live_target.memory_id.into_inner();
        let mut source_draft = draft(None);
        source_draft.refs = vec![live_target_t];
        let source = ingest_stamped(pool, &permit, &source_draft, &[]).await?;
        let source_t = source.memory_id.into_inner();
        let source_key = cold_object_key(source_t);
        cool_one(pool, &owner, &cold, source_t, &source_key).await?;
        let mut source_record = decode_record(&cold.get(&source_key).await?)?;
        source_record.row.refs = vec![Uuid::now_v7()];
        let source_bytes = encode_record(&source_record)?;
        cold.put(&source_key, &source_bytes).await?;
        replace_cold_record(pool, &cold, source_t, &source_key, &source_record).await?;
        let sealed_locator: (Option<Vec<Uuid>>, Option<Vec<Uuid>>) =
            sqlx::query_as("SELECT origins, refs FROM proxima_core.cooled WHERE t = $1")
                .bind(source_t)
                .fetch_one(pool)
                .await?;
        let mut tx = pool.begin().await?;
        let err = hydrate_one_in_tx(
            &mut tx,
            &core_pg_sidecars(),
            &surfaces(),
            &cold,
            source_t,
            owner.stored_owner_id(),
            &non_embeddable_schemas(),
        )
        .await?
        .expect_err("cold pins that differ from the cooled seal must not hydrate");
        tx.rollback().await?;
        assert_eq!(err, ColdRejection::InvalidObject);
        let unchanged_seal: (Option<Vec<Uuid>>, Option<Vec<Uuid>>) =
            sqlx::query_as("SELECT origins, refs FROM proxima_core.cooled WHERE t = $1")
                .bind(source_t)
                .fetch_one(pool)
                .await?;
        assert_eq!(unchanged_seal, sealed_locator);
        assert_eq!(cold.get(&source_key).await?, source_bytes);
        assert_eq!(erased_pin_target_kind(pool, live_target_t).await?, None);
        assert_eq!(erased_pin_target_kind(pool, source_t).await?, None);
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("cold identity/seal mismatch test failed");
}

#[tokio::test]
async fn witnessed_targets_cannot_be_reused_or_newly_pinned() {
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
        let target = pg.ingest_fact_atomic(&permit, &draft(None), None).await?;
        let target_t = target.memory_id.into_inner();
        let mut tx = pool.begin().await?;
        erase_memory(&mut tx, &core_pg_sidecars(), &surfaces(), &owner, target_t).await?;
        tx.commit().await?;
        assert_eq!(
            erased_pin_target_kind(pool, target_t).await?,
            Some("fact".to_owned())
        );

        let before_memory: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.memory")
                .fetch_one(pool)
                .await?;
        let before_heads: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.memory_head")
                .fetch_one(pool)
                .await?;
        let mut source = draft(None);
        source.refs = vec![target_t];
        let err = pg
            .ingest_fact_atomic(&permit, &source, None)
            .await
            .expect_err("a new source cannot pin a witnessed target");
        assert!(
            err.to_string().contains("does not exist")
                || err.to_string().contains("erased")
                || err.to_string().contains("23503"),
            "got: {err}"
        );
        let after_memory: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.memory")
                .fetch_one(pool)
                .await?;
        let after_heads: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.memory_head")
                .fetch_one(pool)
                .await?;
        assert_eq!(
            after_memory, before_memory,
            "new-source rejection is atomic"
        );
        assert_eq!(
            after_heads, before_heads,
            "new-source rejection leaves no head"
        );

        // Direct fixture SQL attempts to recycle the erased t itself. Use a
        // live head so the failure reaches the target collision guard rather
        // than the unrelated head-shape backstop.
        let holder = pg.ingest_fact_atomic(&permit, &draft(None), None).await?;
        let err = sqlx::query(
            "INSERT INTO proxima_core.memory
                (handle, t, kind, owner_id, schema_id, source_id, ingest_key,
                 blob_id, content_id, origins, refs, sidecar_tables)
             VALUES ($1, $2, 'fact', $3, 'core/test-fact-v1', NULL, NULL,
                     NULL, NULL, '{}', '{}', '{}')",
        )
        .bind(holder.handle)
        .bind(target_t)
        .bind(owner.stored_owner_id())
        .execute(pool)
        .await
        .expect_err("a witnessed t cannot be recycled as a new row");
        assert!(
            err.to_string().contains("erased") || err.to_string().contains("23505"),
            "got: {err}"
        );
        let reused: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.memory WHERE t = $1")
                .bind(target_t)
                .fetch_one(pool)
                .await?;
        assert_eq!(reused, 0);
        assert_eq!(
            erased_pin_target_kind(pool, target_t).await?,
            Some("fact".to_owned())
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("witnessed target reuse test failed");
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

        let fact = pg.ingest_fact_atomic(&permit, &draft(None), None).await?;
        let abs = pg
            .ingest_fact_atomic(
                &permit,
                &derived_abstraction(EntityKind::Fact, fact.memory_id.into_inner()),
                None,
            )
            .await
            .expect("A from Fact");
        let abs2 = pg
            .ingest_fact_atomic(
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
        let mixed_abs = pg
            .ingest_fact_atomic(&permit, &mixed, None)
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

        let err = pg
            .ingest_fact_atomic(
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

        pg.ingest_fact_atomic(
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
        let fact = pg.ingest_fact_atomic(&permit, &draft(None), None).await?;
        let abs = pg
            .ingest_fact_atomic(
                &permit,
                &derived_abstraction(EntityKind::Fact, fact.memory_id.into_inner()),
                None,
            )
            .await?;
        let abs2 = pg
            .ingest_fact_atomic(
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
        let written = pg.ingest_fact_atomic(&permit, &draft(None), None).await?;
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

        // Erase uses the same lifecycle lock and therefore waits for the
        // in-flight forget to finish its cold PUT. This linearization is
        // deliberate: the subsequent erase sees the cooled row and records
        // the hard-erase witness itself.
        let erase_pool = pool.clone();
        let erase_owner = owner;
        let erase = tokio::spawn(async move {
            let mut erase_tx = erase_pool.begin().await.expect("erase transaction");
            let plan = erase_memory(
                &mut erase_tx,
                &core_pg_sidecars(),
                &surfaces(),
                &erase_owner,
                t,
            )
            .await
            .expect("erase succeeds after waiting for forget");
            erase_tx.commit().await.expect("erase commit");
            plan
        });

        // The PUT completes only after erase has been queued behind the
        // lifecycle lock. Forget wins this controlled linearization.
        cold.release_first_put.add_permits(1);
        forget.await?.expect("the record restores");
        let plan = erase.await?;
        let purge = purge_cold_objects_after_commit(&pool, cold.as_ref(), &plan).await;
        assert!(purge.purged >= 1, "erase must purge the cooled payload");
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
        let written = pg.ingest_fact_atomic(&permit, &draft(None), None).await?;
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
        hydrate_one_in_tx(
            &mut tx,
            &core_pg_sidecars(),
            &surfaces(),
            cold.as_ref(),
            t,
            owner.stored_owner_id(),
            &non_embeddable_schemas(),
        )
        .await?
        .expect("the record restores");
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
        let written = pg.ingest_fact_atomic(&permit, &sourced, None).await?;
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
        let replay = pg
            .ingest_fact_atomic(&permit, &replay_input, None)
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
        pg.ingest_fact_atomic(&permit, &draft(None), None).await?;
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
        let f1 = pg.ingest_fact_atomic(&p1, &draft(None), None).await?;
        let f2 = pg.ingest_fact_atomic(&p2, &draft(None), None).await?;
        pg.ingest_fact_atomic(&p3, &draft(None), None).await?;
        let a1 = pg
            .ingest_fact_atomic(
                &p1,
                &derived_abstraction(EntityKind::Fact, f1.memory_id.into_inner()),
                None,
            )
            .await?;
        let a2 = pg
            .ingest_fact_atomic(
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
        let dep = pg.ingest_fact_atomic(&p3, &both, None).await?;

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
        let fact = pg.ingest_fact_atomic(&permit, &draft(None), None).await?;
        let abs = pg.ingest_fact_atomic(&permit,
            &derived_abstraction(EntityKind::Fact, fact.memory_id.into_inner()),
            None,
        )
        .await?;
        let a_t = abs.memory_id.into_inner();

        let mut tx_f = pool.begin().await?;
        // Hold the same lifecycle advisory that both forget and admission
        // use. The former row-only gate allowed an admission to take the
        // advisory first and deadlock when forget then tried to acquire it.
        sqlx::query(
            "SELECT pg_advisory_xact_lock(
                 hashtextextended('proxima-forget:' || $1::text, 0)
             )",
        )
        .bind(a_t)
        .execute(&mut *tx_f)
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
            "admit must wait on the lifecycle lock, not pass B2 against the still-hot A"
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

/// A production admission must take the target lifecycle lock before it can
/// lock the existing series head. Otherwise a forget holding the target lock
/// and this append can form a head/advisory cycle.
#[tokio::test]
async fn admission_locks_pins_before_series_head() {
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
        let origin = pg.ingest_fact_atomic(&permit, &draft(None), None).await?;
        // T is itself the prior version on H. The next admission uses the
        // same handle and references T, so forget(T)'s head repair and the
        // writer's head lock are the actual overlapping production paths.
        let target = pg
            .ingest_fact_atomic(
                &permit,
                &derived_abstraction(EntityKind::Fact, origin.memory_id.into_inner()),
                None,
            )
            .await?;
        let target_t = target.memory_id.into_inner();
        let mut append = derived_abstraction(EntityKind::Fact, target_t);
        append.handle = Some(target.handle);
        append.derived_from = vec![EdgeEndpoint::memory(
            EntityKind::Fact,
            proxima_core::MemoryId::new(origin.memory_id.into_inner()),
        )];
        append.refs = vec![target_t];

        let cold = Arc::new(BlockingPutCold {
            inner: MemoryColdStore::default(),
            first_put_entered: tokio::sync::Semaphore::new(0),
            release_first_put: tokio::sync::Semaphore::new(0),
            puts: AtomicUsize::new(0),
            deletes: AtomicUsize::new(0),
        });
        let forget_pool = pool.clone();
        let forget_cold = Arc::clone(&cold);
        let forget_owner = owner;
        let forget = tokio::spawn(async move {
            forget_memory_oneshot(
                &forget_pool,
                &core_pg_sidecars(),
                &surfaces(),
                forget_cold.as_ref(),
                &cold_object_key(target_t),
                target_t,
                forget_owner.stored_owner_id(),
            )
            .await
        });
        cold.first_put_entered.acquire().await?.forget();

        let writer_pg = pg.clone();
        let writer = tokio::spawn(async move {
            let writer_permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
            writer_pg
                .ingest_fact_atomic(&writer_permit, &append, None)
                .await
        });
        wait_for_advisory_waiters(&pool, 1).await?;

        // The writer is blocked on target_t's lifecycle lock and therefore
        // must not yet hold the source series head row lock.
        let mut head_probe = pool.begin().await?;
        sqlx::query("SELECT 1 FROM proxima_core.memory_head WHERE handle = $1 FOR UPDATE NOWAIT")
            .bind(target.handle)
            .execute(&mut *head_probe)
            .await
            .expect("admission must not lock the series head before its pin lock");
        head_probe.rollback().await?;

        cold.release_first_put.add_permits(1);
        let (forget_result, writer_result) =
            tokio::time::timeout(std::time::Duration::from_secs(10), async {
                tokio::join!(forget, writer)
            })
            .await?;
        forget_result??;
        let written = writer_result??;
        assert!(!written.idempotent_replay);
        assert_eq!(written.handle, target.handle);
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM proxima_core.memory WHERE handle = $1",
            )
            .bind(target.handle)
            .fetch_one(&pool)
            .await?,
            1
        );
        assert_eq!(
            sqlx::query_scalar::<_, Option<Uuid>>(
                "SELECT t FROM proxima_core.memory_head WHERE handle = $1",
            )
            .bind(target.handle)
            .fetch_one(&pool)
            .await?,
            Some(written.memory_id.into_inner())
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM proxima_core.cooled WHERE t = $1",)
                .bind(target_t)
                .fetch_one(&pool)
                .await?,
            1,
            "forget linearizes before the append and leaves T cooled"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("admission/head/forget ordering test failed");
}

/// Even a same-handle append with no declared pins must arbitrate the
/// existing head before taking its row lock. A forget holding that head's
/// lifecycle lock may remove the old head; the append retries from the
/// post-forget state and installs the new head without a cycle.
#[tokio::test]
async fn admission_locks_existing_head_without_declared_pins() {
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
        let prior = pg.ingest_fact_atomic(&permit, &draft(None), None).await?;
        let prior_t = prior.memory_id.into_inner();
        let handle = prior.handle;
        let cold = Arc::new(BlockingPutCold {
            inner: MemoryColdStore::default(),
            first_put_entered: tokio::sync::Semaphore::new(0),
            release_first_put: tokio::sync::Semaphore::new(0),
            puts: AtomicUsize::new(0),
            deletes: AtomicUsize::new(0),
        });
        let forget_pool = pool.clone();
        let forget_cold = Arc::clone(&cold);
        let forget = tokio::spawn(async move {
            forget_memory_oneshot(
                &forget_pool,
                &core_pg_sidecars(),
                &surfaces(),
                forget_cold.as_ref(),
                &cold_object_key(prior_t),
                prior_t,
                owner.stored_owner_id(),
            )
            .await
        });
        cold.first_put_entered.acquire().await?.forget();

        let writer_pg = pg.clone();
        let writer = tokio::spawn(async move {
            let writer_permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
            let mut append = draft(None);
            append.handle = Some(handle);
            writer_pg
                .ingest_fact_atomic(&writer_permit, &append, None)
                .await
        });
        wait_for_advisory_waiters(&pool, 1).await?;

        // The append is waiting on prior_t and has not yet touched the shared
        // head row. This NOWAIT probe is the direct inversion guard.
        let mut head_probe = pool.begin().await?;
        sqlx::query("SELECT 1 FROM proxima_core.memory_head WHERE handle = $1 FOR UPDATE NOWAIT")
            .bind(handle)
            .execute(&mut *head_probe)
            .await
            .expect("append must not lock an existing head before its lifecycle set");
        head_probe.rollback().await?;

        cold.release_first_put.add_permits(1);
        let (forget_result, writer_result) =
            tokio::time::timeout(std::time::Duration::from_secs(10), async {
                tokio::join!(forget, writer)
            })
            .await?;
        forget_result??;
        let written = writer_result??;
        assert!(!written.idempotent_replay);
        assert_eq!(written.handle, handle);
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM proxima_core.memory WHERE handle = $1",
            )
            .bind(handle)
            .fetch_one(&pool)
            .await?,
            1,
            "the retry leaves one new hot version"
        );
        assert_eq!(
            sqlx::query_scalar::<_, Option<Uuid>>(
                "SELECT t FROM proxima_core.memory_head WHERE handle = $1",
            )
            .bind(handle)
            .fetch_one(&pool)
            .await?,
            Some(written.memory_id.into_inner())
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM proxima_core.cooled WHERE t = $1")
                .bind(prior_t)
                .fetch_one(&pool)
                .await?,
            1,
            "the prior head is cooled before the append retry"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("same-handle head-only admission ordering test failed");
}

/// Citation persistence also follows the lifecycle lock. A series erase may
/// therefore lock and collect its cited blob before a concurrent append
/// attempts to reuse that blob, without an advisory/blob inversion.
#[tokio::test]
async fn citation_reuse_and_series_erase_share_one_lock_order() {
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
        let mut cited_draft = draft(None);
        cited_draft.citation = Some(
            CitationSpec::v1(
                "core/test-cited-object-v1",
                [0xA7; 32],
                "core/test-citation-mapping-v1",
            )
            .into(),
        );
        let first = pg.ingest_fact_atomic(&permit, &cited_draft, None).await?;
        let mut second_draft = cited_draft.clone();
        second_draft.handle = Some(first.handle);
        let second = pg.ingest_fact_atomic(&permit, &second_draft, None).await?;
        let blob_id = first.cited_object_id.expect("first citation blob");

        // Hold the complete series handle and lifecycle set in the real erase
        // transaction, but pause before invoking the erase body. The writer
        // must queue on the handle before T; the blob remains freely lockable,
        // proving it did not reach citation/blob persistence before lifecycle
        // arbitration.
        let erase_ready = Arc::new(tokio::sync::Semaphore::new(0));
        let erase_release = Arc::new(tokio::sync::Semaphore::new(0));
        let erase_pool = pool.clone();
        let erase_owner = owner;
        let erase_ready_signal = Arc::clone(&erase_ready);
        let erase_release_wait = Arc::clone(&erase_release);
        let first_t = first.memory_id.into_inner();
        let second_t = second.memory_id.into_inner();
        let series_handle = second.handle;
        let erase = tokio::spawn(async move {
            let mut tx = erase_pool
                .begin()
                .await
                .map_err(|error| StorageError::Internal(error.to_string()))?;
            lock_memory_handles_tx(&mut tx, &[series_handle]).await?;
            lock_lifecycle_targets_tx(&mut tx, &[first_t, second_t]).await?;
            erase_ready_signal.add_permits(1);
            erase_release_wait
                .acquire()
                .await
                .map_err(|error| StorageError::Internal(error.to_string()))?
                .forget();
            let result = erase_memory_series(
                &mut tx,
                &core_pg_sidecars(),
                &surfaces(),
                &erase_owner,
                &[first_t],
            )
            .await?;
            tx.commit()
                .await
                .map_err(|error| StorageError::Internal(error.to_string()))?;
            Ok::<_, StorageError>(result)
        });
        erase_ready.acquire().await?.forget();
        let mut writer_draft = cited_draft;
        writer_draft.handle = Some(second.handle);
        writer_draft.refs = vec![first_t];
        let writer_pg = pg.clone();
        let writer = tokio::spawn(async move {
            let writer_permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
            writer_pg
                .ingest_fact_atomic(&writer_permit, &writer_draft, None)
                .await
        });
        wait_for_advisory_waiters(&pool, 1).await?;

        let mut blob_probe = pool.begin().await?;
        sqlx::query("SELECT 1 FROM proxima_core.blob WHERE blob_id = $1 FOR UPDATE NOWAIT")
            .bind(blob_id)
            .execute(&mut *blob_probe)
            .await
            .expect("writer must not touch the citation blob before its lifecycle lock");
        blob_probe.rollback().await?;
        erase_release.add_permits(1);

        let (erase_result, writer_result) =
            tokio::time::timeout(std::time::Duration::from_secs(10), async {
                tokio::join!(erase, writer)
            })
            .await?;
        let (erased_count, plan) = erase_result??;
        assert_eq!(erased_count, 2, "the whole series was erased");
        assert!(
            plan.object_keys().is_empty(),
            "hot series has no cold purge keys"
        );
        let writer_error = writer_result?.expect_err("erased ref must reject the append");
        assert!(
            !writer_error.to_string().contains("40P01"),
            "writer must fail cleanly after erase, not deadlock: {writer_error}"
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM proxima_core.memory WHERE handle = $1",
            )
            .bind(second.handle)
            .fetch_one(&pool)
            .await?,
            0,
            "rejected admission leaves no orphan memory"
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM proxima_core.blob WHERE schema_id = $1 AND content_hash = $2",
            )
            .bind("core/test-cited-object-v1")
            .bind(vec![0xA7_u8; 32])
            .fetch_one(&pool)
            .await?,
            0,
            "series erase garbage-collects the shared blob and rejected reuse adds none"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("citation/blob and series-erase ordering test failed");
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
        let written = pg.ingest_fact_atomic(&permit, &draft(None), None).await?;
        let t = written.memory_id.into_inner();
        let mut conn = pool.acquire().await?;
        let snapshot = snapshot_hot(&mut conn, &core_pg_sidecars(), &surfaces(), t).await?;
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

#[tokio::test]
async fn stale_source_erase_does_not_lock_transferred_series() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let source = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let destination = OwnerRef::Group(GroupId::new(Uuid::now_v7()));
        let source_permit = OwnerWritePermit::new_for_tests(source, AccessKind::Fact);
        let pool = pg.pool_for_tests();
        let first = pg
            .ingest_fact_atomic(&source_permit, &draft(None), None)
            .await?;
        let first_t = first.memory_id.into_inner();
        assert!(
            pg.transfer_to_owner(
                &source_permit,
                EntityId::Memory(first.memory_id),
                destination,
                &transfer_surfaces(),
            )
            .await?
        );

        // Model the source erase after its earlier ownership probe but after
        // transfer won. It must reject before taking the destination's handle
        // lock; Rust-level retry errors leave this transaction usable and its
        // already-acquired advisory locks live until rollback.
        let mut stale_source_erase = pool.begin().await?;
        let err = lock_admissions_for_erase(&mut stale_source_erase, &source, &[first_t])
            .await
            .expect_err("a transferred admission is no longer the source owner's footprint");
        assert!(
            matches!(err, StorageError::Retryable(_)),
            "stale source erase must retry: {err:?}"
        );

        let mut append_draft = draft(None);
        append_draft.handle = Some(first.handle);
        let destination_permit = OwnerWritePermit::new_for_tests(destination, AccessKind::Fact);
        let mut destination_append = pool.begin().await?;
        sqlx::query("SET LOCAL lock_timeout = '1s'")
            .execute(destination_append.as_mut())
            .await?;
        let appended = ingest_fact_timeseries(
            &mut destination_append,
            destination_permit.owner(),
            &append_draft,
            &[],
            &[],
            &[],
            None,
        )
        .await?;
        assert_eq!(appended.handle, first.handle);
        destination_append.commit().await?;
        stale_source_erase.rollback().await?;
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("stale source erase ownership fence test failed");
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
        let written = pg.ingest_fact_atomic(&permit, &draft(None), None).await?;
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
        assert_eq!(
            erased_pin_target_kind(pool, t).await?,
            None,
            "a rolled-back erase must not leave a historical witness"
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
        let written = pg.ingest_fact_atomic(&permit, &draft(None), None).await?;
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

/// The `ForgetRule` declarations on `ingest_keys` and `memory_head`, pinned as
/// BEHAVIOUR.
///
/// Both declare `Keep { why }`, and a declaration that only changes a string is
/// worth nothing, so this asserts the shipped behaviour they claim, in both
/// directions:
///
/// - a cool leaves the receipt and REWINDS the head to the surviving
///   newest `t`;
/// - an erase of the last version removes the receipt and takes the head
///   with it.
///
/// Setting either to `DeleteWithMemory` makes this fail, which is the point:
/// the forget leg reads the contract, so the declaration is falsifiable.
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
        let first = pg
            .ingest_fact_atomic(&permit, &draft(Some(("src", "k1"))), None)
            .await?;
        let mut later = draft(Some(("src", "k2")));
        later.handle = Some(first.handle);
        let second = pg.ingest_fact_atomic(&permit, &later, None).await?;
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
    result.expect("the forget declarations must match shipped behaviour");
}

/// A `ForgetRule::Keep` sidecar that is not owner-pinned stops the forget
/// instead of being deleted by it.
///
/// The stamp walk in `delete_memory_dependents` must not map `Kept` onto the
/// key column `"t"` the way it maps `Dumped`: a surface declared as one the
/// forget does not touch would be deleted by the forget, silently, with the
/// declaration sitting three files away saying otherwise.
///
/// Core ships one `Keep` memory sidecar, `mcp_call_logged_v1`, and its rows
/// survive only because it is ALSO `TransferRule::RetainAtSource` and
/// therefore `pg_sidecar!(owner_pinned: true)`, which the walk skips a few
/// lines earlier for an entirely different reason. Remove the pinning and the
/// `Keep` bought nothing.
///
/// `freeze_against` now refuses that combination outright (see
/// `check_keep_is_owner_pinned`), so this state is unreachable through a
/// registry. It is reachable through `OwnerSurfaces::from_surfaces`, the
/// public test seam, which is what this test uses: `agent_note_v1` is a real
/// registered non-owner-pinned memory sidecar, re-declared here as `Keep`.
///
/// The assertion is that the forget REFUSES and the rows survive.
#[tokio::test]
async fn a_kept_sidecar_that_is_not_owner_pinned_stops_the_forget() {
    use proxima_core::flavor::{ForgetRule, Surface};

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

        // Flavor #0's surfaces with exactly one field changed: the note
        // sidecar now declares that the forget leaves it alone.
        let mut declared: Vec<Surface> = proxima_core::FLAVOR_0.all_surfaces().collect();
        let mut rewritten = 0;
        for surface in &mut declared {
            if surface.table == AGENT_NOTE {
                surface.forget = ForgetRule::Keep {
                    why: "a fixture: a declaration the walk must not ignore",
                };
                rewritten += 1;
            }
        }
        assert_eq!(rewritten, 1, "the note sidecar is a declared surface");
        let kept_surfaces = proxima_core::owner_inverse::OwnerSurfaces::from_surfaces(declared);

        let written = ingest_stamped(pool, &permit, &draft(None), &[AGENT_NOTE.to_owned()]).await?;
        let t = written.memory_id.into_inner();
        sqlx::query(
            "INSERT INTO proxima_core.agent_note_v1 (t, note_id, title, body, tags)
             VALUES ($1, $1, 'n', 'kept', ARRAY['tag'])",
        )
        .bind(t)
        .execute(pool)
        .await?;

        let cold = MemoryColdStore::default();
        let key = cold_object_key(t);
        let mut tx = pool.begin().await?;
        let outcome = forget_memory(
            &mut tx,
            &core_pg_sidecars(),
            &kept_surfaces,
            &cold,
            &key,
            t,
            owner.stored_owner_id(),
        )
        .await;
        tx.rollback().await?;

        let err = outcome.expect_err(
            "a Keep declaration the substrate cannot honour must stop the forget, \
             not be quietly overridden by it",
        );
        let rendered = err.to_string();
        assert!(
            rendered.contains(AGENT_NOTE) && rendered.contains("ForgetRule::Keep"),
            "the refusal must name the table and the declaration it could not \
             honour; got {rendered}"
        );

        let notes: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.agent_note_v1 WHERE t = $1",
        )
        .bind(t)
        .fetch_one(pool)
        .await?;
        assert_eq!(notes, 1, "the rows the declaration keeps are still there");
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("a_kept_sidecar_that_is_not_owner_pinned_stops_the_forget failed");
}

/// A rehydrate re-files the embedding jobs the dump recorded, and the dump
/// records what the row HAD: the models it held vectors under. A row written
/// under a `Never` schema before the recipe was honoured has one, so the
/// models alone would restore the job the recipe exists to prevent — once
/// per hydrate, forever.
#[tokio::test]
async fn hydrate_files_no_embedding_job_for_a_never_schema() {
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
        let cold = MemoryColdStore::default();

        // `core/write-act-v1` declares `Never`; `core/agent-note-v1` declares
        // a unit. One flow, opposite answers — the control is what makes the
        // zero mean the gate rather than an empty table.
        let never = cool_then_hydrate(&pg, &owner, &permit, &cold, "core/write-act-v1").await?;
        let embeds = cool_then_hydrate(&pg, &owner, &permit, &cold, "core/agent-note-v1").await?;
        assert_eq!(never, 0, "a Never schema files no embedding job on hydrate");
        assert_eq!(embeds, 1, "an embeddable schema still restores its job");
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("hydrate_files_no_embedding_job_for_a_never_schema failed");
}

#[tokio::test]
async fn authorized_hydration_reports_typed_one_and_set_outcomes() {
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
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let foreign_owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let foreign_permit = OwnerWritePermit::new_for_tests(foreign_owner, AccessKind::Fact);
        let hot = pg.ingest_fact_atomic(&permit, &draft(None), None).await?;
        let hot_result = MemoryAuthoringPort::hydrate_memories(
            &pg,
            &permit,
            &[hot.memory_id],
        )
        .await?;
        assert!(hot_result.committed);
        assert_eq!(
            hot_result.outcomes[0].status,
            MemoryHydrationStatus::AlreadyHot
        );

        let cooled = pg.ingest_fact_atomic(&permit, &draft(None), None).await?;
        let cooled_t = cooled.memory_id.into_inner();
        cool_one(
            pool,
            &owner,
            cold.as_ref(),
            cooled_t,
            &cold_object_key(cooled_t),
        )
        .await?;
        let cooled_bytes = cold.get(&cold_object_key(cooled_t)).await?;
        let stored_digest: Vec<u8> =
            sqlx::query_scalar("SELECT cold_digest FROM proxima_core.cooled WHERE t = $1")
                .bind(cooled_t)
                .fetch_one(pool)
                .await?;
        assert_eq!(
            stored_digest,
            super::cold_digest(&cooled_bytes),
            "the cooled row witnesses the exact encoded cold bytes"
        );
        let hydrated = MemoryAuthoringPort::hydrate_memories(
            &pg,
            &permit,
            &[cooled.memory_id],
        )
        .await?;
        assert!(hydrated.committed);
        assert_eq!(
            hydrated.outcomes[0].status,
            MemoryHydrationStatus::Hydrated
        );
        assert_eq!(hydrated.outcomes[0].memory_id.into_inner(), cooled_t);

        let incomplete_seal = pg.ingest_fact_atomic(&permit, &draft(None), None).await?;
        let incomplete_t = incomplete_seal.memory_id.into_inner();
        let incomplete_key = cold_object_key(incomplete_t);
        cool_one(
            pool,
            &owner,
            cold.as_ref(),
            incomplete_t,
            &incomplete_key,
        )
        .await?;
        make_legacy_cooled(pool, incomplete_t).await?;
        let incomplete_result =
            MemoryAuthoringPort::hydrate_memories(&pg, &permit, &[incomplete_seal.memory_id])
                .await?;
        assert_eq!(
            incomplete_result.outcomes[0].status,
            MemoryHydrationStatus::UnsupportedColdObject,
            "a digest-bearing row with a NULL pin seal must not derive a new lock footprint"
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*)::bigint FROM proxima_core.cooled WHERE t = $1",
            )
            .bind(incomplete_t)
            .fetch_one(pool)
            .await?,
            1
        );

        let witness_target = pg.ingest_fact_atomic(&permit, &draft(None), None).await?;
        let witness_target_t = witness_target.memory_id.into_inner();
        let mut witness_source_draft = draft(None);
        witness_source_draft.refs = vec![witness_target_t];
        let witness_source = ingest_stamped(
            pool,
            &permit,
            &witness_source_draft,
            &[],
        )
        .await?;
        let witness_source_t = witness_source.memory_id.into_inner();
        let witness_source_key = cold_object_key(witness_source_t);
        cool_one(
            pool,
            &owner,
            cold.as_ref(),
            witness_source_t,
            &witness_source_key,
        )
        .await?;
        let mut erase_tx = pool.begin().await?;
        erase_memory(
            &mut erase_tx,
            &core_pg_sidecars(),
            &surfaces(),
            &owner,
            witness_target_t,
        )
        .await?;
        erase_tx.commit().await?;
        let witnessed = MemoryAuthoringPort::hydrate_memories(
            &pg,
            &permit,
            &[witness_source.memory_id],
        )
        .await?;
        assert!(witnessed.committed);
        assert_eq!(
            witnessed.outcomes[0].status,
            MemoryHydrationStatus::Hydrated
        );
        assert_eq!(witnessed.outcomes[0].preserved_witnesses, 1);

        let sidecar_mutations = [
            ("t", serde_json::Value::String("not-a-uuid".into())),
            ("note_id", serde_json::json!(17)),
            ("title", serde_json::Value::Null),
            (
                "t",
                serde_json::Value::String(Uuid::now_v7().to_string()),
            ),
        ];
        let mut sidecar_ids = Vec::new();
        for (field, value) in sidecar_mutations {
            let sidecar_source =
                ingest_stamped(pool, &permit, &draft(None), &[AGENT_NOTE.to_owned()]).await?;
            let sidecar_t = sidecar_source.memory_id.into_inner();
            sqlx::query(
                "INSERT INTO proxima_core.agent_note_v1 (t, note_id, title, body, tags)
                 VALUES ($1, $1, 'n', 'body', ARRAY['tag'])",
            )
            .bind(sidecar_t)
            .execute(pool)
            .await?;
            let sidecar_key = cold_object_key(sidecar_t);
            cool_one(
                pool,
                &owner,
                cold.as_ref(),
                sidecar_t,
                &sidecar_key,
            )
            .await?;
            let mut record = decode_record(&cold.get(&sidecar_key).await?)?;
            let (_, json) = record
                .sidecar_dumps
                .first_mut()
                .expect("the stamped sidecar is in the cold dump");
            let mut payload: serde_json::Value = serde_json::from_str(json)?;
            payload
                .as_object_mut()
                .expect("sidecar dump is an object")
                .insert(field.to_owned(), value);
            *json = payload.to_string();
            replace_cold_record(pool, cold.as_ref(), sidecar_t, &sidecar_key, &record).await?;
            sidecar_ids.push(sidecar_source.memory_id);
        }
        // A second registered, hydratable table with the same `t` is still
        // not part of this admission. The v6 stamp must reject it before the
        // restore can turn the valid dump into a new sidecar row.
        let extra_source =
            ingest_stamped(pool, &permit, &draft(None), &[AGENT_NOTE.to_owned()]).await?;
        let extra_t = extra_source.memory_id.into_inner();
        sqlx::query(
            "INSERT INTO proxima_core.agent_note_v1 (t, note_id, title, body, tags)
             VALUES ($1, $1, 'n', 'body', ARRAY['tag'])",
        )
        .bind(extra_t)
        .execute(pool)
        .await?;
        let extra_key = cold_object_key(extra_t);
        cool_one(pool, &owner, cold.as_ref(), extra_t, &extra_key).await?;
        let mut extra_record = decode_record(&cold.get(&extra_key).await?)?;
        extra_record.sidecar_dumps.push((
            WRITE_ACT.to_owned(),
            serde_json::json!({
                "t": extra_t.to_string(),
                "episode_id": Uuid::now_v7().to_string(),
            })
            .to_string(),
        ));
        replace_cold_record(pool, cold.as_ref(), extra_t, &extra_key, &extra_record).await?;
        sidecar_ids.push(extra_source.memory_id);

        let duplicate_source =
            ingest_stamped(pool, &permit, &draft(None), &[AGENT_NOTE.to_owned()]).await?;
        let duplicate_t = duplicate_source.memory_id.into_inner();
        sqlx::query(
            "INSERT INTO proxima_core.agent_note_v1 (t, note_id, title, body, tags)
             VALUES ($1, $1, 'n', 'body', ARRAY['tag'])",
        )
        .bind(duplicate_t)
        .execute(pool)
        .await?;
        let duplicate_key = cold_object_key(duplicate_t);
        cool_one(
            pool,
            &owner,
            cold.as_ref(),
            duplicate_t,
            &duplicate_key,
        )
        .await?;
        let mut duplicate_record = decode_record(&cold.get(&duplicate_key).await?)?;
        let duplicate_dump = duplicate_record
            .sidecar_dumps
            .first()
            .cloned()
            .expect("the stamped sidecar is in the cold dump");
        duplicate_record.sidecar_dumps.push(duplicate_dump);
        replace_cold_record(
            pool,
            cold.as_ref(),
            duplicate_t,
            &duplicate_key,
            &duplicate_record,
        )
        .await?;
        sidecar_ids.push(duplicate_source.memory_id);

        let owner_pinned_source =
            ingest_stamped(pool, &permit, &draft(None), &[AGENT_NOTE.to_owned()]).await?;
        let owner_pinned_t = owner_pinned_source.memory_id.into_inner();
        sqlx::query(
            "INSERT INTO proxima_core.agent_note_v1 (t, note_id, title, body, tags)
             VALUES ($1, $1, 'n', 'body', ARRAY['tag'])",
        )
        .bind(owner_pinned_t)
        .execute(pool)
        .await?;
        let owner_pinned_key = cold_object_key(owner_pinned_t);
        cool_one(
            pool,
            &owner,
            cold.as_ref(),
            owner_pinned_t,
            &owner_pinned_key,
        )
        .await?;
        let mut owner_pinned_record = decode_record(&cold.get(&owner_pinned_key).await?)?;
        owner_pinned_record.sidecar_dumps[0].0 = "proxima_core.mcp_call_logged_v1".into();
        replace_cold_record(
            pool,
            cold.as_ref(),
            owner_pinned_t,
            &owner_pinned_key,
            &owner_pinned_record,
        )
        .await?;
        sidecar_ids.push(owner_pinned_source.memory_id);
        let invalid_sidecars = MemoryAuthoringPort::hydrate_memories(
            &pg,
            &permit,
            &sidecar_ids,
        )
        .await?;
        assert!(!invalid_sidecars.committed);
        assert!(invalid_sidecars.outcomes.iter().all(|outcome| {
            matches!(
                outcome.status,
                MemoryHydrationStatus::InvalidColdObject
                    | MemoryHydrationStatus::UnsupportedColdSidecar
            )
        }));
        assert!(
            invalid_sidecars.outcomes.iter().any(|outcome| {
                outcome.status == MemoryHydrationStatus::UnsupportedColdSidecar
            }),
            "unsupported sidecar stamps need their own operator outcome"
        );
        let sidecar_cool_count: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.cooled WHERE t = ANY($1::uuid[])",
        )
        .bind(
            sidecar_ids
                .iter()
                .copied()
                .map(proxima_core::MemoryId::into_inner)
                .collect::<Vec<_>>(),
        )
        .fetch_one(pool)
        .await?;
        assert_eq!(sidecar_cool_count, 7);
        let sidecar_row_count: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.agent_note_v1 WHERE t = ANY($1::uuid[])",
        )
        .bind(
            sidecar_ids
                .iter()
                .copied()
                .map(proxima_core::MemoryId::into_inner)
                .collect::<Vec<_>>(),
        )
        .fetch_one(pool)
        .await?;
        assert_eq!(sidecar_row_count, 0, "failed hydration restores no sidecar");

        let extra_row_count: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.write_act_v1 WHERE t = ANY($1::uuid[])",
        )
        .bind(
            sidecar_ids
                .iter()
                .copied()
                .map(proxima_core::MemoryId::into_inner)
                .collect::<Vec<_>>(),
        )
        .fetch_one(pool)
        .await?;
        assert_eq!(extra_row_count, 0, "a valid extra sidecar must not be injected");

        let missing = pg.ingest_fact_atomic(&permit, &draft(None), None).await?;
        let missing_t = missing.memory_id.into_inner();
        let missing_key = cold_object_key(missing_t);
        cool_one(pool, &owner, cold.as_ref(), missing_t, &missing_key).await?;
        cold.delete(&missing_key).await?;
        let missing_result = MemoryAuthoringPort::hydrate_memories(
            &pg,
            &permit,
            &[missing.memory_id],
        )
        .await?;
        assert!(!missing_result.committed);
        assert_eq!(
            missing_result.outcomes[0].status,
            MemoryHydrationStatus::MissingColdObject
        );

        let corrupt = pg.ingest_fact_atomic(&permit, &draft(None), None).await?;
        let corrupt_t = corrupt.memory_id.into_inner();
        let corrupt_key = cold_object_key(corrupt_t);
        cool_one(pool, &owner, cold.as_ref(), corrupt_t, &corrupt_key).await?;
        cold.put(&corrupt_key, &[5, 0, 1]).await?;
        let corrupt_result = MemoryAuthoringPort::hydrate_memories(
            &pg,
            &permit,
            &[corrupt.memory_id],
        )
        .await?;
        assert!(!corrupt_result.committed);
        assert_eq!(
            corrupt_result.outcomes[0].status,
            MemoryHydrationStatus::InvalidColdObject
        );

        let legacy = pg.ingest_fact_atomic(&permit, &draft(None), None).await?;
        let legacy_t = legacy.memory_id.into_inner();
        let legacy_key = cold_object_key(legacy_t);
        cool_one(pool, &owner, cold.as_ref(), legacy_t, &legacy_key).await?;
        let legacy_record = decode_record(&cold.get(&legacy_key).await?)?;
        let legacy_bytes = encode_v5_without_sidecar_stamp(&legacy_record)?;
        replace_cold_bytes(pool, &cold, legacy_t, &legacy_key, &legacy_bytes).await?;
        let legacy_result = MemoryAuthoringPort::hydrate_memories(&pg, &permit, &[legacy.memory_id])
            .await?;
        assert!(!legacy_result.committed);
        assert_eq!(
            legacy_result.outcomes[0].status,
            MemoryHydrationStatus::UnsupportedColdObject,
            "pre-v6 objects fail closed because their sidecar stamp is absent"
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*)::bigint FROM proxima_core.cooled WHERE t = $1",
            )
            .bind(legacy_t)
            .fetch_one(pool)
            .await?,
            1,
            "legacy rejection leaves the cooled locator intact"
        );

        let unwitnessed = pg.ingest_fact_atomic(&permit, &draft(None), None).await?;
        let unwitnessed_t = unwitnessed.memory_id.into_inner();
        let unwitnessed_key = cold_object_key(unwitnessed_t);
        cool_one(
            pool,
            &owner,
            cold.as_ref(),
            unwitnessed_t,
            &unwitnessed_key,
        )
        .await?;
        let mut unwitnessed_tx = pool.begin().await?;
        sqlx::query("ALTER TABLE proxima_core.cooled DISABLE TRIGGER cooled_append_only")
            .execute(unwitnessed_tx.as_mut())
            .await?;
        sqlx::query("UPDATE proxima_core.cooled SET cold_digest = NULL WHERE t = $1")
            .bind(unwitnessed_t)
            .execute(unwitnessed_tx.as_mut())
            .await?;
        sqlx::query("ALTER TABLE proxima_core.cooled ENABLE TRIGGER cooled_append_only")
            .execute(unwitnessed_tx.as_mut())
            .await?;
        unwitnessed_tx.commit().await?;
        let unwitnessed_result =
            MemoryAuthoringPort::hydrate_memories(&pg, &permit, &[unwitnessed.memory_id]).await?;
        assert_eq!(
            unwitnessed_result.outcomes[0].status,
            MemoryHydrationStatus::UnsupportedColdObject
        );

        let future = pg.ingest_fact_atomic(&permit, &draft(None), None).await?;
        let future_t = future.memory_id.into_inner();
        let future_key = cold_object_key(future_t);
        cool_one(pool, &owner, cold.as_ref(), future_t, &future_key).await?;
        let mut future_bytes = cold.get(&future_key).await?;
        future_bytes[0] = COLD_FORMAT_VERSION.saturating_add(1);
        replace_cold_bytes(pool, &cold, future_t, &future_key, &future_bytes).await?;
        let future_result = MemoryAuthoringPort::hydrate_memories(&pg, &permit, &[future.memory_id])
            .await?;
        assert_eq!(
            future_result.outcomes[0].status,
            MemoryHydrationStatus::UnsupportedColdObject
        );

        let foreign_result = MemoryAuthoringPort::hydrate_memories(
            &pg,
            &foreign_permit,
            &[missing.memory_id],
        )
        .await?;
        assert!(foreign_result.committed);
        assert_eq!(
            foreign_result.outcomes[0].status,
            MemoryHydrationStatus::NotFound
        );

        let atomic_a = pg.ingest_fact_atomic(&permit, &draft(None), None).await?;
        let atomic_b = pg.ingest_fact_atomic(&permit, &draft(None), None).await?;
        let first_atomic_t = atomic_a.memory_id.into_inner();
        let second_atomic_t = atomic_b.memory_id.into_inner();
        let first_atomic_key = cold_object_key(first_atomic_t);
        let second_atomic_key = cold_object_key(second_atomic_t);
        cool_one(
            pool,
            &owner,
            cold.as_ref(),
            first_atomic_t,
            &first_atomic_key,
        )
        .await?;
        cool_one(
            pool,
            &owner,
            cold.as_ref(),
            second_atomic_t,
            &second_atomic_key,
        )
        .await?;
        cold.delete(&second_atomic_key).await?;
        let atomic = MemoryAuthoringPort::hydrate_memories(
            &pg,
            &permit,
            &[atomic_a.memory_id, atomic_b.memory_id],
        )
        .await?;
        assert!(!atomic.committed);
        assert_eq!(
            atomic.outcomes[0].status,
            MemoryHydrationStatus::NotAttempted
        );
        assert_eq!(
            atomic.outcomes[1].status,
            MemoryHydrationStatus::MissingColdObject
        );
        let cooled_count: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.cooled WHERE t = ANY($1::uuid[]) AND owner_id = $2",
        )
        .bind([first_atomic_t, second_atomic_t])
        .bind(owner.stored_owner_id())
        .fetch_one(pool)
        .await?;
        assert_eq!(cooled_count, 2, "a failed set hydrates no partial subset");
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("authorized hydration outcome test failed");
}

/// The target erase is queued before hydration while a gate holds their
/// lifecycle lock. Hydration must report the witness created by that erase,
/// not a count sampled before it waited for the lock.
#[tokio::test]
async fn authorized_hydration_reports_witness_count_after_erase_race() {
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
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);

        let target = pg.ingest_fact_atomic(&permit, &draft(None), None).await?;
        let target_t = target.memory_id.into_inner();
        let mut source_draft = draft(None);
        source_draft.refs = vec![target_t];
        let source = ingest_stamped(pool, &permit, &source_draft, &[]).await?;
        let source_t = source.memory_id.into_inner();
        let source_key = cold_object_key(source_t);
        cool_one(pool, &owner, cold.as_ref(), source_t, &source_key).await?;

        let mut gate = pool.begin().await?;
        lock_lifecycle_targets_tx(&mut gate, &[target_t]).await?;

        let erase_pool = pool.clone();
        let erase_owner = owner;
        let erase_task = tokio::spawn(async move {
            let mut tx = erase_pool
                .begin()
                .await
                .map_err(|error| StorageError::Internal(error.to_string()))?;
            erase_memory(
                &mut tx,
                &core_pg_sidecars(),
                &surfaces(),
                &erase_owner,
                target_t,
            )
            .await?;
            tx.commit()
                .await
                .map_err(|error| StorageError::Internal(error.to_string()))?;
            Ok::<(), StorageError>(())
        });
        wait_for_advisory_waiters(pool, 1).await?;

        let hydrate_pg = pg.clone();
        let hydrate_owner = owner;
        let hydrate_task = tokio::spawn(async move {
            let hydrate_permit = OwnerWritePermit::new_for_tests(hydrate_owner, AccessKind::Fact);
            MemoryAuthoringPort::hydrate_memories(&hydrate_pg, &hydrate_permit, &[source.memory_id])
                .await
        });
        wait_for_advisory_waiters(pool, 2).await?;
        gate.rollback().await?;

        erase_task.await?.expect("the record restores");
        let hydrated = hydrate_task.await?.expect("the record restores");
        assert!(hydrated.committed);
        assert_eq!(hydrated.outcomes[0].status, MemoryHydrationStatus::Hydrated);
        assert_eq!(
            hydrated.outcomes[0].preserved_witnesses, 1,
            "the committed result counts the witness created before it acquired the lock"
        );
        assert_eq!(
            erased_pin_target_kind(pool, target_t).await?,
            Some("fact".to_owned())
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("authorized hydration witness race test failed");
}

/// Ingest one Fact under `schema_id`, give it the vector a write that ignored
/// the recipe would have left behind, cool it, hydrate it, count the jobs.
async fn cool_then_hydrate(
    pg: &PgStorage,
    owner: &OwnerRef,
    permit: &OwnerWritePermit,
    cold: &MemoryColdStore,
    schema_id: &str,
) -> Result<i64, Box<dyn std::error::Error>> {
    let pool = pg.pool_for_tests();
    let mut sourced = draft(None);
    sourced.schema_id = SchemaId::new(schema_id.to_owned());
    sourced.rendered_text = Some("a line".into());
    let written = pg.ingest_fact_atomic(permit, &sourced, None).await?;
    let t = written.memory_id.into_inner();
    let zeroes = format!("[{}]", vec!["0"; 1024].join(","));
    sqlx::query(
        "INSERT INTO proxima_core.embeddings
            (entity_id, model_id, embedding_version, vec, owner_id)
         VALUES ($1, 'test-model', 1, $2::vector, $3)",
    )
    .bind(t)
    .bind(&zeroes)
    .bind(owner.stored_owner_id())
    .execute(pool)
    .await?;

    let key = cold_object_key(t);
    let mut tx = pool.begin().await?;
    forget_memory(
        &mut tx,
        &core_pg_sidecars(),
        &surfaces(),
        cold,
        &key,
        t,
        owner.stored_owner_id(),
    )
    .await?;
    tx.commit().await?;

    let mut tx = pool.begin().await?;
    hydrate_one_in_tx(
        &mut tx,
        &core_pg_sidecars(),
        &surfaces(),
        cold,
        t,
        owner.stored_owner_id(),
        &non_embeddable_schemas(),
    )
    .await?
    .expect("the record restores");
    tx.commit().await?;

    Ok(sqlx::query_scalar(
        "SELECT count(*)::bigint FROM proxima_core.embedding_jobs WHERE entity_id = $1",
    )
    .bind(t)
    .fetch_one(pool)
    .await?)
}
