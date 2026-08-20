//! P2: an owner-to-owner transfer is an in-place series transfer.
#![allow(clippy::doc_markdown, clippy::too_many_lines)]

use std::sync::Arc;

use proxima_core::compliance::{ComplianceEraseOutcome, ComplianceEraseTarget, EraseAuthorization};
use proxima_core::storage_ports::{
    ChangeEventPort, ComplianceErasePort, MemoryReadPort, OwnerTransferPort, OwnerWritePermit,
};
use proxima_core::verbs::fact_ingest::{CitationSpec, FactWriteCommand};
use proxima_core::verbs::goal_write::GoalState;
use proxima_core::{
    AccessKind, ChangeEventKind, EntityId, EntityRef, GoalId, GroupId, MemoryId, OwnerRef,
    SchemaId, SchemaVersion, StorageError, UserId,
};
use proxima_pg_testkit::{create_db, db_url, drop_db};
use proxima_storage_pg::verbs::fact_ingest::ingest_fact_atomic;
use proxima_storage_pg::verbs::forget::{MemoryColdStore, hydrate_memory};
use proxima_storage_pg::verbs::goal_timeseries::{GoalWriteCommand, write_goal};
use proxima_storage_pg::verbs::wake_timeseries::{
    WakeConfigDraft, WakeTriggerKind, insert_wake_config, write_armed_goal,
};
use proxima_storage_pg::{PgStorage, core_pg_sidecars};
use uuid::Uuid;

