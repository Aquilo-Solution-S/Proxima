//! Slice 2: FactIngest timeseries write/read + ingest_keys replay.
#![allow(
    clippy::missing_errors_doc,
    clippy::doc_markdown,
    clippy::too_many_lines,
    clippy::used_underscore_binding
)]

use proxima_core::storage_ports::OwnerWritePermit;
use proxima_core::verbs::fact_ingest::FactWriteCommand;
use proxima_core::verbs::goal_write::GoalState;
use proxima_core::{AccessKind, GroupId, OwnerRef, SchemaId, SchemaVersion, StorageError, UserId};
use proxima_pg_testkit::{create_db, db_url, drop_db};
use proxima_storage_pg::PgStorage;
use proxima_storage_pg::access::owner_columns::ensure_owner_row;
use proxima_storage_pg::verbs::fact_ingest::ingest_fact_atomic;
use proxima_storage_pg::verbs::goal_timeseries::{GoalWriteCommand, write_goal};
use proxima_storage_pg::verbs::memory_timeseries::{read_memory_by_t, read_memory_head};
use proxima_storage_pg::verbs::wake_timeseries::{
    WakeConfigDraft, WakeTriggerKind, insert_wake_config,
};
use uuid::Uuid;

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
async fn memory_timeseries_keyless_and_ingest_key_replay() {
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

        let first = ingest_fact_atomic(pg.pool_for_tests(), &permit, &draft(None), None).await?;
        let second = ingest_fact_atomic(pg.pool_for_tests(), &permit, &draft(None), None).await?;
        assert_ne!(
            first.memory_id, second.memory_id,
            "keyless Fact must mint a new t"
        );
        assert_ne!(first.handle, second.handle);

        let sourced = draft(Some(("src/webhook", "delivery-1")));
        let a = ingest_fact_atomic(pg.pool_for_tests(), &permit, &sourced, None).await?;
        assert!(!a.idempotent_replay);
        let b = ingest_fact_atomic(pg.pool_for_tests(), &permit, &sourced, None).await?;
        assert!(b.idempotent_replay);
        assert_eq!(a.memory_id, b.memory_id);
        assert_eq!(a.handle, b.handle);

        let mut tx = pg.pool_for_tests().begin().await?;
        let by_t = read_memory_by_t(&mut tx, a.memory_id.into_inner())
            .await?
            .expect("read by t");
        let by_head = read_memory_head(&mut tx, a.handle)
            .await?
            .expect("read by head");
        tx.commit().await?;
        assert_eq!(by_t.t, a.memory_id.into_inner());
        assert_eq!(by_head.t, a.memory_id.into_inner());
        assert_eq!(by_t.handle, a.handle);
        assert_eq!(by_head.handle, a.handle);

        let head_t: Uuid =
            sqlx::query_scalar("SELECT t FROM proxima_core.memory_head WHERE handle = $1")
                .bind(a.handle)
                .fetch_one(pg.pool_for_tests())
                .await?;
        assert_eq!(
            head_t,
            a.memory_id.into_inner(),
            "replay must not bump head"
        );

        let row_schema: String =
            sqlx::query_scalar("SELECT schema_id FROM proxima_core.memory WHERE t = $1")
                .bind(a.memory_id.into_inner())
                .fetch_one(pg.pool_for_tests())
                .await?;
        let head_schema: String =
            sqlx::query_scalar("SELECT schema_id FROM proxima_core.memory_head WHERE handle = $1")
                .bind(a.handle)
                .fetch_one(pg.pool_for_tests())
                .await?;
        assert_eq!(row_schema, "core/test-fact-v1");
        assert_eq!(row_schema, head_schema);

        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("memory_timeseries test failed");
}

