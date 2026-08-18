//! P2: publish-to-World is an in-place series transfer.
#![allow(clippy::doc_markdown, clippy::too_many_lines)]

use proxima_core::storage_ports::{OwnerTransferPort, OwnerWritePermit};
use proxima_core::verbs::fact_ingest::FactWriteCommand;
use proxima_core::verbs::goal_write::GoalState;
use proxima_core::{AccessKind, EntityId, GoalId, OwnerRef, SchemaId, SchemaVersion, UserId};
use proxima_pg_testkit::{create_db, db_url, drop_db};
use proxima_storage_pg::PgStorage;
use proxima_storage_pg::verbs::fact_ingest::ingest_fact_atomic;
use proxima_storage_pg::verbs::goal_timeseries::{GoalWriteCommand, write_goal};
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

#[tokio::test]
async fn publish_transfers_same_memory_t_and_sidecar() {
    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
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
            .transfer_to_world(&permit, EntityId::Memory(written.memory_id))
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
            OwnerRef::World.stored_owner_id(),
            "publish must re-home Content with the Memory"
        );
        let old_left: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.content WHERE content_id = $1",
        )
        .bind(content_id)
        .fetch_one(pool)
        .await?;
        assert_eq!(
            old_left, 0,
            "origin Content is GC'd after exclusive publish"
        );
        let owner_id: Uuid =
            sqlx::query_scalar("SELECT owner_id FROM proxima_core.memory WHERE t = $1")
                .bind(t)
                .fetch_one(pool)
                .await?;
        assert_eq!(owner_id, OwnerRef::World.stored_owner_id());
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
            .transfer_to_world(&permit, EntityId::Memory(written.memory_id))
            .await?;
        assert!(!replay, "already World is a clean false");
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("memory transfer failed");
}

#[tokio::test]
async fn publish_rehomes_cooled_versions_and_remints_object_key() {
    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        use proxima_core::storage_ports::MemoryAuthoringPort;
        use proxima_storage_pg::verbs::forget::{cold_object_key, owner_hash_hex};

        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let pool = pg.pool_for_tests();
        let first = ingest_fact_atomic(pool, &permit, &draft(), None).await?;
        MemoryAuthoringPort::forget_memory(&pg, &permit, first.memory_id).await?;
        let mut later = draft();
        later.handle = Some(first.handle);
        later.ingest_key = Some("k2".into());
        let second = ingest_fact_atomic(pool, &permit, &later, None).await?;
        assert_eq!(second.handle, first.handle);

        let transferred = pg
            .transfer_to_world(&permit, EntityId::Memory(second.memory_id))
            .await?;
        assert!(transferred);
        let cooled_owner: uuid::Uuid =
            sqlx::query_scalar("SELECT owner_id FROM proxima_core.cooled WHERE t = $1")
                .bind(first.memory_id.into_inner())
                .fetch_one(pool)
                .await?;
        assert_eq!(
            cooled_owner,
            OwnerRef::World.stored_owner_id(),
            "publish must re-home cooled versions with the series"
        );
        let object_key: String =
            sqlx::query_scalar("SELECT object_key FROM proxima_core.cooled WHERE t = $1")
                .bind(first.memory_id.into_inner())
                .fetch_one(pool)
                .await?;
        let expected = cold_object_key(
            &owner_hash_hex(&OwnerRef::World),
            first.handle,
            first.memory_id.into_inner(),
        );
        assert_eq!(object_key, expected);
        let personal_prefix = format!("cold/{}/", owner_hash_hex(&owner));
        assert!(
            !object_key.starts_with(&personal_prefix),
            "reminted key must not stay under the personal owner hash"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("publish cooled remint failed");
}

#[tokio::test]
async fn publish_transfers_goal_same_t() {
    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Goal);
        let pool = pg.pool_for_tests();
        let mut tx = pool.begin().await?;
        let out = write_goal(
            &mut tx,
            &owner,
            &GoalWriteCommand {
                handle: None,
                schema_id: "core/task-goal-v1".into(),
                title: "publish me".into(),
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

        let transferred = pg
            .transfer_to_world(&permit, EntityId::Goal(GoalId::new(out.t)))
            .await?;
        assert!(transferred);
        let head_owner: Uuid =
            sqlx::query_scalar("SELECT owner_id FROM proxima_core.goal_head WHERE handle = $1")
                .bind(out.handle)
                .fetch_one(pool)
                .await?;
        assert_eq!(head_owner, OwnerRef::World.stored_owner_id());
        let row_owner: Uuid =
            sqlx::query_scalar("SELECT owner_id FROM proxima_core.goal WHERE t = $1")
                .bind(out.t)
                .fetch_one(pool)
                .await?;
        assert_eq!(row_owner, OwnerRef::World.stored_owner_id());
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("goal transfer failed");
}

#[tokio::test]
async fn publish_moves_exclusive_blob_with_the_fact() {
    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
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
            pg.transfer_to_world(&permit, EntityId::Memory(written.memory_id))
                .await?
        );
        let blob_owner: Uuid =
            sqlx::query_scalar("SELECT owner_id FROM proxima_core.blob WHERE blob_id = $1")
                .bind(blob_id)
                .fetch_one(pool)
                .await?;
        assert_eq!(blob_owner, OwnerRef::World.stored_owner_id());
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
