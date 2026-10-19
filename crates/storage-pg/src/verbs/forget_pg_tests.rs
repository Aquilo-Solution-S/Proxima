//! Forget / erase.
#![allow(clippy::doc_markdown, clippy::too_many_lines)]

use std::sync::Arc;

use crate::PgStorage;
use crate::core_pg_sidecars;
use crate::test_fixtures::create_core_db;
use crate::verbs::forget::{
    ColdPurgeEntry, ColdPurgePlan, MemoryColdStore, erase_memory, erase_memory_series,
    erase_memory_series_after_snapshot, forget_memory, lock_admissions_for_erase,
    lock_lifecycle_targets_tx, lock_memory_handles_tx, purge_cold_objects_after_commit,
    snapshot_series_for_erase_tx,
};
use crate::verbs::goal_timeseries::{GoalWriteCommand, write_goal};
use crate::verbs::memory_timeseries::ingest_fact_timeseries;
use proxima_core::storage_ports::FactIngestPort;
use proxima_core::storage_ports::{MemoryAuthoringPort, OwnerTransferPort, OwnerWritePermit};
use proxima_core::verbs::fact_ingest::{CitationSpec, FactWriteCommand};
use proxima_core::verbs::goal_write::GoalState;
use proxima_core::{
    AccessKind, ColdObjectStore, EdgeEndpoint, EntityId, EntityKind, GroupId, OwnerId, SchemaId,
    SchemaVersion, StorageError, UserId,
};
use proxima_pg_testkit::{db_url, drop_db};
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

fn erase_context(
    surfaces: proxima_core::owner_inverse::OwnerSurfaces,
) -> crate::PgHostStateEraseContext {
    crate::PgHostStateEraseContext::for_surfaces_for_tests(surfaces)
        .expect("these fixtures declare no host lifecycle tables")
}

/// The 1024-wide fixture vector `[1, 0, …]`.
fn unit_vector() -> pgvector::Vector {
    let mut values = vec![0.0; 1024];
    values[0] = 1.0;
    pgvector::Vector::from(values)
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
/// Ingest a Fact under an explicit `sidecar_tables` stamp, and write the rows
/// that stamp promises, in the one transaction.
///
/// `sidecar_rows` is not garnish. The current definitions trigger bundle makes
/// a
/// memory row that stamps a registered table and has no row in it is refused
/// at `COMMIT`, so a fixture that stamps `AGENT_NOTE` inserts the note here
/// rather than after this returns. Each statement binds `$1` to the new `t`.
pub(crate) async fn ingest_stamped(
    pool: &sqlx::PgPool,
    permit: &OwnerWritePermit,
    draft: &FactWriteCommand,
    tables: &[String],
    sidecar_rows: &[&str],
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
        crate::verbs::memory_timeseries::MemoryAdmissionDraft {
            store_number: 1,
            natural_key: None,
            owner: permit.owner(),
            writer: crate::test_fixtures::writer(),
            draft,
            origins: &draft.additional_references,
            references: &references,
            sidecar_tables: tables,
            scopes: &[],
        },
        None,
    )
    .await?;
    for sql in sidecar_rows {
        // SQL-POLICY: fixed-fragment — a literal from the test that asked
        // for the stamp, with the new `t` bound.
        sqlx::query(sqlx::AssertSqlSafe(*sql))
            .bind(outcome.memory_id.into_inner())
            .execute(&mut *tx)
            .await
            .map_err(|err| StorageError::Internal(err.to_string()))?;
    }
    tx.commit()
        .await
        .map_err(|err| StorageError::Internal(err.to_string()))?;
    Ok(outcome)
}

