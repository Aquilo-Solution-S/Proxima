//! WakeConfig share / RESTRICT / match / fire.
#![allow(clippy::doc_markdown, clippy::too_many_lines)]

use std::sync::Arc;

use proxima_core::owner_inverse::OwnerSurfaces;
use proxima_core::storage_ports::{FactIngestPort, MemoryAuthoringPort, OwnerWritePermit};
use proxima_core::verbs::fact_ingest::FactWriteCommand;
use proxima_core::verbs::goal_write::GoalState;
use proxima_core::{
    AccessKind, EdgeEndpoint, EntityKind, MemoryId, OwnerRef, SchemaId, SchemaVersion,
    StorageError, UserId,
};
use proxima_pg_testkit::{create_db, db_url, drop_db};
use proxima_storage_pg::PgStorage;
use proxima_storage_pg::verbs::forget::{MemoryColdStore, erase_memory};
use proxima_storage_pg::verbs::goal_timeseries::{GoalWriteCommand, write_goal};
use proxima_storage_pg::verbs::wake_timeseries::{
    WakeConfigDraft, WakeTriggerKind, fire_wake, insert_wake_config, matching_wake_ids,
    update_wake_prompt, write_armed_goal,
};
use uuid::Uuid;

fn fact_draft(schema: &str) -> FactWriteCommand {
    FactWriteCommand {
        schema_id: SchemaId::new(schema.to_string()),
        schema_version: SchemaVersion::new(1),
        handle: None,
        source_id: None,
        ingest_key: None,
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

fn abstraction_draft(origin: MemoryId) -> FactWriteCommand {
    let mut draft = fact_draft("core/test-fact-v1");
    draft.kind = "abstraction".into();
    draft.derived_from = vec![EdgeEndpoint::memory(EntityKind::Fact, origin)];
    draft
}

fn perspective_draft(origin: MemoryId) -> FactWriteCommand {
    let mut draft = fact_draft("core/test-fact-v1");
    draft.kind = "perspective".into();
    draft.derived_from = vec![EdgeEndpoint::memory(EntityKind::Abstraction, origin)];
    draft
}

fn erase_surfaces() -> OwnerSurfaces {
    OwnerSurfaces::for_registry(&proxima_core::FlavorRegistry::new().freeze_or_panic_for_tests())
}

#[tokio::test]
async fn goal_wake_share_restrict_match_fire() {
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

        let mut tx = pool.begin().await?;
        let wake_id = insert_wake_config(
            &mut tx,
            &owner,
            &WakeConfigDraft {
                trigger_kind: WakeTriggerKind::FactSchema,
                trigger_schema_id: Some("core/visit-v1".into()),
                trigger_t: None,
                tool_ids: vec!["core.remember".into()],
                prompt: "on visit".into(),
                hard_memory_t: vec![],
            },
        )
        .await?;
        let g1 = write_armed_goal(&mut tx, &owner, "one", "g1", wake_id).await?;
        let g2 = write_armed_goal(&mut tx, &owner, "two", "g2", wake_id).await?;
        tx.commit().await?;
        assert_ne!(g1, g2);

        let n: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.goal WHERE wake_id = $1")
                .bind(wake_id)
                .fetch_one(pool)
                .await?;
        assert_eq!(n, 2);

        let err = sqlx::query("DELETE FROM proxima_core.wake_config WHERE wake_id = $1")
            .bind(wake_id)
            .execute(pool)
            .await
            .expect_err("RESTRICT while goals name it");
        assert!(
            err.to_string().contains("restrict")
                || err.to_string().contains("violates")
                || err.to_string().contains("23503"),
            "got: {err}"
        );

        update_wake_prompt(pool, wake_id, "updated").await?;
        let prompt: String =
            sqlx::query_scalar("SELECT prompt FROM proxima_core.wake_config WHERE wake_id = $1")
                .bind(wake_id)
                .fetch_one(pool)
                .await?;
        assert_eq!(prompt, "updated");

        let visit = pg
            .ingest_fact_atomic(&permit, &fact_draft("core/visit-v1"), None)
            .await?;
        let other = pg
            .ingest_fact_atomic(&permit, &fact_draft("core/other-v1"), None)
            .await?;
        let matched = matching_wake_ids(pool, visit.memory_id.into_inner()).await?;
        assert_eq!(matched, vec![wake_id]);
        let unmatched = matching_wake_ids(pool, other.memory_id.into_inner()).await?;
        assert!(unmatched.is_empty());

        let mut tx = pool.begin().await?;
        let tr = fire_wake(&mut tx, &owner).await?;
        tx.commit().await?;
        let schema: String = sqlx::query_scalar(
            "SELECT m.schema_id FROM proxima_core.memory m
             WHERE m.t = $1",
        )
        .bind(tr)
        .fetch_one(pool)
        .await?;
        assert_eq!(schema, "core/write-act-v1");

        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("wake test failed");
}

#[tokio::test]
async fn wake_target_erase_and_admission_have_one_lifecycle_order() {
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

        // The admission wins first; the later erase is allowed to leave its
        // wake row behind because wake references are intentionally unowned.
        let first = pg
            .ingest_fact_atomic(&permit, &fact_draft("core/visit-v1"), None)
            .await?;
        let mut tx = pool.begin().await?;
        let first_wake = insert_wake_config(
            &mut tx,
            &owner,
            &WakeConfigDraft {
                trigger_kind: WakeTriggerKind::FactMemory,
                trigger_schema_id: None,
                trigger_t: Some(first.memory_id.into_inner()),
                tool_ids: vec!["core.remember".into()],
                prompt: "first".into(),
                hard_memory_t: vec![],
            },
        )
        .await?;
        tx.commit().await?;
        let mut tx = pool.begin().await?;
        erase_memory(
            &mut tx,
            &proxima_storage_pg::core_pg_sidecars(),
            &erase_surfaces(),
            &owner,
            first.memory_id.into_inner(),
        )
        .await?;
        tx.commit().await?;
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*)::bigint FROM proxima_core.wake_config WHERE wake_id = $1",
            )
            .bind(first_wake)
            .fetch_one(pool)
            .await?,
            1
        );

        // The erase wins this time. The direct wake admission must wait on
        // the target advisory and then fail before inserting a wake row.
        let second = pg
            .ingest_fact_atomic(&permit, &fact_draft("core/visit-v1"), None)
            .await?;
        let target_t = second.memory_id.into_inner();
        let mut erase_tx = pool.begin().await?;
        sqlx::query(
            "SELECT pg_advisory_xact_lock(
                 hashtextextended('proxima-forget:' || $1::text, 0)
             )",
        )
        .bind(target_t)
        .execute(&mut *erase_tx)
        .await?;
        let writer_pool = pool.clone();
        let writer_owner = owner;
        let mut writer = tokio::spawn(async move {
            let mut tx = writer_pool.begin().await.map_err(|err| err.to_string())?;
            let result = insert_wake_config(
                &mut tx,
                &writer_owner,
                &WakeConfigDraft {
                    trigger_kind: WakeTriggerKind::FactMemory,
                    trigger_schema_id: None,
                    trigger_t: Some(target_t),
                    tool_ids: vec!["core.remember".into()],
                    prompt: "loser".into(),
                    hard_memory_t: vec![],
                },
            )
            .await
            .map_err(|err| err.to_string());
            let _ = tx.rollback().await;
            result
        });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut writer)
                .await
                .is_err(),
            "wake admission must wait for the target lifecycle lock"
        );
        erase_memory(
            &mut erase_tx,
            &proxima_storage_pg::core_pg_sidecars(),
            &erase_surfaces(),
            &owner,
            target_t,
        )
        .await?;
        erase_tx.commit().await?;
        let err = writer.await?.expect_err("erased wake target must reject");
        assert!(
            err.contains("does not exist") || err.contains("23503"),
            "got {err}"
        );
        let wake_count: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.wake_config WHERE trigger_t = $1",
        )
        .bind(target_t)
        .fetch_one(pool)
        .await?;
        assert_eq!(wake_count, 0);
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("wake target lifecycle ordering failed");
}