fn draft() -> FactWriteCommand {
    FactWriteCommand {
        schema_id: SchemaId::new("core/test-fact-v1".to_string()),
        schema_version: SchemaVersion::new(1),
        handle: None,
        source_id: Some("src".into()),
        ingest_key: Some("k1".into()),
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

/// Transfers go owner -> owner. The destination is a group: group-manage is
/// the receiving side's consent, so a group is the only destination the
/// engine gate admits. Its `owners` row is deliberately NOT pre-created —
/// the transfer transaction must mint it (`ensure_owner_row`) or every
/// `owner_id` FK below fails.
fn destination() -> OwnerRef {
    OwnerRef::Group(GroupId::new(Uuid::now_v7()))
}

async fn fresh_pg() -> (String, PgStorage) {
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

async fn fresh_pg_with_cold() -> (String, PgStorage, Arc<MemoryColdStore>) {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let cold = Arc::new(MemoryColdStore::default());
    let pg = PgStorage::connect(&db_url(&db_name))
        .await
        .expect("connect")
        .with_cold(cold.clone());
    pg.run_migrations().await.expect("migrate");
    (db_name, pg, cold)
}

#[tokio::test]
async fn transfer_moves_same_memory_t_and_sidecar() {
    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let dest = destination();
        let pool = pg.pool_for_tests();
        let written = ingest_fact_atomic(pool, &permit, &draft(), None).await?;
        let t = written.memory_id.into_inner();
        sqlx::query(
            "INSERT INTO proxima_core.agent_note_v1 (t, note_id, title, body)
             VALUES ($1, $1, 'pub', 'body')",
        )
        .bind(t)
        .execute(pool)
        .await?;
        let content_id: Uuid = sqlx::query_scalar(
            "INSERT INTO proxima_core.content (owner_id, schema_id, content_hash)
             VALUES ($1, 'core/test-fact-v1', $2)
             RETURNING content_id",
        )
        .bind(owner.stored_owner_id())
        .bind(vec![7_u8; 32])
        .fetch_one(pool)
        .await?;
        sqlx::query("UPDATE proxima_core.memory SET content_id = $2 WHERE t = $1")
            .bind(t)
            .bind(content_id)
            .execute(pool)
            .await?;

        let first = pg
            .transfer_to_owner(&permit, EntityId::Memory(written.memory_id), dest)
            .await?;
        assert!(first);
        let content_owner: Uuid = sqlx::query_scalar(
            "SELECT c.owner_id
               FROM proxima_core.content c
               JOIN proxima_core.memory m ON m.content_id = c.content_id
              WHERE m.t = $1",
        )
        .bind(t)
        .fetch_one(pool)
        .await?;
        assert_eq!(
            content_owner,
            dest.stored_owner_id(),
            "transfer must re-home Content with the Memory"
        );
        let old_left: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.content WHERE content_id = $1",
        )
        .bind(content_id)
        .fetch_one(pool)
        .await?;
        assert_eq!(
            old_left, 0,
            "origin Content is GC'd after an exclusive transfer"
        );
        let owner_id: Uuid =
            sqlx::query_scalar("SELECT owner_id FROM proxima_core.memory WHERE t = $1")
                .bind(t)
                .fetch_one(pool)
                .await?;
        assert_eq!(owner_id, dest.stored_owner_id());
        let sketch_owner: Uuid =
            sqlx::query_scalar("SELECT owner_id FROM proxima_core.sketch WHERE t = $1")
                .bind(t)
                .fetch_one(pool)
                .await?;
        assert_eq!(
            sketch_owner,
            dest.stored_owner_id(),
            "the hot sketch must follow its transferred Memory"
        );
        let dest_sketches =
            MemoryReadPort::load_sketches(&pg, &[dest], &[written.memory_id]).await?;
        assert_eq!(
            dest_sketches.len(),
            1,
            "the destination must read the moved sketch"
        );
        let notes: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.agent_note_v1 WHERE t = $1",
        )
        .bind(t)
        .fetch_one(pool)
        .await?;
        assert_eq!(notes, 1);
        let keys: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.ingest_keys WHERE ingest_key = 'k1'",
        )
        .fetch_one(pool)
        .await?;
        assert_eq!(keys, 0);
        let replay = pg
            .transfer_to_owner(&permit, EntityId::Memory(written.memory_id), dest)
            .await?;
        assert!(
            !replay,
            "a series that already left this owner is a clean false"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("memory transfer failed");
}

#[tokio::test]
async fn transfer_rehomes_cooled_versions_and_remints_object_key() {
    let (db_name, pg, cold) = fresh_pg_with_cold().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        use proxima_core::storage_ports::MemoryAuthoringPort;
        use proxima_storage_pg::verbs::forget::cold_object_key;

        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let dest = destination();
        let pool = pg.pool_for_tests();
        let mut first_draft = draft();
        first_draft.citation = Some(
            CitationSpec::v1(
                "core/test-cited-object-v1",
                [9_u8; 32],
                "core/test-citation-mapping-v1",
            )
            .into(),
        );
        let first = ingest_fact_atomic(pool, &permit, &first_draft, None).await?;
        let cited_object_id = first
            .cited_object_id
            .expect("citation-bearing write returns its object");
        let personal_content_id: Uuid = sqlx::query_scalar(
            "INSERT INTO proxima_core.content (owner_id, schema_id, content_hash)
             VALUES ($1, 'core/test-fact-v1', $2)
             RETURNING content_id",
        )
        .bind(owner.stored_owner_id())
        .bind(vec![11_u8; 32])
        .fetch_one(pool)
        .await?;
        sqlx::query("UPDATE proxima_core.memory SET content_id = $2 WHERE t = $1")
            .bind(first.memory_id.into_inner())
            .bind(personal_content_id)
            .execute(pool)
            .await?;
        MemoryAuthoringPort::forget_memory(&pg, &permit, first.memory_id).await?;
        let mut later = draft();
        later.handle = Some(first.handle);
        later.ingest_key = Some("k2".into());
        let second = ingest_fact_atomic(pool, &permit, &later, None).await?;
        assert_eq!(second.handle, first.handle);

        let transferred = pg
            .transfer_to_owner(&permit, EntityId::Memory(second.memory_id), dest)
            .await?;
        assert!(transferred);
        let cooled_owner: uuid::Uuid =
            sqlx::query_scalar("SELECT owner_id FROM proxima_core.cooled WHERE t = $1")
                .bind(first.memory_id.into_inner())
                .fetch_one(pool)
                .await?;
        assert_eq!(
            cooled_owner,
            dest.stored_owner_id(),
            "transfer must re-home cooled versions with the series"
        );
        let object_key: String =
            sqlx::query_scalar("SELECT object_key FROM proxima_core.cooled WHERE t = $1")
                .bind(first.memory_id.into_inner())
                .fetch_one(pool)
                .await?;
        let expected = cold_object_key(first.memory_id.into_inner());
        assert_eq!(object_key, expected);
        assert!(
            !object_key.contains(&owner.stored_owner_id().to_string())
                && !object_key.contains(&dest.stored_owner_id().to_string()),
            "the cold key is owner-free, so a transfer never has to re-mint it"
        );
        let blob_owner: Uuid =
            sqlx::query_scalar("SELECT owner_id FROM proxima_core.blob WHERE blob_id = $1")
                .bind(cited_object_id)
                .fetch_one(pool)
                .await?;
        assert_eq!(
            blob_owner,
            dest.stored_owner_id(),
            "a cooled citation follows its transferred series"
        );
        let stale_ingest_keys: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.ingest_keys WHERE t = $1",
        )
        .bind(first.memory_id.into_inner())
        .fetch_one(pool)
        .await?;
        assert_eq!(
            stale_ingest_keys, 0,
            "transfer removes ingest keys for cooled versions too"
        );
        let cooled_content_owner: Uuid = sqlx::query_scalar(
            "SELECT c.owner_id
               FROM proxima_core.cooled cooled
               JOIN proxima_core.content c ON c.content_id = cooled.content_id
              WHERE cooled.t = $1",
        )
        .bind(first.memory_id.into_inner())
        .fetch_one(pool)
        .await?;
        assert_eq!(
            cooled_content_owner,
            dest.stored_owner_id(),
            "Content referenced only by a cooled version must follow it"
        );
        let personal_content_left: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.content WHERE content_id = $1",
        )
        .bind(personal_content_id)
        .fetch_one(pool)
        .await?;
        assert_eq!(
            personal_content_left, 0,
            "exclusive cooled Content is GC'd after its destination remap"
        );

        let mut tx = pool.begin().await?;
        hydrate_memory(
            &mut tx,
            &core_pg_sidecars(),
            cold.as_ref(),
            first.memory_id.into_inner(),
        )
        .await?;
        tx.commit().await?;
        let hydrated_owner: Uuid =
            sqlx::query_scalar("SELECT owner_id FROM proxima_core.memory WHERE t = $1")
                .bind(first.memory_id.into_inner())
                .fetch_one(pool)
                .await?;
        assert_eq!(
            hydrated_owner,
            dest.stored_owner_id(),
            "the reminted cold record must hydrate under the destination"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("transfer cooled remint failed");
}