async fn append_in_own_transaction(
    pool: &sqlx::PgPool,
    owner: OwnerId,
    draft: FactWriteCommand,
) -> Result<proxima_core::verbs::fact_ingest::FactIngestOutcome, StorageError> {
    let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
    let mut tx = pool
        .begin()
        .await
        .map_err(|err| StorageError::Internal(err.to_string()))?;
    let outcome = ingest_fact_timeseries(
        &mut tx,
        crate::verbs::memory_timeseries::MemoryAdmissionDraft {
            store_number: 1,
            natural_key: None,
            owner: permit.owner(),
            writer: crate::test_fixtures::writer(),
            draft: &draft,
            origins: &[],
            references: &[],
            sidecar_tables: &[],
            scopes: &[],
        },
        None,
    )
    .await;
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

async fn cool_one(
    pool: &sqlx::PgPool,
    owner: &OwnerId,
    t: Uuid,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut tx = pool.begin().await?;
    forget_memory(
        &mut tx,
        &core_pg_sidecars(),
        &surfaces(),
        t,
        owner.into_inner(),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

pub(crate) fn draft(source: Option<(&str, &str)>) -> FactWriteCommand {
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
        kind: "fact".into(),
    }
}

#[tokio::test]
async fn forget_non_last_t_rewinds_memory_head() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let pool = pg.pool_for_tests();
        let first = pg.ingest_fact_atomic(&permit, &draft(None), None).await?;
        let mut later = draft(None);
        later.handle = Some(first.handle);
        let second = pg.ingest_fact_atomic(&permit, &later, None).await?;
        assert_eq!(second.handle, first.handle);
        assert_ne!(second.memory_id, first.memory_id);

        let mut tx = pool.begin().await?;
        forget_memory(
            &mut tx,
            &core_pg_sidecars(),
            &surfaces(),
            second.memory_id.into_inner(),
            owner.into_inner(),
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

/// #488 S14: forgetting the head moves the head to the previous version;
/// restoring it moves the head back. Restoring a live memory is a no-op
/// success, and an id the owner does not have is `NotFound`.
#[tokio::test]
async fn restore_moves_the_head_back_and_a_live_restore_changes_nothing() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let pool = pg.pool_for_tests();
        let first = pg.ingest_fact_atomic(&permit, &draft(None), None).await?;
        let mut later = draft(None);
        later.handle = Some(first.handle);
        let second = pg.ingest_fact_atomic(&permit, &later, None).await?;
        let head = || async {
            sqlx::query_scalar::<_, Uuid>(
                "SELECT t FROM proxima_core.memory_head WHERE handle = $1",
            )
            .bind(first.handle)
            .fetch_one(pool)
            .await
        };
        let state = |t: Uuid| async move {
            sqlx::query_as::<_, (bool, i64)>(
                "SELECT m.forgotten_at IS NOT NULL,
                        (SELECT count(*) FROM proxima_core.sketch s WHERE s.t = m.t)
                   FROM proxima_core.memory m WHERE m.t = $1",
            )
            .bind(t)
            .fetch_one(pool)
            .await
        };

        pg.forget_memory(&permit, second.memory_id).await?;
        assert_eq!(head().await?, first.memory_id.into_inner());
        assert_eq!(
            state(second.memory_id.into_inner()).await?,
            (true, 1),
            "forget keeps the sketch: it is the memory's own text"
        );

        pg.restore_memory(&permit, second.memory_id, &[]).await?;
        assert_eq!(head().await?, second.memory_id.into_inner(), "head again");
        assert_eq!(state(second.memory_id.into_inner()).await?, (false, 1));

        pg.restore_memory(&permit, second.memory_id, &[]).await?;
        pg.restore_memory(&permit, first.memory_id, &[]).await?;
        assert_eq!(head().await?, second.memory_id.into_inner(), "no-op");
        assert_eq!(state(first.memory_id.into_inner()).await?, (false, 1));

        let stranger = OwnerWritePermit::new_for_tests(
            OwnerId::from(UserId::new(Uuid::now_v7())),
            AccessKind::Fact,
        );
        pg.forget_memory(&permit, second.memory_id).await?;
        assert!(matches!(
            pg.restore_memory(&stranger, second.memory_id, &[]).await,
            Err(StorageError::NotFound)
        ));
        assert!(matches!(
            pg.restore_memory(&permit, proxima_core::MemoryId::new(Uuid::now_v7()), &[])
                .await,
            Err(StorageError::NotFound)
        ));
        assert_eq!(state(second.memory_id.into_inner()).await?, (true, 1));
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("restore head");
}

#[tokio::test]
async fn series_erase_includes_hot_append_that_wins_the_handle_lock_first() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
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
                &erase_context(surfaces()),
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
            crate::verbs::memory_timeseries::MemoryAdmissionDraft {
                store_number: 1,
                natural_key: None,
                owner: permit.owner(),
                writer: crate::test_fixtures::writer(),
                draft: &append_draft,
                origins: &[],
                references: &[],
                sidecar_tables: &[],
                scopes: &[],
            },
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
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
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
            &erase_context(surfaces()),
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

/// A hard erase takes a memory's other-store origins (0029) with it: no
/// foreign key and no declared surface reaches them, so the erase names
/// them, and none outlives the memory it described.
#[tokio::test]
async fn a_hard_erase_deletes_the_memorys_other_store_origins() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let pool = pg.pool_for_tests();
        let erased_t = pg
            .ingest_fact_atomic(&permit, &draft(None), None)
            .await?
            .memory_id
            .into_inner();
        let kept_t = pg
            .ingest_fact_atomic(&permit, &draft(None), None)
            .await?
            .memory_id
            .into_inner();
        sqlx::query(
            "INSERT INTO proxima_core.memory_external_origin (t, memory, edge_kind)
             VALUES ($1, '00000000-0000-8000-8000-000500000001', 'origin'), ($1, '00000000-0000-8000-8000-000600000001', 'origin'), ($2, '00000000-0000-8000-8000-000500000002', 'origin')",
        )
        .bind(erased_t)
        .bind(kept_t)
        .execute(pool)
        .await?;

        let mut tx = pool.begin().await?;
        let (erased, _) = erase_memory_series(
            &mut tx,
            &core_pg_sidecars(),
            &erase_context(surfaces()),
            &owner,
            &[erased_t],
        )
        .await?;
        tx.commit().await?;
        assert_eq!(erased, 1);

        let remaining: Vec<(Uuid, Uuid)> = sqlx::query_as(
            "SELECT t, memory FROM proxima_core.memory_external_origin ORDER BY memory",
        )
        .fetch_all(pool)
        .await?;
        assert_eq!(remaining, vec![(kept_t, Uuid::parse_str("00000000-0000-8000-8000-000500000002")?)]);
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("hard erase of other-store origins failed");
}

#[tokio::test]
async fn series_erase_linearizes_with_fully_cooled_headless_series() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let pool = pg.pool_for_tests();
        let first = pg.ingest_fact_atomic(&permit, &draft(None), None).await?;
        let mut second_draft = draft(None);
        second_draft.handle = Some(first.handle);
        let second = pg.ingest_fact_atomic(&permit, &second_draft, None).await?;
        let first_t = first.memory_id.into_inner();
        let second_t = second.memory_id.into_inner();
        cool_one(pool, &owner, first_t).await?;
        cool_one(pool, &owner, second_t).await?;
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*)::bigint FROM proxima_core.memory_head WHERE handle = $1",
            )
            .bind(first.handle)
            .fetch_one(pool)
            .await?,
            1,
            "a fully forgotten series keeps its head on the newest forgotten version"
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
                &erase_context(surfaces()),
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
            crate::verbs::memory_timeseries::MemoryAdmissionDraft {
                store_number: 1,
                natural_key: None,
                owner: permit.owner(),
                writer: crate::test_fixtures::writer(),
                draft: &append_draft,
                origins: &[],
                references: &[],
                sidecar_tables: &[],
                scopes: &[],
            },
            None,
        )
        .await?;
        append_holder.commit().await?;
        let (erased, _) =
            tokio::time::timeout(std::time::Duration::from_secs(10), erase).await???;
        assert_eq!(erased, 3);

        // Recreate a fully forgotten series for the opposite linearization.
        // If erase wins the handle, the append survives after the erase.
        let first = pg.ingest_fact_atomic(&permit, &draft(None), None).await?;
        let mut second_draft = draft(None);
        second_draft.handle = Some(first.handle);
        let second = pg.ingest_fact_atomic(&permit, &second_draft, None).await?;
        let first_t = first.memory_id.into_inner();
        let second_t = second.memory_id.into_inner();
        cool_one(pool, &owner, first_t).await?;
        cool_one(pool, &owner, second_t).await?;
        let mut erase_holder = pool.begin().await?;
        lock_memory_handles_tx(&mut erase_holder, &[first.handle]).await?;
        let append_pool = pool.clone();
        let append_owner = owner;
        let mut append_draft = draft(None);
        append_draft.handle = Some(first.handle);
        let retry_draft = append_draft.clone();
        let append = tokio::spawn(async move {
            append_in_own_transaction(&append_pool, append_owner, append_draft).await
        });
        wait_for_advisory_waiters(pool, 1).await?;
        let (erased, _) = erase_memory_series(
            &mut erase_holder,
            &core_pg_sidecars(),
            &erase_context(surfaces()),
            &owner,
            &[first_t],
        )
        .await?;
        assert_eq!(erased, 2);
        erase_holder.commit().await?;
        // The append prepared against the head on the newest forgotten
        // version; the erase deleted that head, so the append retries, as
        // production's bounded retry does.
        let survivor =
            match tokio::time::timeout(std::time::Duration::from_secs(10), append).await?? {
                Err(StorageError::Retryable(_)) => {
                    append_in_own_transaction(pool, owner, retry_draft).await?
                }
                other => other?,
            };
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
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
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
            owner.into_inner(),
            &[reused_t, untouched_t],
        )
        .await?;

        let mut replacement_erase = pool.begin().await?;
        let (erased, plan) = erase_memory_series(
            &mut replacement_erase,
            &core_pg_sidecars(),
            &erase_context(surfaces()),
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
            &erase_context(surfaces()),
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
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
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
                &erase_context(surfaces()),
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
            crate::verbs::memory_timeseries::MemoryAdmissionDraft {
                store_number: 1,
                natural_key: None,
                owner: permit.owner(),
                writer: crate::test_fixtures::writer(),
                draft: &append_draft,
                origins: &[],
                references: &[],
                sidecar_tables: &[],
                scopes: &[],
            },
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

fn derived_abstraction(origin_kind: EntityKind, origin: Uuid) -> FactWriteCommand {
    let mut cmd = draft(None);
    cmd.kind = "abstraction".into();
    cmd.additional_references = vec![EdgeEndpoint::memory(
        origin_kind,
        proxima_core::MemoryId::new(origin),
    )];
    cmd
}

fn derived_perspective(origin: Uuid) -> FactWriteCommand {
    let mut cmd = draft(None);
    cmd.kind = "perspective".into();
    cmd.additional_references = vec![EdgeEndpoint::memory(
        EntityKind::Abstraction,
        proxima_core::MemoryId::new(origin),
    )];
    cmd
}

#[tokio::test]
async fn hard_erase_witnesses_each_hot_memory_kind() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
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
            erase_memory(
                &mut tx,
                &core_pg_sidecars(),
                &erase_context(surfaces()),
                &owner,
                t,
            )
            .await?;
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
        let mut tx = pool.begin().await?;
        forget_memory(
            &mut tx,
            &core_pg_sidecars(),
            &surfaces(),
            cooled_t,
            owner.into_inner(),
        )
        .await?;
        tx.commit().await?;
        assert_eq!(erased_pin_target_kind(pool, cooled_t).await?, None);
        let mut tx = pool.begin().await?;
        let plan = erase_memory(
            &mut tx,
            &core_pg_sidecars(),
            &erase_context(surfaces()),
            &owner,
            cooled_t,
        )
        .await?;
        tx.commit().await?;
        assert_eq!(
            erased_pin_target_kind(pool, cooled_t).await?,
            Some("fact".to_owned())
        );
        let platform = pg.platform_scope_for_host();
        purge_cold_objects_after_commit(pool, &platform, &cold, &plan).await;

        // The Goal trigger has its own vocabulary and does not depend on the
        // memory erase path. Delete a real Goal row to exercise that trigger.
        let mut tx = pool.begin().await?;
        let goal = write_goal(
            &mut tx,
            &owner,
            crate::test_fixtures::writer(),
            &GoalWriteCommand {
                handle: None,
                schema_id: "core/task-goal-v1".into(),
                title: "witness me".into(),
                text: None,
                state: GoalState::Active,
                request_id: "witness-goal".into(),
                close_fact_t: None,
                assignment_t: None,
                dependency_t: vec![],
                evidence_t: vec![],
                mint_write_act: false,
                write_act_t: None,
            },
            1,
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
async fn witnessed_targets_cannot_be_reused_or_newly_pinned() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let pool = pg.pool_for_tests();
        let target = pg.ingest_fact_atomic(&permit, &draft(None), None).await?;
        let target_t = target.memory_id.into_inner();
        let mut tx = pool.begin().await?;
        erase_memory(
            &mut tx,
            &core_pg_sidecars(),
            &erase_context(surfaces()),
            &owner,
            target_t,
        )
        .await?;
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
                 blob_id, content_id, origins, refs, sidecar_tables, occurred_at, \"by\", via)
             VALUES ($1, $2, 'fact', $3, 'core/test-fact-v1', NULL, NULL,
                     NULL, NULL, '{}', '{}', '{}', proxima_core.memory_created_at($2), '00000000-0000-0000-0000-000000000509'::uuid, NULL)",
        )
        .bind(holder.handle)
        .bind(target_t)
        .bind(owner.into_inner())
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
async fn forgetting_every_source_keeps_what_rests_on_them_live() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let pool = pg.pool_for_tests();

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

        // #488 S13: a forgotten memory still grounds what rests on it, so
        // forgetting A2's only source is allowed and A2 stays live.
        let mut tx = pool.begin().await?;
        forget_memory(
            &mut tx,
            &core_pg_sidecars(),
            &surfaces(),
            abs.memory_id.into_inner(),
            owner.into_inner(),
        )
        .await
        .expect("forget A while A2 pins only A");
        tx.commit().await?;
        let mut tx = pool.begin().await?;
        forget_memory(
            &mut tx,
            &core_pg_sidecars(),
            &surfaces(),
            fact.memory_id.into_inner(),
            owner.into_inner(),
        )
        .await
        .expect("forget the Fact under A too");
        tx.commit().await?;

        let states: Vec<(Uuid, bool)> = sqlx::query_as(
            "SELECT t, forgotten_at IS NOT NULL FROM proxima_core.memory
              WHERE t = ANY($1) ORDER BY t",
        )
        .bind(vec![
            fact.memory_id.into_inner(),
            abs.memory_id.into_inner(),
            abs2.memory_id.into_inner(),
        ])
        .fetch_all(pool)
        .await?;
        assert_eq!(
            states,
            vec![
                (fact.memory_id.into_inner(), true),
                (abs.memory_id.into_inner(), true),
                (abs2.memory_id.into_inner(), false),
            ],
            "both sources are forgotten and A2, resting only on them, stays live"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("grounding-support forget/admit test failed");
}

/// Forget keeps `ingest_keys` and moves the handle to the cooled stub, so an
/// at-least-once source re-delivering a cooled admission must still get the
/// idempotent replay carrying the original handle and citation.
#[tokio::test]
async fn redelivering_a_forgotten_ingest_key_is_an_idempotent_replay() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let pool = pg.pool_for_tests();
        let mut sourced = draft(Some(("src/webhook", "delivery-cooled")));
        sourced.citation = Some(CitationSpec::v1("core/test-cited-object-v1", [7_u8; 32]).into());
        let written = pg.ingest_fact_atomic(&permit, &sourced, None).await?;
        let t = written.memory_id.into_inner();
        let cited_object_id = written
            .cited_object_id
            .expect("citation-bearing write returns its object");

        let mut tx = pool.begin().await?;
        forget_memory(
            &mut tx,
            &core_pg_sidecars(),
            &surfaces(),
            t,
            owner.into_inner(),
        )
        .await?;
        tx.commit().await?;

        let kept_blob: Option<Uuid> = sqlx::query_scalar(
            "SELECT blob_id FROM proxima_core.memory WHERE t = $1 AND forgotten_at IS NOT NULL",
        )
        .bind(t)
        .fetch_one(pool)
        .await?;
        assert_eq!(kept_blob, Some(cited_object_id));

        // The retry need not repeat the citation input: replay reads the
        // original witness from the admission selected by its ingest key.
        let mut replay_input = sourced.clone();
        replay_input.citation = None;
        replay_input.blob_id = None;
        let replay = pg
            .ingest_fact_atomic(&permit, &replay_input, None)
            .await
            .expect("re-delivery of a forgotten admission");
        assert!(replay.idempotent_replay);
        assert!(
            replay.forgotten,
            "the replay says the admission is forgotten"
        );
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
        assert_eq!(hot, 1, "a replay mints no new row");
        let still_forgotten: bool = sqlx::query_scalar(
            "SELECT forgotten_at IS NOT NULL FROM proxima_core.memory WHERE t = $1",
        )
        .bind(t)
        .fetch_one(pool)
        .await?;
        assert!(still_forgotten, "the admission stays forgotten");
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("forgotten ingest-key replay test failed");
}