#[tokio::test]
async fn carried_wake_with_erased_target_rejects_without_a_goal_head() {
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
        let target = pg
            .ingest_fact_atomic(&permit, &fact_draft("core/visit-v1"), None)
            .await?;
        let mut tx = pool.begin().await?;
        let wake_id = insert_wake_config(
            &mut tx,
            &owner,
            &WakeConfigDraft {
                trigger_kind: WakeTriggerKind::FactMemory,
                trigger_schema_id: None,
                trigger_t: Some(target.memory_id.into_inner()),
                tool_ids: vec!["core.remember".into()],
                prompt: "carried".into(),
                hard_memory_t: vec![],
            },
        )
        .await?;
        let source = write_goal(
            &mut tx,
            &owner,
            &GoalWriteCommand {
                handle: None,
                schema_id: "core/task-v1".into(),
                title: "source".into(),
                state: GoalState::Active,
                request_id: "carried-source".into(),
                close_fact_t: None,
                assignment_t: None,
                dependency_t: vec![],
                evidence_t: vec![],
                wake_id: Some(wake_id),
                mint_write_act: false,
                write_act_t: None,
            },
        )
        .await?;
        tx.commit().await?;

        let mut tx = pool.begin().await?;
        erase_memory(
            &mut tx,
            &proxima_storage_pg::core_pg_sidecars(),
            &erase_surfaces(),
            &owner,
            target.memory_id.into_inner(),
        )
        .await?;
        tx.commit().await?;
        let before: (i64, i64) = sqlx::query_as(
            "SELECT
                (SELECT count(*)::bigint FROM proxima_core.goal),
                (SELECT count(*)::bigint FROM proxima_core.goal_head)",
        )
        .fetch_one(pool)
        .await?;
        let mut tx = pool.begin().await?;
        let err = write_goal(
            &mut tx,
            &owner,
            &GoalWriteCommand {
                handle: None,
                schema_id: "core/task-v1".into(),
                title: "rejected successor".into(),
                state: GoalState::Active,
                request_id: "carried-successor".into(),
                close_fact_t: None,
                assignment_t: None,
                dependency_t: vec![],
                evidence_t: vec![],
                wake_id: Some(wake_id),
                mint_write_act: false,
                write_act_t: None,
            },
        )
        .await
        .expect_err("carried wake target was erased");
        tx.rollback().await?;
        assert!(err.to_string().contains("does not exist"), "got {err}");
        let after: (i64, i64) = sqlx::query_as(
            "SELECT
                (SELECT count(*)::bigint FROM proxima_core.goal),
                (SELECT count(*)::bigint FROM proxima_core.goal_head)",
        )
        .fetch_one(pool)
        .await?;
        assert_eq!(after, before, "failed carry must not mint a Goal or head");
        let source_count: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.goal WHERE t = $1")
                .bind(source.t)
                .fetch_one(pool)
                .await?;
        assert_eq!(source_count, 1, "the existing Goal remains stored");
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("carried wake lifecycle test failed");
}