#[tokio::test]
async fn memory_timeseries_pins_blob_and_closed_handle() {
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

        let file = ingest_fact_atomic(pool, &permit, &draft(None), None).await?;
        let mut chunk = draft(None);
        chunk.refs = vec![file.memory_id.into_inner()];
        let chunk_out = ingest_fact_atomic(pool, &permit, &chunk, None).await?;
        let mut tx = pool.begin().await?;
        let row = read_memory_by_t(&mut tx, chunk_out.memory_id.into_inner())
            .await?
            .expect("chunk");
        tx.commit().await?;
        assert_eq!(row.refs, vec![file.memory_id.into_inner()]);

        let other = ingest_fact_atomic(pool, &permit, &draft(None), None).await?;
        let mut many = draft(None);
        many.refs = vec![
            file.memory_id.into_inner(),
            chunk_out.memory_id.into_inner(),
            other.memory_id.into_inner(),
        ];
        let many_out = ingest_fact_atomic(pool, &permit, &many, None)
            .await
            .expect("multi-pin refs");
        let mut tx = pool.begin().await?;
        let many_row = read_memory_by_t(&mut tx, many_out.memory_id.into_inner())
            .await?
            .expect("many");
        tx.commit().await?;
        assert_eq!(many_row.refs.len(), 3);

        let missing = Uuid::now_v7();
        let mut bad = draft(None);
        bad.refs = vec![missing];
        let err = ingest_fact_atomic(pool, &permit, &bad, None)
            .await
            .expect_err("missing pin");
        assert!(
            err.to_string().contains("does not exist") || err.to_string().contains("23503"),
            "got: {err}"
        );

        sqlx::query("INSERT INTO proxima_core.closed_handle (handle) VALUES ($1)")
            .bind(file.handle)
            .execute(pool)
            .await?;
        let mut closed = draft(None);
        closed.refs = vec![file.memory_id.into_inner()];
        let err = ingest_fact_atomic(pool, &permit, &closed, None)
            .await
            .expect_err("closed_handle");
        assert!(
            err.to_string().contains("closed_handle") || err.to_string().contains("23514"),
            "got: {err}"
        );

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
        let mut cited = draft(None);
        cited.blob_id = Some(blob_id);
        let cited_out = ingest_fact_atomic(pool, &permit, &cited, None).await?;
        let stored: Option<Uuid> =
            sqlx::query_scalar("SELECT blob_id FROM proxima_core.memory WHERE t = $1")
                .bind(cited_out.memory_id.into_inner())
                .fetch_one(pool)
                .await?;
        assert_eq!(stored, Some(blob_id));

        let edges: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM information_schema.tables
              WHERE table_schema = 'proxima_core' AND table_name = 'edges'",
        )
        .fetch_one(pool)
        .await?;
        assert_eq!(edges, 0, "no edges table");

        let mut abs = draft(None);
        abs.kind = "abstraction".into();
        abs.derived_from = vec![proxima_core::EdgeEndpoint::memory(
            proxima_core::EntityKind::Fact,
            chunk_out.memory_id,
        )];
        let abs_out = ingest_fact_atomic(pool, &permit, &abs, None)
            .await
            .expect("A origins Fact t");

        let mut abs_many = draft(None);
        abs_many.kind = "abstraction".into();
        abs_many.derived_from = vec![
            proxima_core::EdgeEndpoint::memory(proxima_core::EntityKind::Fact, chunk_out.memory_id),
            proxima_core::EdgeEndpoint::memory(proxima_core::EntityKind::Fact, other.memory_id),
        ];
        ingest_fact_atomic(pool, &permit, &abs_many, None)
            .await
            .expect("A origins many Facts");

        let mut abs_from_abs = draft(None);
        abs_from_abs.kind = "abstraction".into();
        abs_from_abs.derived_from = vec![proxima_core::EdgeEndpoint::memory(
            proxima_core::EntityKind::Abstraction,
            abs_out.memory_id,
        )];
        ingest_fact_atomic(pool, &permit, &abs_from_abs, None)
            .await
            .expect("A origins Abstraction t");

        let mut persp_from_fact = draft(None);
        persp_from_fact.kind = "perspective".into();
        persp_from_fact.derived_from = vec![proxima_core::EdgeEndpoint::memory(
            proxima_core::EntityKind::Fact,
            chunk_out.memory_id,
        )];
        let err = ingest_fact_atomic(pool, &permit, &persp_from_fact, None)
            .await
            .expect_err("P origins Fact");
        assert!(
            err.to_string().contains("perspective origins") || err.to_string().contains("23514"),
            "got: {err}"
        );

        let mut persp_ok = draft(None);
        persp_ok.kind = "perspective".into();
        persp_ok.derived_from = vec![proxima_core::EdgeEndpoint::memory(
            proxima_core::EntityKind::Abstraction,
            abs_out.memory_id,
        )];
        let persp_out = ingest_fact_atomic(pool, &permit, &persp_ok, None)
            .await
            .expect("P origins A");

        let mut abs_from_p = draft(None);
        abs_from_p.kind = "abstraction".into();
        abs_from_p.derived_from = vec![proxima_core::EdgeEndpoint::memory(
            proxima_core::EntityKind::Perspective,
            persp_out.memory_id,
        )];
        let err = ingest_fact_atomic(pool, &permit, &abs_from_p, None)
            .await
            .expect_err("A origins P");
        assert!(
            err.to_string().contains("abstraction origins") || err.to_string().contains("23514"),
            "got: {err}"
        );

        sqlx::query(
            "CREATE TABLE proxima_core.sidecar_sum (
                 t uuid PRIMARY KEY REFERENCES proxima_core.memory (t),
                 text text NOT NULL,
                 lexical_language regconfig NOT NULL DEFAULT 'simple'
             )",
        )
        .execute(pool)
        .await?;
        sqlx::query("INSERT INTO proxima_core.sidecar_sum (t, text) VALUES ($1, 'summary')")
            .bind(abs_out.memory_id.into_inner())
            .execute(pool)
            .await?;

        let mut persp = draft(None);
        persp.kind = "perspective".into();
        persp.derived_from = vec![proxima_core::EdgeEndpoint::memory(
            proxima_core::EntityKind::Abstraction,
            abs_out.memory_id,
        )];
        persp.blob_id = Some(blob_id);
        let err = ingest_fact_atomic(pool, &permit, &persp, None)
            .await
            .expect_err("P cannot cite");
        assert!(
            err.to_string().contains("blob") || err.to_string().contains("23514"),
            "got: {err}"
        );

        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("pin/blob test failed");
}