#[tokio::test]
async fn forget_pinless_abstraction_is_refused() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let pool = pg.pool_for_tests();
        pg.ingest_fact_atomic(&permit, &draft(None), None).await?;
        let owner_id = owner.into_inner();
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
                (handle, t, kind, owner_id, schema_id, content_id, origins, refs, \"by\", via)
             VALUES ($1, $2, 'abstraction', $3, 'core/test-abs-v1', $4, '{}', '{}', '00000000-0000-0000-0000-000000000509'::uuid, NULL)",
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
async fn concurrent_forgets_of_both_sources_both_commit() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        let p1 = OwnerWritePermit::new_for_tests(
            OwnerId::from(UserId::new(Uuid::now_v7())),
            AccessKind::Fact,
        );
        let p2 = OwnerWritePermit::new_for_tests(
            OwnerId::from(UserId::new(Uuid::now_v7())),
            AccessKind::Fact,
        );
        let p3 = OwnerWritePermit::new_for_tests(
            OwnerId::from(UserId::new(Uuid::now_v7())),
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
        both.additional_references = vec![
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
        let a1_owner = p1.owner().into_inner();
        let a2_owner = p2.owner().into_inner();
        let f1 = tokio::spawn(async move {
            let mut tx = pool_a.begin().await.map_err(|err| err.to_string())?;
            let r = forget_memory(&mut tx, &core_pg_sidecars(), &surfaces(), a1_t, a1_owner).await;
            match &r {
                Ok(()) => tx.commit().await.map_err(|err| err.to_string())?,
                Err(_) => tx.rollback().await.map_err(|err| err.to_string())?,
            }
            Ok::<_, String>(r)
        });
        let f2 = tokio::spawn(async move {
            let mut tx = pool_b.begin().await.map_err(|err| err.to_string())?;
            let r = forget_memory(&mut tx, &core_pg_sidecars(), &surfaces(), a2_t, a2_owner).await;
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
            ok_count, 2,
            "a forgotten source still grounds, so both forgets commit; {r1:?} {r2:?}"
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
        assert_eq!(
            remaining, 2,
            "the dependent keeps both pins, each to a forgotten memory row"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("concurrent forget grounding test failed");
}

#[tokio::test]
async fn forget_blocks_admit_until_its_lifecycle_lock_is_released() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
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

        let owner_id = owner.into_inner();
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
                    (handle, t, kind, owner_id, schema_id, content_id, origins, refs, \"by\", via)
                 VALUES ($1, $2, 'abstraction', $3, 'core/test-abs-v1', $4, ARRAY[$5]::uuid[], '{}', '00000000-0000-0000-0000-000000000509'::uuid, NULL)",
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

        forget_memory(&mut tx_f, &core_pg_sidecars(), &surfaces(), a_t, owner_id)
        .await?;
        tx_f.commit().await?;

        // Since #488 S13 pin integrity accepts any memory row, so a raw
        // admission (table owner, no owner scope) pinning the now-forgotten A
        // passes once it gets the lock. A real write cannot cite A: owner
        // scope does not resolve a forgotten memory.
        admit
            .await?
            .expect("raw admission over the forgotten A, after the lifecycle lock");
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
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
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
        append.additional_references = vec![EdgeEndpoint::memory(
            EntityKind::Fact,
            proxima_core::MemoryId::new(origin.memory_id.into_inner()),
        )];
        append.refs = vec![target_t];

        // The forget holds its whole lock footprint and pauses before it
        // marks the row: the point the writer must queue behind.
        let mut forget = pool.begin().await?;
        super::lock_forget_footprint_tx(&mut forget, target_t, target.handle).await?;

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

        forget_memory(
            &mut forget,
            &core_pg_sidecars(),
            &surfaces(),
            target_t,
            owner.into_inner(),
        )
        .await?;
        forget.commit().await?;
        let writer_result =
            tokio::time::timeout(std::time::Duration::from_secs(10), writer).await?;
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
            2,
            "the forgotten T stays as a row beside the append"
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
        assert!(
            sqlx::query_scalar::<_, bool>(
                "SELECT forgotten_at IS NOT NULL FROM proxima_core.memory WHERE t = $1",
            )
            .bind(target_t)
            .fetch_one(&pool)
            .await?,
            "forget linearizes before the append and leaves T forgotten"
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
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let pool = pg.pool_for_tests().clone();
        let prior = pg.ingest_fact_atomic(&permit, &draft(None), None).await?;
        let prior_t = prior.memory_id.into_inner();
        let handle = prior.handle;
        let mut forget = pool.begin().await?;
        super::lock_forget_footprint_tx(&mut forget, prior_t, handle).await?;

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

        forget_memory(
            &mut forget,
            &core_pg_sidecars(),
            &surfaces(),
            prior_t,
            owner.into_inner(),
        )
        .await?;
        forget.commit().await?;
        let writer_result =
            tokio::time::timeout(std::time::Duration::from_secs(10), writer).await?;
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
            2,
            "the retry leaves the forgotten prior and one new live version"
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
        assert!(
            sqlx::query_scalar::<_, bool>(
                "SELECT forgotten_at IS NOT NULL FROM proxima_core.memory WHERE t = $1",
            )
            .bind(prior_t)
            .fetch_one(&pool)
            .await?,
            "the prior head is forgotten before the append retry"
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
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let pool = pg.pool_for_tests().clone();
        let mut cited_draft = draft(None);
        cited_draft.citation =
            Some(CitationSpec::v1("core/test-cited-object-v1", [0xA7; 32]).into());
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
                &erase_context(surfaces()),
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
async fn forget_aborts_when_owner_transferred() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let pool = pg.pool_for_tests();
        let written = pg.ingest_fact_atomic(&permit, &draft(None), None).await?;
        let t = written.memory_id.into_inner();
        let dest = OwnerId::from(GroupId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::transfer_for_tests(owner, AccessKind::Fact, dest);
        assert!(
            pg.transfer_to_owner(
                &permit,
                EntityId::Memory(written.memory_id),
                dest,
                &transfer_surfaces(),
                &[]
            )
            .await?
        );

        let mut tx = pool.begin().await?;
        let err = forget_memory(
            &mut tx,
            &core_pg_sidecars(),
            &surfaces(),
            t,
            owner.into_inner(),
        )
        .await
        .expect_err("forget after a transfer must not mark the new owner's row");
        assert!(matches!(err, StorageError::NotFound), "got {err:?}");
        tx.rollback().await?;

        let moved: Uuid =
            sqlx::query_scalar("SELECT owner_id FROM proxima_core.memory WHERE t = $1")
                .bind(t)
                .fetch_one(pool)
                .await?;
        assert_eq!(moved, dest.into_inner());
        let forgotten: bool = sqlx::query_scalar(
            "SELECT forgotten_at IS NOT NULL FROM proxima_core.memory WHERE t = $1",
        )
        .bind(t)
        .fetch_one(pool)
        .await?;
        assert!(!forgotten, "the transferred row is not marked");
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("forget-after-transfer must abort");
}

#[tokio::test]
async fn stale_source_erase_does_not_lock_transferred_series() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        let source = OwnerId::from(UserId::new(Uuid::now_v7()));
        let destination = OwnerId::from(GroupId::new(Uuid::now_v7()));
        let source_permit =
            OwnerWritePermit::transfer_for_tests(source, AccessKind::Fact, destination);
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
                &[],
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
            crate::verbs::memory_timeseries::MemoryAdmissionDraft {
                store_number: 1,
                natural_key: None,
                owner: destination_permit.owner(),
                writer: crate::test_fixtures::writer(),
                draft: &append_draft,
                origins: &[],
                references: &[],
                sidecar_tables: &[],
                scopes: &[],
            },
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

#[derive(Default)]
struct PurgeRecordingCold {
    inner: MemoryColdStore,
    deletes: std::sync::Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl ColdObjectStore for PurgeRecordingCold {
    fn backend(&self) -> &str {
        self.inner.backend()
    }

    async fn put(&self, key: &str, bytes: &[u8]) -> Result<(), StorageError> {
        self.inner.put(key, bytes).await
    }

    async fn get(&self, key: &str) -> Result<Vec<u8>, StorageError> {
        self.inner.get(key).await
    }

    async fn delete(&self, key: &str) -> Result<(), StorageError> {
        self.deletes
            .lock()
            .expect("cold delete calls")
            .push(key.to_owned());
        self.inner.delete(key).await
    }
}

/// A captured plan must recheck durable authority before touching the provider.
/// Retired and reassigned debts leave bytes alone; the unchanged debt still
/// performs its actual deletion in the same pass.
#[tokio::test]
async fn stale_purge_plan_rechecks_current_debt_before_provider_delete() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        let pool = pg.pool_for_tests();
        let cold = PurgeRecordingCold::default();
        let retired_key = "objects/retired-debt";
        let reassigned_key = "objects/reassigned-debt";
        let current_key = "objects/current-debt";
        let bytes = b"bytes require a current matching purge obligation";
        let mut entries = Vec::new();
        for key in [retired_key, reassigned_key, current_key] {
            cold.put(key, bytes).await?;
            let (object_key, backend): (String, String) = sqlx::query_as(
                "INSERT INTO proxima_core.cold_purge_pending (object_key, owner_id, backend)
                 VALUES ($1, NULL, $2)
                 RETURNING object_key, backend",
            )
            .bind(key)
            .bind(cold.backend())
            .fetch_one(pool)
            .await?;
            entries.push(ColdPurgeEntry {
                object_key,
                backend,
            });
        }
        let plan = ColdPurgePlan::from_entries(entries);

        // Model a committed retirement and reassignment after plan capture,
        // before any provider I/O. The plan itself still names this store.
        assert_eq!(
            sqlx::query("DELETE FROM proxima_core.cold_purge_pending WHERE object_key = $1")
                .bind(retired_key)
                .execute(pool)
                .await?
                .rows_affected(),
            1
        );
        assert_eq!(
            sqlx::query(
                "UPDATE proxima_core.cold_purge_pending SET backend = 'other-bucket'
                 WHERE object_key = $1",
            )
            .bind(reassigned_key)
            .execute(pool)
            .await?
            .rows_affected(),
            1
        );

        let platform = pg.platform_scope_for_host();
        let outcome = purge_cold_objects_after_commit(pool, &platform, &cold, &plan).await;
        assert_eq!(
            (
                outcome.attempted,
                outcome.purged,
                outcome.failed,
                outcome.pending
            ),
            (3, 2, 1, true)
        );
        assert_eq!(
            *cold.deletes.lock().expect("cold delete calls"),
            vec![current_key.to_owned()],
            "only the unchanged durable debt may reach the provider"
        );
        assert_eq!(cold.get(retired_key).await?, bytes);
        assert_eq!(cold.get(reassigned_key).await?, bytes);
        assert!(matches!(
            cold.get(current_key).await,
            Err(StorageError::NotFound)
        ));
        let remaining: Vec<(String, String)> = sqlx::query_as(
            "SELECT object_key, backend FROM proxima_core.cold_purge_pending ORDER BY object_key",
        )
        .fetch_all(pool)
        .await?;
        assert_eq!(
            remaining,
            vec![(reassigned_key.to_owned(), "other-bucket".to_owned())],
            "the reassigned obligation survives with its actual backend"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("stale purge plan provider boundary test failed");
}

/// The erase is already committed by the time the object store is asked, so a
/// refusing store cannot undo it. What it must not do is lose the debt: the
/// key stays in `cold_purge_pending` as the durable record a retry reads.
#[tokio::test]
async fn a_refusing_cold_store_leaves_the_purge_mark_for_retry() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let pool = pg.pool_for_tests();
        let written = pg.ingest_fact_atomic(&permit, &draft(None), None).await?;
        let t = written.memory_id.into_inner();
        MemoryAuthoringPort::forget_memory(&pg, &permit, written.memory_id).await?;

        let mut tx = pool.begin().await?;
        let memory_plan = erase_memory(
            &mut tx,
            &core_pg_sidecars(),
            &erase_context(surfaces()),
            &owner,
            t,
        )
        .await?;
        // #488: a forgotten memory keeps its row and owes the object store
        // nothing. The debt below is an owned object's, the one kind left.
        assert!(
            memory_plan.is_empty(),
            "erasing a memory owes no cold object"
        );
        let key = "objects/refused".to_owned();
        let backend: String = sqlx::query_scalar(
            "INSERT INTO proxima_core.cold_purge_pending (object_key, owner_id, backend)
             VALUES ($1, $2, $3)
             RETURNING backend",
        )
        .bind(&key)
        .bind(owner.into_inner())
        .bind(FailDeleteCold.backend())
        .fetch_one(tx.as_mut())
        .await?;
        tx.commit().await?;
        let plan = ColdPurgePlan::from_entries(vec![ColdPurgeEntry {
            object_key: key.clone(),
            backend,
        }]);

        let platform = pg.platform_scope_for_host();
        let purged = purge_cold_objects_after_commit(pool, &platform, &FailDeleteCold, &plan).await;
        assert_eq!(purged.purged, 0, "a refusing object store destroys nothing");
        assert!(purged.pending);
        let pending: Vec<String> = sqlx::query_scalar(
            "SELECT object_key FROM proxima_core.cold_purge_pending WHERE owner_id = $1",
        )
        .bind(owner.into_inner())
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
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let pool = pg.pool_for_tests().clone();

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
        pg.forget_memory(&permit, second.memory_id).await?;
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
        erase_memory(
            &mut tx,
            &core_pg_sidecars(),
            &erase_context(surfaces()),
            &owner,
            t1,
        )
        .await?;
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
            Some(t2),
            "with only a forgotten version left, the head stays on it: memory.handle \
             references memory_head, so the head goes only when the last row does"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("the forget declarations must match shipped behaviour");
}

/// A `ForgetRule::Keep` sidecar that is not owner-pinned stops the erase's
/// stamp walk instead of being deleted by it.
///
/// Since #488 a forget leaves every sidecar in place, so the stamp walk in
/// `delete_memory_dependents` serves erase only. It must still not map `Kept`
/// onto the key column `"t"` the way it maps `Marked`: that would delete a
/// surface the declaration says is kept, silently.
///
/// A legacy `Keep` memory sidecar and its rows survive only because it is
/// ALSO `TransferRule::RetainAtSource` and
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
async fn a_kept_sidecar_that_is_not_owner_pinned_stops_the_erase() {
    use proxima_core::flavor::{ForgetRule, Surface};

    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
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

        let written = ingest_stamped(
            pool,
            &permit,
            &draft(None),
            &[AGENT_NOTE.to_owned()],
            &[
                "INSERT INTO proxima_core.agent_note_v1 (t, note_id, title, body, tags)
             VALUES ($1, $1, 'n', 'kept', ARRAY['tag'])",
            ],
        )
        .await?;
        let t = written.memory_id.into_inner();

        let mut tx = pool.begin().await?;
        let outcome = erase_memory(
            &mut tx,
            &core_pg_sidecars(),
            &erase_context(kept_surfaces),
            &owner,
            t,
        )
        .await;
        tx.rollback().await?;

        let err = outcome.expect_err(
            "a Keep declaration the substrate cannot honour must stop the erase's stamp \
             walk, not be quietly overridden by it",
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
    result.expect("a_kept_sidecar_that_is_not_owner_pinned_stops_the_erase failed");
}

/// Wait until some backend of `pool`'s database is blocked on a row lock
/// while running a statement that starts with `prefix`. A pause point that
/// observes the server's own lock graph, so no timing is involved.
async fn await_backend_blocked_on(pool: &sqlx::PgPool, prefix: &str) -> Result<(), sqlx::Error> {
    loop {
        let blocked: bool = sqlx::query_scalar(
            "SELECT EXISTS (
                SELECT 1 FROM pg_stat_activity
                 WHERE datname = current_database()
                   AND wait_event_type = 'Lock'
                   AND starts_with(ltrim(query), $1))",
        )
        .bind(prefix)
        .fetch_one(pool)
        .await?;
        if blocked {
            return Ok(());
        }
        tokio::task::yield_now().await;
    }
}

/// A drainer that holds an `embedding_jobs` claim while forget runs must not
/// leave an `embedding_heads` row behind. Forget has to take the job row
/// before it deletes heads, the same order owner erase uses; otherwise the
/// drainer writes a head after forget's head delete and before its job
/// delete, and that head outlives the forgotten `t`.
#[tokio::test]
async fn forget_overlapping_drain_leaves_no_orphan_head() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone())
            .await?
            .with_cold(Arc::new(MemoryColdStore::default()));
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let pool = pg.pool_for_tests().clone();
        let written = pg.ingest_fact_atomic(&permit, &draft(None), None).await?;
        let t = written.memory_id.into_inner();
        sqlx::query("DELETE FROM proxima_core.embedding_jobs WHERE entity_id = $1")
            .bind(t)
            .execute(&pool)
            .await?;
        sqlx::query(
            "INSERT INTO proxima_core.embedding_jobs (entity_id, model_id, dim, owner_id)
             VALUES ($1, 'test-embed', 1024, $2)",
        )
        .bind(t)
        .bind(owner.into_inner())
        .execute(&pool)
        .await?;

        // The drainer claims the job and holds it.
        let mut drain = pool.begin().await?;
        sqlx::query(
            "SELECT job_id FROM proxima_core.embedding_jobs WHERE entity_id = $1 FOR UPDATE",
        )
        .bind(t)
        .fetch_one(drain.as_mut())
        .await?;
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let forget = {
            let pg = pg.clone();
            let barrier = barrier.clone();
            tokio::spawn(async move {
                barrier.wait().await;
                MemoryAuthoringPort::forget_memory(&pg, &permit, written.memory_id).await
            })
        };
        barrier.wait().await;
        // Forget is now parked behind the drainer's job lock.
        await_backend_blocked_on(&pool, "DELETE FROM proxima_core.embedding_jobs").await?;

        // The drainer finishes its write: one chunk and the head.
        sqlx::query(
            "INSERT INTO proxima_core.embeddings
                (entity_id, model_id, dim, embedding_version, chunk_ordinal, vec, owner_id)
             VALUES ($1, 'test-embed', 1024, 1, 0, $3, $2)",
        )
        .bind(t)
        .bind(owner.into_inner())
        .bind(unit_vector())
        .execute(drain.as_mut())
        .await?;
        sqlx::query(
            "INSERT INTO proxima_core.embedding_heads
                (entity_id, model_id, dim, embedding_version, owner_id)
             VALUES ($1, 'test-embed', 1024, 1, $2)",
        )
        .bind(t)
        .bind(owner.into_inner())
        .execute(drain.as_mut())
        .await?;
        drain.commit().await?;
        forget.await??;

        let heads: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM proxima_core.embedding_heads WHERE entity_id = $1",
        )
        .bind(t)
        .fetch_one(&pool)
        .await?;
        assert_eq!(heads, 0, "no embedding head outlives the forgotten t");
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("forget overlapping drain failed");
}