#[tokio::test]
async fn cooled_assignment_rejects_a_goal_successor_without_a_new_head() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url)
            .await?
            .with_cold(Arc::new(MemoryColdStore::default()));
        pg.run_migrations().await?;
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let pool = pg.pool_for_tests();
        let fact = pg
            .ingest_fact_atomic(&permit, &fact_draft("core/goal-origin-v1"), None)
            .await?;
        let abstraction = pg
            .ingest_fact_atomic(&permit, &abstraction_draft(fact.memory_id), None)
            .await?;
        let assignment = pg
            .ingest_fact_atomic(&permit, &perspective_draft(abstraction.memory_id), None)
            .await?;
        let mut tx = pool.begin().await?;
        let source = write_goal(
            &mut tx,
            &owner,
            &GoalWriteCommand {
                handle: None,
                schema_id: "core/task-v1".into(),
                title: "assigned source".into(),
                state: GoalState::Active,
                request_id: "cooled-assignment-source".into(),
                close_fact_t: None,
                assignment_t: Some(assignment.memory_id.into_inner()),
                dependency_t: vec![],
                evidence_t: vec![],
                wake_id: None,
                mint_write_act: false,
                write_act_t: None,
            },
        )
        .await?;
        tx.commit().await?;

        MemoryAuthoringPort::forget_memory(&pg, &permit, assignment.memory_id).await?;
        let before: (i64, i64) = sqlx::query_as(
            "SELECT
                (SELECT count(*)::bigint FROM proxima_core.goal),
                (SELECT count(*)::bigint FROM proxima_core.goal_head)",
        )
        .fetch_one(pool)
        .await?;
        let mut tx = pool.begin().await?;
        let err = write_goal(
            &mut tx,
            &owner,
            &GoalWriteCommand {
                handle: Some(source.handle),
                schema_id: "core/task-v1".into(),
                title: "rejected successor".into(),
                state: GoalState::Active,
                request_id: "cooled-assignment-successor".into(),
                close_fact_t: None,
                assignment_t: Some(assignment.memory_id.into_inner()),
                dependency_t: vec![],
                evidence_t: vec![],
                wake_id: None,
                mint_write_act: false,
                write_act_t: None,
            },
        )
        .await
        .expect_err("cooled assignment must fail successor admission");
        tx.rollback().await?;
        assert!(matches!(
            err,
            StorageError::ConstraintViolation(ref message)
                if message == "goal assignment perspective does not exist"
        ));

        let after: (i64, i64) = sqlx::query_as(
            "SELECT
                (SELECT count(*)::bigint FROM proxima_core.goal),
                (SELECT count(*)::bigint FROM proxima_core.goal_head)",
        )
        .fetch_one(pool)
        .await?;
        assert_eq!(
            after, before,
            "failed assignment must not mint a Goal or head"
        );
        let source_count: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.goal WHERE t = $1")
                .bind(source.t)
                .fetch_one(pool)
                .await?;
        assert_eq!(source_count, 1, "the retained source Goal remains stored");
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("cooled assignment lifecycle test failed");
}