/// The transfer captures the old head, then blocks on the cooled-row UPDATE.
/// A same-handle ingest advances the head in that window. The transfer's CAS
/// must roll back that partial attempt, retry with the grown series, and move
/// every owner-scoped row together without adding a lock to normal ingest.
#[tokio::test]
async fn transfer_retries_when_ingest_advances_the_captured_head() {
    const PROBE_LOCK: i64 = 0x5052_4f58_5055_4238;

    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        use proxima_core::storage_ports::MemoryAuthoringPort;

        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let dest = destination();
        let pool = pg.pool_for_tests();
        let first = ingest_fact_atomic(pool, &permit, &draft(), None).await?;
        MemoryAuthoringPort::forget_memory(&pg, &permit, first.memory_id).await?;
        let mut second_draft = draft();
        second_draft.handle = Some(first.handle);
        second_draft.ingest_key = Some("k2".into());
        let second = ingest_fact_atomic(pool, &permit, &second_draft, None).await?;

        sqlx::raw_sql(
            "CREATE SEQUENCE public.transfer_race_probe;
             CREATE FUNCTION public.block_owner_transfer() RETURNS trigger
             LANGUAGE plpgsql AS $$
             BEGIN
                 PERFORM nextval('public.transfer_race_probe');
                 PERFORM pg_advisory_xact_lock(5787775711847989816);
                 RETURN NEW;
             END
             $$;
             CREATE TRIGGER block_owner_transfer
             BEFORE UPDATE OF owner_id ON proxima_core.cooled
             FOR EACH ROW
             WHEN (OLD.owner_id IS DISTINCT FROM NEW.owner_id)
             EXECUTE FUNCTION public.block_owner_transfer();",
        )
        .execute(pool)
        .await?;

        let mut blocker = pool.acquire().await?;
        sqlx::query("SELECT pg_advisory_lock($1)")
            .bind(PROBE_LOCK)
            .execute(&mut *blocker)
            .await?;

        let transfer_pg = pg.clone();
        let transferred_id = second.memory_id;
        let transfer = tokio::spawn(async move {
            let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
            transfer_pg
                .transfer_to_owner(&permit, EntityId::Memory(transferred_id), dest)
                .await
        });

        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let reached: bool =
                    sqlx::query_scalar("SELECT is_called FROM public.transfer_race_probe")
                        .fetch_one(pool)
                        .await?;
                if reached {
                    return Ok::<(), sqlx::Error>(());
                }
                tokio::task::yield_now().await;
            }
        })
        .await??;

        let mut third_draft = draft();
        third_draft.handle = Some(first.handle);
        third_draft.ingest_key = Some("k3".into());
        let third = ingest_fact_atomic(pool, &permit, &third_draft, None).await?;

        let unlocked: bool = sqlx::query_scalar("SELECT pg_advisory_unlock($1)")
            .bind(PROBE_LOCK)
            .fetch_one(&mut *blocker)
            .await?;
        assert!(unlocked, "test must release the transfer probe lock");
        drop(blocker);

        let transferred = tokio::time::timeout(std::time::Duration::from_secs(5), transfer)
            .await??
            .map_err(|err| format!("transfer failed after retry: {err}"))?;
        assert!(transferred);

        let head: (Uuid, Uuid) =
            sqlx::query_as("SELECT t, owner_id FROM proxima_core.memory_head WHERE handle = $1")
                .bind(first.handle)
                .fetch_one(pool)
                .await?;
        assert_eq!(head.0, third.memory_id.into_inner());
        assert_eq!(head.1, dest.stored_owner_id());
        let unmoved_versions: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint
               FROM (
                     SELECT owner_id FROM proxima_core.memory WHERE handle = $1
                     UNION ALL
                     SELECT owner_id FROM proxima_core.cooled WHERE handle = $1
                    ) versions
              WHERE owner_id <> $2",
        )
        .bind(first.handle)
        .bind(dest.stored_owner_id())
        .fetch_one(pool)
        .await?;
        assert_eq!(unmoved_versions, 0, "the retry must move the grown series");
        let stale_third_key: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.ingest_keys WHERE t = $1",
        )
        .bind(third.memory_id.into_inner())
        .fetch_one(pool)
        .await?;
        assert_eq!(
            stale_third_key, 0,
            "the late version's ingest key follows retry"
        );
        let third_sketch_owner: Uuid =
            sqlx::query_scalar("SELECT owner_id FROM proxima_core.sketch WHERE t = $1")
                .bind(third.memory_id.into_inner())
                .fetch_one(pool)
                .await?;
        assert_eq!(third_sketch_owner, dest.stored_owner_id());
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("transfer/ingest CAS retry failed");
}