fn assert_kind_conflict(err: StorageError) {
    match err {
        StorageError::ConstraintViolation(msg) => {
            assert!(
                msg.contains("owners.kind conflict"),
                "got ConstraintViolation {msg}"
            );
        }
        other => panic!("expected kind conflict, got {other}"),
    }
}

#[tokio::test]
async fn owners_upsert_rejects_kind_conflict_on_every_write_path() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let id = Uuid::now_v7();
        let personal = OwnerRef::Personal(UserId::new(id));
        let group = OwnerRef::Group(GroupId::new(id));
        let permit = OwnerWritePermit::new_for_tests(personal, AccessKind::Fact);
        let pool = pg.pool_for_tests();

        ingest_fact_atomic(pool, &permit, &draft(None), None).await?;
        {
            let mut conn = pool.acquire().await?;
            ensure_owner_row(&mut conn, &personal).await?;
            assert_kind_conflict(
                ensure_owner_row(&mut conn, &group)
                    .await
                    .expect_err("helper"),
            );
        }

        let mut tx = pool.begin().await?;
        assert_kind_conflict(
            write_goal(
                &mut tx,
                &group,
                &GoalWriteCommand {
                    handle: None,
                    schema_id: "core/task-v1".into(),
                    title: "kind conflict".into(),
                    state: GoalState::Active,
                    request_id: "kind-conflict".into(),
                    close_fact_t: None,
                    assignment_t: None,
                    dependency_t: vec![],
                    evidence_t: vec![],
                    wake_id: None,
                    mint_write_act: false,
                    write_act_t: None,
                },
            )
            .await
            .expect_err("goal"),
        );
        assert_kind_conflict(
            insert_wake_config(
                &mut tx,
                &group,
                &WakeConfigDraft {
                    trigger_kind: WakeTriggerKind::FactSchema,
                    trigger_schema_id: Some("core/visit-v1".into()),
                    trigger_t: None,
                    tool_ids: vec!["core.remember".into()],
                    prompt: "on visit".into(),
                    hard_memory_t: vec![],
                },
            )
            .await
            .expect_err("wake"),
        );
        tx.rollback().await?;

        let group_permit = OwnerWritePermit::new_for_tests(group, AccessKind::Fact);
        assert_kind_conflict(
            ingest_fact_atomic(pool, &group_permit, &draft(None), None)
                .await
                .expect_err("fact"),
        );

        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("owners kind-conflict test failed");
}

#[tokio::test]
async fn ensure_owner_row_returns_under_concurrent_first_insert() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let pool = pg.pool_for_tests();
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));

        let mut joins = Vec::new();
        for _ in 0..8 {
            let pool = pool.clone();
            joins.push(tokio::spawn(async move {
                let mut conn = pool
                    .acquire()
                    .await
                    .map_err(|err| StorageError::Unavailable(format!("acquire: {err}")))?;
                ensure_owner_row(&mut conn, &owner).await
            }));
        }
        for join in joins {
            join.await??;
        }
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("concurrent ensure_owner_row must not RowNotFound");
}

#[test]
fn memory_pin_checks_is_set_based() {
    let src = include_str!("../migrations/0001_v008.sql");
    let start = src
        .find("CREATE FUNCTION proxima_core.memory_pin_checks")
        .expect("memory_pin_checks");
    let next = src[start + 1..]
        .find("CREATE FUNCTION")
        .expect("next function");
    let body = &src[start..start + 1 + next];
    assert!(
        !body.contains("FOREACH"),
        "pin checks must not walk pins in plpgsql"
    );
    assert!(
        body.contains("unnest") && body.contains("= ANY"),
        "pin checks must be set predicates"
    );
}