#[tokio::test]
async fn cooled_carried_wake_rejects_a_goal_successor_without_a_new_head() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url)
            .await?
            .with_cold(Arc::new(MemoryColdStore::default()));
        pg.run_migrations().await?;
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let pool = pg.pool_for_tests();
        let trigger = pg
            .ingest_fact_atomic(&permit, &fact_draft("core/wake-trigger-v1"), None)
            .await?;
        let mut tx = pool.begin().await?;
        let wake_id = insert_wake_config(
            &mut tx,
            &owner,
            &WakeConfigDraft {
                trigger_kind: WakeTriggerKind::FactMemory,
                trigger_schema_id: None,
                trigger_t: Some(trigger.memory_id.into_inner()),
                tool_ids: vec!["core.remember".into()],
                prompt: "cooled carried wake".into(),
                hard_memory_t: vec![],
            },
        )
        .await?;
        let source = write_goal(
            &mut tx,
            &owner,
            &GoalWriteCommand {
                handle: None,
                schema_id: "core/task-v1".into(),
                title: "wake source".into(),
                state: GoalState::Active,
                request_id: "cooled-wake-source".into(),
                close_fact_t: None,
                assignment_t: None,
                dependency_t: vec![],
                evidence_t: vec![],
                wake_id: Some(wake_id),
                mint_write_act: false,
                write_act_t: None,
            },
        )
        .await?;
        tx.commit().await?;

        MemoryAuthoringPort::forget_memory(&pg, &permit, trigger.memory_id).await?;
        let before: (i64, i64) = sqlx::query_as(
            "SELECT
                (SELECT count(*)::bigint FROM proxima_core.goal),
                (SELECT count(*)::bigint FROM proxima_core.goal_head)",
        )
        .fetch_one(pool)
        .await?;
        let mut tx = pool.begin().await?;
        let err = write_goal(
            &mut tx,
            &owner,
            &GoalWriteCommand {
                handle: Some(source.handle),
                schema_id: "core/task-v1".into(),
                title: "rejected wake successor".into(),
                state: GoalState::Active,
                request_id: "cooled-wake-successor".into(),
                close_fact_t: None,
                assignment_t: None,
                dependency_t: vec![],
                evidence_t: vec![],
                wake_id: Some(wake_id),
                mint_write_act: false,
                write_act_t: None,
            },
        )
        .await
        .expect_err("cooled carried wake must fail successor admission");
        tx.rollback().await?;
        assert!(matches!(
            err,
            StorageError::ConstraintViolation(ref message)
                if message == "wake trigger memory does not exist"
        ));

        let after: (i64, i64) = sqlx::query_as(
            "SELECT
                (SELECT count(*)::bigint FROM proxima_core.goal),
                (SELECT count(*)::bigint FROM proxima_core.goal_head)",
        )
        .fetch_one(pool)
        .await?;
        assert_eq!(
            after, before,
            "failed wake carry must not mint a Goal or head"
        );
        let source_count: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.goal WHERE t = $1")
                .bind(source.t)
                .fetch_one(pool)
                .await?;
        assert_eq!(source_count, 1, "the retained source Goal remains stored");
        let wake_count: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.wake_config WHERE wake_id = $1",
        )
        .bind(wake_id)
        .fetch_one(pool)
        .await?;
        assert_eq!(wake_count, 1, "the carried wake config remains stored");
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("cooled carried wake lifecycle test failed");
}