/// A caller can resolve the Memory under its prior owner after the series head
/// has already moved elsewhere. Transfer must return clean `false` and leave
/// every hot/cold row and announce lane untouched. The head DELETE variant is
/// not statically constructible (`memory.handle` FK-references `memory_head`),
/// so the probe diverges the head owner, which `memory_head_t_only` admits.
#[tokio::test]
async fn transfer_is_unchanged_when_the_head_owner_is_already_lost() {
    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        use proxima_core::storage_ports::MemoryAuthoringPort;
        use proxima_storage_pg::verbs::forget::cold_object_key;

        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let dest = destination();
        let pool = pg.pool_for_tests();
        let first = ingest_fact_atomic(pool, &permit, &draft(), None).await?;
        MemoryAuthoringPort::forget_memory(&pg, &permit, first.memory_id).await?;
        let mut later = draft();
        later.handle = Some(first.handle);
        later.ingest_key = Some("k2".into());
        let second = ingest_fact_atomic(pool, &permit, &later, None).await?;
        assert_eq!(second.handle, first.handle);
        let personal_key = cold_object_key(first.memory_id.into_inner());

        let usurper = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        sqlx::query(
            "INSERT INTO proxima_core.owners (owner_id, kind)
             VALUES ($1, 'personal') ON CONFLICT DO NOTHING",
        )
        .bind(usurper.stored_owner_id())
        .execute(pool)
        .await?;
        sqlx::query("UPDATE proxima_core.memory_head SET owner_id = $2 WHERE handle = $1")
            .bind(first.handle)
            .bind(usurper.stored_owner_id())
            .execute(pool)
            .await?;
        let announces_before: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.announce")
                .fetch_one(pool)
                .await?;

        let transferred = pg
            .transfer_to_owner(&permit, EntityId::Memory(second.memory_id), dest)
            .await?;
        assert!(
            !transferred,
            "a lost head is the clean not-transferred false, not an error"
        );

        let (cooled_owner, cooled_key): (Uuid, String) =
            sqlx::query_as("SELECT owner_id, object_key FROM proxima_core.cooled WHERE t = $1")
                .bind(first.memory_id.into_inner())
                .fetch_one(pool)
                .await?;
        assert_eq!(
            cooled_owner,
            owner.stored_owner_id(),
            "the rolled-back transfer must not re-home the cooled row"
        );
        assert_eq!(
            cooled_key, personal_key,
            "the rolled-back transfer must keep the personal object key"
        );
        let hot_owner: Uuid =
            sqlx::query_scalar("SELECT owner_id FROM proxima_core.memory WHERE t = $1")
                .bind(second.memory_id.into_inner())
                .fetch_one(pool)
                .await?;
        assert_eq!(hot_owner, owner.stored_owner_id());
        let transfer_rows: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.announce WHERE op = 'transfer'",
        )
        .fetch_one(pool)
        .await?;
        assert_eq!(transfer_rows, 0, "no lane may announce a failed transfer");
        let announces_after: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.announce")
                .fetch_one(pool)
                .await?;
        assert_eq!(
            announces_after, announces_before,
            "a rolled-back transfer leaves the log exactly as it was"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("mid-transfer head loss rollback failed");
}