#[tokio::test]
async fn direct_fact_memory_wake_rejects_a_non_fact_trigger() {
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
        let fact = pg
            .ingest_fact_atomic(&permit, &fact_draft("core/visit-v1"), None)
            .await?;
        let abstraction = pg
            .ingest_fact_atomic(&permit, &abstraction_draft(fact.memory_id), None)
            .await?;
        let mut tx = pool.begin().await?;
        let err = insert_wake_config(
            &mut tx,
            &owner,
            &WakeConfigDraft {
                trigger_kind: WakeTriggerKind::FactMemory,
                trigger_schema_id: None,
                trigger_t: Some(abstraction.memory_id.into_inner()),
                tool_ids: vec!["core.remember".into()],
                prompt: "wrong kind".into(),
                hard_memory_t: vec![],
            },
        )
        .await
        .expect_err("FactMemory wake must target a Fact");
        tx.rollback().await?;
        assert!(err.to_string().contains("must be a Fact"), "got {err}");
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.wake_config WHERE trigger_t = $1",
        )
        .bind(abstraction.memory_id.into_inner())
        .fetch_one(pool)
        .await?;
        assert_eq!(count, 0);
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("wrong-kind wake test failed");
}

#[tokio::test]
async fn goal_assignment_and_evidence_erase_before_head_have_one_order() {
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
        let base = pg
            .ingest_fact_atomic(&permit, &fact_draft("core/base-v1"), None)
            .await?;
        let abstraction = pg
            .ingest_fact_atomic(&permit, &abstraction_draft(base.memory_id), None)
            .await?;
        let assignment = pg
            .ingest_fact_atomic(&permit, &perspective_draft(abstraction.memory_id), None)
            .await?;
        let evidence = pg
            .ingest_fact_atomic(&permit, &fact_draft("core/evidence-v1"), None)
            .await?;

        // An erase that already owns the target lock wins. The Goal writer
        // waits, then fails its live-kind check before touching goal_head.
        let assignment_t = assignment.memory_id.into_inner();
        let mut erase_assignment = pool.begin().await?;
        lock_target(&mut erase_assignment, assignment_t).await?;
        let assignment_pool = pool.clone();
        let assignment_owner = owner;
        let mut assignment_writer = tokio::spawn(async move {
            let mut tx = assignment_pool
                .begin()
                .await
                .map_err(|err| err.to_string())?;
            let result = write_goal(
                &mut tx,
                &assignment_owner,
                &GoalWriteCommand {
                    handle: None,
                    schema_id: "core/task-v1".into(),
                    title: "assignment race".into(),
                    state: GoalState::Active,
                    request_id: "assignment-race".into(),
                    close_fact_t: None,
                    assignment_t: Some(assignment_t),
                    dependency_t: vec![],
                    evidence_t: vec![],
                    wake_id: None,
                    mint_write_act: false,
                    write_act_t: None,
                },
            )
            .await
            .map(|_| ())
            .map_err(|err| err.to_string());
            let _ = tx.rollback().await;
            result
        });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut assignment_writer)
                .await
                .is_err(),
            "assignment admission must wait for the target lock"
        );
        erase_memory(
            &mut erase_assignment,
            &proxima_storage_pg::core_pg_sidecars(),
            &erase_surfaces(),
            &owner,
            assignment_t,
        )
        .await?;
        erase_assignment.commit().await?;
        let assignment_error = assignment_writer
            .await?
            .expect_err("erased assignment target must reject");
        assert!(assignment_error.contains("perspective") || assignment_error.contains("exist"));
        let assignment_goals: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.goal WHERE request_id = 'assignment-race'",
        )
        .fetch_one(pool)
        .await?;
        let assignment_heads: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.goal_head h
             WHERE NOT EXISTS (SELECT 1 FROM proxima_core.goal g WHERE g.handle = h.handle)",
        )
        .fetch_one(pool)
        .await?;
        assert_eq!(assignment_goals, 0);
        assert_eq!(assignment_heads, 0, "no orphan head was left behind");

        let evidence_t = evidence.memory_id.into_inner();
        let mut erase_evidence = pool.begin().await?;
        lock_target(&mut erase_evidence, evidence_t).await?;
        let evidence_pool = pool.clone();
        let evidence_owner = owner;
        let mut evidence_writer = tokio::spawn(async move {
            let mut tx = evidence_pool.begin().await.map_err(|err| err.to_string())?;
            let result = write_goal(
                &mut tx,
                &evidence_owner,
                &GoalWriteCommand {
                    handle: None,
                    schema_id: "core/task-v1".into(),
                    title: "evidence race".into(),
                    state: GoalState::Active,
                    request_id: "evidence-race".into(),
                    close_fact_t: None,
                    assignment_t: None,
                    dependency_t: vec![],
                    evidence_t: vec![evidence_t],
                    wake_id: None,
                    mint_write_act: false,
                    write_act_t: None,
                },
            )
            .await
            .map(|_| ())
            .map_err(|err| err.to_string());
            let _ = tx.rollback().await;
            result
        });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut evidence_writer)
                .await
                .is_err(),
            "evidence admission must wait for the target lock"
        );
        erase_memory(
            &mut erase_evidence,
            &proxima_storage_pg::core_pg_sidecars(),
            &erase_surfaces(),
            &owner,
            evidence_t,
        )
        .await?;
        erase_evidence.commit().await?;
        let evidence_error = evidence_writer
            .await?
            .expect_err("erased evidence target must reject");
        assert!(evidence_error.contains("evidence") || evidence_error.contains("exist"));
        let evidence_goals: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.goal WHERE request_id = 'evidence-race'",
        )
        .fetch_one(pool)
        .await?;
        assert_eq!(evidence_goals, 0);
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("Goal target lifecycle ordering failed");
}

async fn lock_target(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    t: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "SELECT pg_advisory_xact_lock(
             hashtextextended('proxima-forget:' || $1::text, 0)
         )",
    )
    .bind(t)
    .execute(&mut **tx)
    .await
    .map(|_| ())
}