#[tokio::test]
async fn transfer_refuses_goal_entities() {
    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Goal);
        let dest = destination();
        let pool = pg.pool_for_tests();
        let mut tx = pool.begin().await?;
        let out = write_goal(
            &mut tx,
            &owner,
            &GoalWriteCommand {
                handle: None,
                schema_id: "core/task-goal-v1".into(),
                title: "transfer me".into(),
                state: GoalState::Active,
                request_id: "pub-g".into(),
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

        let err = pg
            .transfer_to_owner(&permit, EntityId::Goal(GoalId::new(out.t)), dest)
            .await
            .expect_err("goals do not transfer");
        assert!(
            matches!(&err, StorageError::ConstraintViolation(msg)
                if msg.contains("goals do not transfer")),
            "refusal must be a ConstraintViolation naming the ruling: {err}"
        );
        let head_owner: Uuid =
            sqlx::query_scalar("SELECT owner_id FROM proxima_core.goal_head WHERE handle = $1")
                .bind(out.handle)
                .fetch_one(pool)
                .await?;
        assert_eq!(head_owner, owner.stored_owner_id());
        let row_owner: Uuid =
            sqlx::query_scalar("SELECT owner_id FROM proxima_core.goal WHERE t = $1")
                .bind(out.t)
                .fetch_one(pool)
                .await?;
        assert_eq!(row_owner, owner.stored_owner_id());
        let transfer_rows: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.announce WHERE op = 'transfer'",
        )
        .fetch_one(pool)
        .await?;
        assert_eq!(transfer_rows, 0, "a refused transfer announces nothing");

        // DDL backstops behind the storage guard: goal rows refuse UPDATE
        // entirely, and `goal_head_t_only` freezes the head's owner_id.
        let frozen = sqlx::query("UPDATE proxima_core.goal SET owner_id = $2 WHERE t = $1")
            .bind(out.t)
            .bind(dest.stored_owner_id())
            .execute(pool)
            .await;
        assert!(frozen.is_err(), "goal must refuse UPDATE entirely");
        sqlx::query(
            "INSERT INTO proxima_core.owners (owner_id, kind)
             VALUES ($1, 'group') ON CONFLICT DO NOTHING",
        )
        .bind(dest.stored_owner_id())
        .execute(pool)
        .await?;
        let head_frozen =
            sqlx::query("UPDATE proxima_core.goal_head SET owner_id = $2 WHERE handle = $1")
                .bind(out.handle)
                .bind(dest.stored_owner_id())
                .execute(pool)
                .await;
        assert!(
            head_frozen.is_err(),
            "goal_head_t_only must freeze the goal head's owner at the DDL layer"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("goal transfer refusal failed");
}

/// The erase-poison scenario: transferring an armed goal would strand its
/// `wake_config` row under the prior owner while the moved goal keeps arming
/// it, and that owner's compliance erase would abort forever on the
/// `ON DELETE RESTRICT` FK. Refusal keeps the wake row erasable.
#[tokio::test]
async fn transfer_refuses_armed_goal_and_owner_erase_still_succeeds() {
    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let user = UserId::new(Uuid::now_v7());
        let owner = OwnerRef::Personal(user);
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Goal);
        let dest = destination();
        let pool = pg.pool_for_tests();
        let mut tx = pool.begin().await?;
        let wake = insert_wake_config(
            &mut tx,
            &owner,
            &WakeConfigDraft {
                trigger_kind: WakeTriggerKind::FactSchema,
                trigger_schema_id: Some("core/test-fact-v1".into()),
                trigger_t: None,
                tool_ids: vec!["core.remember".into()],
                prompt: "private prompt".into(),
                hard_memory_t: vec![],
            },
        )
        .await?;
        let goal_handle =
            write_armed_goal(&mut tx, &owner, "armed goal", "pub-armed", wake).await?;
        tx.commit().await?;
        let goal_t: Uuid = sqlx::query_scalar("SELECT t FROM proxima_core.goal WHERE handle = $1")
            .bind(goal_handle)
            .fetch_one(pool)
            .await?;

        let err = pg
            .transfer_to_owner(&permit, EntityId::Goal(GoalId::new(goal_t)), dest)
            .await
            .expect_err("an armed goal is refused like any goal");
        assert!(matches!(err, StorageError::ConstraintViolation(_)));
        let wake_owner: Uuid =
            sqlx::query_scalar("SELECT owner_id FROM proxima_core.wake_config WHERE wake_id = $1")
                .bind(wake)
                .fetch_one(pool)
                .await?;
        assert_eq!(
            wake_owner,
            owner.stored_owner_id(),
            "the wake row must stay with the goal's owner"
        );

        let auth = EraseAuthorization::new_for_tests(ComplianceEraseTarget::PersonalOwner {
            user_id: user,
            drop_event_id: "test-drop-armed-transfer".into(),
        });
        let outcome = pg
            .erase_personal_owner_if_drop_verified(&auth, user, false, &[], &[], &[], &[])
            .await?;
        let ComplianceEraseOutcome::Completed { counts, .. } = outcome else {
            panic!("erase must complete after the refused transfer, got {outcome:?}");
        };
        assert_eq!(counts.wake_configs, 1, "the armed wake row is erased");
        let remaining: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.wake_config WHERE owner_id = $1",
        )
        .bind(owner.stored_owner_id())
        .fetch_one(pool)
        .await?;
        assert_eq!(remaining, 0);
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("armed goal refusal / erase failed");
}

#[tokio::test]
async fn transfer_writes_announce_rows_under_both_lanes() {
    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let user = UserId::new(Uuid::now_v7());
        let owner = OwnerRef::Personal(user);
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let dest = destination();
        let pool = pg.pool_for_tests();
        let written = ingest_fact_atomic(pool, &permit, &draft(), None).await?;
        let t = written.memory_id.into_inner();

        assert!(
            pg.transfer_to_owner(&permit, EntityId::Memory(written.memory_id), dest)
                .await?
        );

        // Raw rail: exactly one 'transfer' row per lane, same (handle, t).
        let rows: Vec<(Uuid, Uuid, Uuid)> = sqlx::query_as(
            "SELECT owner_id, handle, t FROM proxima_core.announce
              WHERE op = 'transfer' ORDER BY seq",
        )
        .fetch_all(pool)
        .await?;
        let lanes: Vec<Uuid> = rows.iter().map(|(owner_id, _, _)| *owner_id).collect();
        assert_eq!(
            rows.len(),
            2,
            "one row per lane: prior owner and destination owner"
        );
        assert!(lanes.contains(&owner.stored_owner_id()));
        assert!(lanes.contains(&dest.stored_owner_id()));
        for (_, handle, event_t) in &rows {
            assert_eq!(*handle, written.handle);
            assert_eq!(*event_t, t);
        }

        // The prior owner's earlier 'append' row is untouched.
        let (append_op, append_owner): (String, Uuid) =
            sqlx::query_as("SELECT op::text, owner_id FROM proxima_core.announce WHERE seq = $1")
                .bind(written.change_event_seq)
                .fetch_one(pool)
                .await?;
        assert_eq!(append_op, "append");
        assert_eq!(append_owner, owner.stored_owner_id());

        // The log stays append-only: the transfer wrote new rows, and no row
        // accepts UPDATE.
        let frozen =
            sqlx::query("UPDATE proxima_core.announce SET op = 'append' WHERE op = 'transfer'")
                .execute(pool)
                .await;
        assert!(frozen.is_err(), "announce rows must refuse UPDATE");

        // Hydrated rail, prior owner's lane: the transfer event is visible.
        let prior_lane = pg
            .list_change_events_after(std::slice::from_ref(&owner), Uuid::nil(), 100)
            .await?;
        let departed = prior_lane
            .iter()
            .find(|row| {
                matches!(
                    &row.event.kind,
                    ChangeEventKind::EntityTransfer { entity, .. }
                        if *entity == EntityRef::Memory(MemoryId::new(t))
                )
            })
            .expect("the prior owner's poll sees the transfer");
        assert_eq!(departed.event.owner, owner);

        // Destination lane: the arrival is visible under the new owner.
        let dest_lane = pg
            .list_change_events_after(&[dest], Uuid::nil(), 100)
            .await?;
        let arrived = dest_lane
            .iter()
            .find(|row| {
                matches!(
                    &row.event.kind,
                    ChangeEventKind::EntityTransfer { entity, .. }
                        if *entity == EntityRef::Memory(MemoryId::new(t))
                )
            })
            .expect("the destination's poll sees the arrival");
        assert_eq!(arrived.event.owner, dest);
        assert!(
            !dest_lane.iter().any(|row| matches!(
                row.event.kind,
                ChangeEventKind::EntityAppend { .. } | ChangeEventKind::EntityDelete { .. }
            )),
            "the append stays on the prior owner's lane"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("transfer announce rows failed");
}

#[tokio::test]
async fn transfer_moves_exclusive_blob_with_the_fact() {
    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let dest = destination();
        let pool = pg.pool_for_tests();
        let seed = ingest_fact_atomic(pool, &permit, &draft(), None).await?;
        let _ = seed;
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

        let mut cited = draft();
        cited.ingest_key = Some("blob-k".into());
        cited.blob_id = Some(blob_id);
        let written = ingest_fact_atomic(pool, &permit, &cited, None).await?;
        assert!(
            pg.transfer_to_owner(&permit, EntityId::Memory(written.memory_id), dest)
                .await?
        );
        let blob_owner: Uuid =
            sqlx::query_scalar("SELECT owner_id FROM proxima_core.blob WHERE blob_id = $1")
                .bind(blob_id)
                .fetch_one(pool)
                .await?;
        assert_eq!(blob_owner, dest.stored_owner_id());
        let stored_blob: Option<Uuid> =
            sqlx::query_scalar("SELECT blob_id FROM proxima_core.memory WHERE t = $1")
                .bind(written.memory_id.into_inner())
                .fetch_one(pool)
                .await?;
        assert_eq!(stored_blob, Some(blob_id));
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("blob transfer failed");
}

/// Retain-at-source for the audit sidecar. `mcp_call_logged_v1` describes the
/// ACTOR of a tool call (`actor_upn`), not the memory, and it has no owner
/// column: `read_mcp_call_history` reaches it by joining `memory.owner_id`.
/// Left in place the rows would follow the memory and hand the destination the
/// prior owner's actor identities, so the transfer drops them. Every other
/// sidecar (here: `agent_note_v1`) still follows the memory.
#[tokio::test]
async fn transfer_drops_the_actor_call_log_but_keeps_other_sidecars() {
    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let dest = destination();
        let pool = pg.pool_for_tests();
        let written = ingest_fact_atomic(pool, &permit, &draft(), None).await?;
        let t = written.memory_id.into_inner();
        sqlx::query(
            "INSERT INTO proxima_core.mcp_call_logged_v1
                 (t, tool_name, actor_oid, actor_upn, ok, latency_ms,
                  io_byte_len, io_truncated, io_content_hash)
             VALUES ($1, 'core_remember', 'oid-1', 'alice@example.test', true, 5,
                     10, false, $2)",
        )
        .bind(t)
        .bind(vec![3_u8; 32])
        .execute(pool)
        .await?;
        sqlx::query(
            "INSERT INTO proxima_core.agent_note_v1 (t, note_id, title, body)
             VALUES ($1, $1, 'note', 'body')",
        )
        .bind(t)
        .execute(pool)
        .await?;

        assert!(
            pg.transfer_to_owner(&permit, EntityId::Memory(written.memory_id), dest)
                .await?
        );

        let call_rows: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.mcp_call_logged_v1 WHERE t = $1",
        )
        .bind(t)
        .fetch_one(pool)
        .await?;
        assert_eq!(
            call_rows, 0,
            "the actor call log must not travel to the destination owner"
        );
        let notes: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.agent_note_v1 WHERE t = $1",
        )
        .bind(t)
        .fetch_one(pool)
        .await?;
        assert_eq!(notes, 1, "ordinary sidecars still follow the memory");
        let memory_owner: Uuid =
            sqlx::query_scalar("SELECT owner_id FROM proxima_core.memory WHERE t = $1")
                .bind(t)
                .fetch_one(pool)
                .await?;
        assert_eq!(memory_owner, dest.stored_owner_id());
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("audit sidecar retain-at-source failed");
}

/// The FK safety that used to lean on World's migration-seeded `owners` row.
/// The destination here has never been written to, so the transfer transaction
/// must mint its `owners` row before any `owner_id` FK binds — including the
/// destination's own announce lane.
#[tokio::test]
async fn transfer_mints_the_destination_owner_row_in_the_same_transaction() {
    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let dest = destination();
        let pool = pg.pool_for_tests();
        let written = ingest_fact_atomic(pool, &permit, &draft(), None).await?;

        let before: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.owners WHERE owner_id = $1",
        )
        .bind(dest.stored_owner_id())
        .fetch_one(pool)
        .await?;
        assert_eq!(before, 0, "the destination owner row must not pre-exist");

        assert!(
            pg.transfer_to_owner(&permit, EntityId::Memory(written.memory_id), dest)
                .await?
        );

        let kind: String =
            sqlx::query_scalar("SELECT kind::text FROM proxima_core.owners WHERE owner_id = $1")
                .bind(dest.stored_owner_id())
                .fetch_one(pool)
                .await?;
        assert_eq!(
            kind, "group",
            "the transfer mints the destination owner row"
        );
        let dest_announce: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.announce
              WHERE op = 'transfer' AND owner_id = $1",
        )
        .bind(dest.stored_owner_id())
        .fetch_one(pool)
        .await?;
        assert_eq!(dest_announce, 1, "the destination lane's FK holds");
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("destination owner row minting failed");
}

/// A transfer to the current owner is a no-op the storage layer refuses
/// outright rather than announcing a self-transfer.
#[tokio::test]
async fn transfer_refuses_the_current_owner_as_destination() {
    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let pool = pg.pool_for_tests();
        let written = ingest_fact_atomic(pool, &permit, &draft(), None).await?;

        let err = pg
            .transfer_to_owner(&permit, EntityId::Memory(written.memory_id), owner)
            .await
            .expect_err("a self-transfer is refused");
        assert!(matches!(err, StorageError::ConstraintViolation(_)));
        let transfer_rows: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.announce WHERE op = 'transfer'",
        )
        .fetch_one(pool)
        .await?;
        assert_eq!(transfer_rows, 0, "a refused transfer announces nothing");
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("self-transfer refusal failed");
}
