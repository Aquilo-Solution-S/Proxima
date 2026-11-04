//! A schema drop proves emptiness under the same writer fence as its DDL.

use std::borrow::Cow;
use std::time::Duration;

use proxiima_core::owner_inverse::OwnerSurfaces;
use proxiima_core::test_fixtures::owner_scope_for_tests;
use proxiima_core::{OwnerId, SpiiceRegistry, UserId};
use proxiima_pg_testkit::SplitRoleDb;
use proxiima_storage_pg::verbs::forget::erase_memory_series;
use proxiima_storage_pg::{
    PgHostStateEraseContext, PgMigrationStorage, PgPoolConfig, PgTuning,
    begin_migration_transaction, begin_owner_transaction, core_pg_sidecars,
};
use sqlx::migrate::{Migration, MigrationType, Migrator};
use sqlx::postgres::PgPoolOptions;
use sqlx::{Connection, PgConnection, PgPool, Postgres, SqlSafeStr, Transaction};
use uuid::Uuid;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

const SCHEMA: &str = "test/retired-v1";
const TABLE: &str = "proxiima_core.agent_note_v1";
const VERSION: i64 = 20_991_104_521_400;
const PATIENCE: Duration = Duration::from_secs(10);
const GUARD: &str = "SELECT proxiima_core.assert_fact_schema_drained($1, $2::text[])";

struct Harness {
    platform: PgPool,
    runtime: PgPool,
    admin: PgPool,
    owners: [OwnerId; 2],
    db: SplitRoleDb,
}

impl Harness {
    async fn new() -> TestResult<Self> {
        let db = SplitRoleDb::create("proxiima_drop_guard", &[]).await?;
        let storage = PgMigrationStorage::connect_with_config(
            db.platform_url(),
            PgPoolConfig::default(),
            PgTuning::default(),
        )
        .await?;
        storage.run_migrations().await?;
        let platform = storage.clone_pool_for_backend();
        let runtime = PgPoolOptions::new()
            .max_connections(2)
            .connect(db.runtime_url())
            .await?;
        let admin = PgPoolOptions::new()
            .max_connections(2)
            .connect(&db.admin_url())
            .await?;
        let owners = [(); 2].map(|()| OwnerId::from(UserId::new(Uuid::now_v7())));
        let mut connection = platform.acquire().await?;
        let mut tx = begin_migration_transaction(&mut connection).await?;
        for owner in owners {
            sqlx::query("INSERT INTO proxiima_core.owners(owner_id, kind) VALUES ($1, 'personal')")
                .bind(owner.into_inner())
                .execute(&mut *tx)
                .await?;
        }
        sqlx::raw_sql(
            "CREATE TABLE public.schema_drop_unrelated (value text);
             INSERT INTO public.schema_drop_unrelated VALUES ('keep');
             CREATE TABLE public._sqlx_migrations_drop_guard (
                 version bigint PRIMARY KEY, description text NOT NULL,
                 installed_on timestamptz NOT NULL DEFAULT now(), success boolean NOT NULL,
                 checksum bytea NOT NULL, execution_time bigint NOT NULL)",
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        drop(connection);
        Ok(Self {
            platform,
            runtime,
            admin,
            owners,
            db,
        })
    }

    async fn fact(&self, schema: &str, stamped: bool, forgotten: bool) -> TestResult<Uuid> {
        let mut connection = self.platform.acquire().await?;
        let mut tx = begin_migration_transaction(&mut connection).await?;
        let t = insert_fact(&mut tx, self.owners[1], schema, stamped, forgotten).await?;
        tx.commit().await?;
        Ok(t)
    }

    async fn drop_schema(&self) -> TestResult<Result<(), sqlx::migrate::MigrateError>> {
        let mut connection = self.platform.acquire().await?;
        let mut tx = begin_migration_transaction(&mut connection).await?;
        let result = drop_plan().run_direct(None, tx.as_mut(), false).await;
        if result.is_ok() {
            tx.commit().await?;
        } else {
            tx.rollback().await?;
        }
        Ok(result)
    }

    async fn erase(&self, t: Uuid) -> TestResult {
        let surfaces =
            OwnerSurfaces::for_registry(&SpiiceRegistry::new().freeze_or_panic_for_tests());
        let context = PgHostStateEraseContext::for_surfaces_for_tests(surfaces)?;
        let mut connection = self.platform.acquire().await?;
        let mut tx = begin_migration_transaction(&mut connection).await?;
        let (count, _) = erase_memory_series(
            &mut tx,
            &core_pg_sidecars(),
            &context,
            &self.owners[1],
            &[t],
        )
        .await?;
        assert_eq!(count, 1);
        tx.commit().await?;
        Ok(())
    }
}

async fn insert_fact(
    tx: &mut Transaction<'_, Postgres>,
    owner: OwnerId,
    schema: &str,
    stamped: bool,
    forgotten: bool,
) -> Result<Uuid, sqlx::Error> {
    let t = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO proxiima_core.memory_head(handle,t,kind,owner_id,schema_id)
         VALUES ($1,$1,'fact',$2,$3)",
    )
    .bind(t)
    .bind(owner.into_inner())
    .bind(schema)
    .execute(&mut **tx)
    .await?;
    let stamps: Vec<&str> = if stamped { vec![TABLE] } else { Vec::new() };
    sqlx::query(
        "INSERT INTO proxiima_core.memory
             (handle,t,kind,owner_id,schema_id,sidecar_tables,occurred_at,forgotten_at,\"by\",via)
         VALUES ($1,$1,'fact',$2,$3,$4,uuid_extract_timestamp($1),
                 CASE WHEN $5 THEN now() ELSE NULL END,
                 '00000000-0000-0000-0000-000000000509'::uuid,NULL)",
    )
    .bind(t)
    .bind(owner.into_inner())
    .bind(schema)
    .bind(stamps)
    .bind(forgotten)
    .execute(&mut **tx)
    .await?;
    if stamped {
        sqlx::query(
            "INSERT INTO proxiima_core.agent_note_v1(t,note_id,title,body)
             VALUES ($1,$1,'retired fixture','retired fixture')",
        )
        .bind(t)
        .execute(&mut **tx)
        .await?;
    }
    Ok(t)
}

fn drop_plan() -> Migrator {
    let mut migrator = Migrator {
        migrations: Cow::Owned(vec![Migration::new(
            VERSION,
            Cow::Borrowed("drop retired fixture"),
            MigrationType::Simple,
            sqlx::AssertSqlSafe(
                "SELECT proxiima_core.assert_fact_schema_drained(
                     'test/retired-v1', ARRAY['proxiima_core.agent_note_v1']);
                 DROP TRIGGER memory_declares_proxiima_core_agent_note_v1 ON proxiima_core.memory;
                 DROP TABLE proxiima_core.agent_note_v1;
                 DELETE FROM proxiima_core.spiice_surface
                  WHERE table_name = 'proxiima_core.agent_note_v1';"
                    .to_owned(),
            )
            .into_sql_str(),
            false,
        )]),
        ..Migrator::DEFAULT
    };
    migrator.dangerous_set_table_name("public._sqlx_migrations_drop_guard");
    migrator
}

#[tokio::test]
async fn every_stored_reference_refuses_drop_until_the_core_inverse_erases_it() -> TestResult {
    // Each variant runs a complete migration and inverse on its own database.
    for (schema, stamped, forgotten) in [
        (SCHEMA, false, false),
        (SCHEMA, true, true),
        ("test/other-v1", true, false),
    ] {
        let h = Harness::new().await?;
        let retired = h.fact(schema, stamped, forgotten).await?;
        let unrelated = h.fact("test/keep-v1", false, false).await?;
        let error = h
            .drop_schema()
            .await?
            .expect_err("stored reference refuses DROP");
        let sqlx::migrate::MigrateError::ExecuteMigration(ref cause, _) = error else {
            panic!("expected a refused migration, got {error}");
        };
        assert_sqlstate(cause, "23503");
        let hint = cause
            .as_database_error()
            .unwrap()
            .downcast_ref::<sqlx::postgres::PgDatabaseError>()
            .hint()
            .unwrap();
        assert!(hint.contains("erase-capable"), "{hint}");
        let (exists, rows, ledger): (bool, i64, i64) = sqlx::query_as(
            "SELECT to_regclass('proxiima_core.agent_note_v1') IS NOT NULL,
                    (SELECT count(*) FROM proxiima_core.memory WHERE t=$1),
                    (SELECT count(*) FROM public._sqlx_migrations_drop_guard)",
        )
        .bind(retired)
        .fetch_one(&h.admin)
        .await?;
        assert!(exists);
        assert_eq!((rows, ledger), (1, 0));
        let sidecar_rows: i64 =
            sqlx::query_scalar("SELECT count(*) FROM proxiima_core.agent_note_v1 WHERE t=$1")
                .bind(retired)
                .fetch_one(&h.admin)
                .await?;
        assert_eq!(sidecar_rows, i64::from(stamped));

        h.erase(retired).await?;
        h.drop_schema().await??;
        let (exists, rows, ledger, value): (bool, i64, i64, String) = sqlx::query_as(
            "SELECT to_regclass('proxiima_core.agent_note_v1') IS NOT NULL,
                    (SELECT count(*) FROM proxiima_core.memory WHERE t=$1),
                    (SELECT count(*) FROM public._sqlx_migrations_drop_guard WHERE version=$2),
                    (SELECT value FROM public.schema_drop_unrelated)",
        )
        .bind(unrelated)
        .bind(VERSION)
        .fetch_one(&h.admin)
        .await?;
        assert!(!exists);
        assert_eq!((rows, ledger, value.as_str()), (1, 1, "keep"));
    }
    Ok(())
}

#[tokio::test]
async fn filtered_and_unbound_connections_cannot_certify_an_empty_store() -> TestResult {
    let h = Harness::new().await?;
    h.fact(SCHEMA, true, false).await?;
    let (safe, forced): (bool, bool) = sqlx::query_as(
        "SELECT NOT r.rolsuper AND NOT r.rolbypassrls, c.relrowsecurity AND c.relforcerowsecurity
           FROM pg_roles r CROSS JOIN pg_class c
          WHERE r.rolname=current_user AND c.oid='proxiima_core.memory'::regclass",
    )
    .fetch_one(&h.platform)
    .await?;
    assert!(safe && forced);
    let mut connection = h.platform.acquire().await?;
    let mut tx = begin_migration_transaction(&mut connection).await?;
    insert_fact(&mut tx, h.owners[0], "test/keep-v1", false, false).await?;
    tx.commit().await?;
    drop(connection);
    let mut owner_tx =
        begin_owner_transaction(&h.runtime, &owner_scope_for_tests(&[h.owners[0]])).await?;
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM proxiima_core.memory WHERE schema_id='test/retired-v1'",
    )
    .fetch_one(&mut *owner_tx)
    .await?;
    assert_eq!(count, 0, "the other owner's Fact is invisible");
    let refused = sqlx::query(GUARD)
        .bind(SCHEMA)
        .bind(vec![TABLE])
        .execute(&mut *owner_tx)
        .await
        .expect_err("owner view is no proof");
    assert_sqlstate(&refused, "42501");
    owner_tx.rollback().await?;
    let mut connection = h.platform.acquire().await?;
    let mut tx = begin_migration_transaction(&mut connection).await?;
    let all: i64 = sqlx::query_scalar("SELECT count(DISTINCT owner_id) FROM proxiima_core.memory")
        .fetch_one(&mut *tx)
        .await?;
    assert_eq!(all, 2, "the platform proof reaches both owners");
    let refused = sqlx::query(GUARD)
        .bind(SCHEMA)
        .bind(vec![TABLE])
        .execute(&mut *tx)
        .await
        .expect_err("foreign owner's Fact prevents DROP");
    assert_sqlstate(&refused, "23503");
    tx.rollback().await?;
    drop(connection);
    let mut unbound = PgConnection::connect(h.db.platform_url()).await?;
    let refused = sqlx::query(GUARD)
        .bind(SCHEMA)
        .bind(vec![TABLE])
        .execute(&mut unbound)
        .await
        .expect_err("unbound platform role is no proof");
    assert_sqlstate(&refused, "42501");
    let mut spoofed = h.runtime.begin().await?;
    sqlx::query("SELECT set_config('app.proxiima_scope','platform',true)")
        .execute(&mut *spoofed)
        .await?;
    let refused = sqlx::query(GUARD)
        .bind(SCHEMA)
        .bind(vec![TABLE])
        .execute(&mut *spoofed)
        .await
        .expect_err("scope alone is no authority");
    assert_sqlstate(&refused, "42501");
    Ok(())
}

fn assert_sqlstate(error: &sqlx::Error, expected: &str) {
    assert_eq!(
        error
            .as_database_error()
            .and_then(sqlx::error::DatabaseError::code)
            .as_deref(),
        Some(expected),
        "{error}"
    );
}

#[tokio::test]
async fn arguments_and_snapshot_isolation_are_refused_at_the_guard() -> TestResult {
    let h = Harness::new().await?;
    for (schema, tables) in [
        (None, Some(vec![Some(TABLE)])),
        (Some(""), Some(vec![Some(TABLE)])),
        (Some(SCHEMA), None),
        (Some(SCHEMA), Some(vec![])),
        (Some(SCHEMA), Some(vec![None])),
        (Some(SCHEMA), Some(vec![Some("agent_note_v1")])),
        (Some(SCHEMA), Some(vec![Some("proxiima_core.missing")])),
        (
            Some(SCHEMA),
            Some(vec![Some("public.schema_drop_unrelated")]),
        ),
    ] {
        let mut connection = h.platform.acquire().await?;
        let mut tx = begin_migration_transaction(&mut connection).await?;
        let refused = sqlx::query(GUARD)
            .bind(schema)
            .bind(tables)
            .execute(&mut *tx)
            .await
            .expect_err("invalid external arguments must refuse");
        assert_sqlstate(&refused, "22023");
        tx.rollback().await?;
    }
    for serializable in [false, true] {
        let mut connection = h.platform.acquire().await?;
        let mut tx = connection.begin().await?;
        if serializable {
            sqlx::query("SET TRANSACTION ISOLATION LEVEL SERIALIZABLE")
                .execute(&mut *tx)
                .await?;
        } else {
            sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
                .execute(&mut *tx)
                .await?;
        }
        sqlx::query("SELECT set_config('app.proxiima_scope','platform',true)")
            .execute(&mut *tx)
            .await?;
        let refused = sqlx::query(GUARD)
            .bind(SCHEMA)
            .bind(vec![TABLE])
            .execute(&mut *tx)
            .await
            .expect_err("an older transaction snapshot cannot prove emptiness");
        assert_sqlstate(&refused, "25001");
    }
    Ok(())
}

async fn wait_until_blocked(observer: &PgPool, waiter: i32, holder: i32) -> TestResult {
    tokio::time::timeout(PATIENCE, async {
        loop {
            let waiting: bool = sqlx::query_scalar(
                "SELECT $2=ANY(pg_blocking_pids($1)) AND EXISTS (
                     SELECT 1 FROM pg_locks WHERE pid=$1 AND NOT granted
                      AND relation='proxiima_core.memory'::regclass)",
            )
            .bind(waiter)
            .bind(holder)
            .fetch_one(observer)
            .await?;
            if waiting {
                return Ok::<(), sqlx::Error>(());
            }
            tokio::task::yield_now().await;
        }
    })
    .await??;
    Ok(())
}

#[tokio::test]
async fn a_committed_admission_is_seen_after_the_guard_waits_for_its_writer() -> TestResult {
    let h = Harness::new().await?;
    let mut writer =
        begin_owner_transaction(&h.runtime, &owner_scope_for_tests(&[h.owners[1]])).await?;
    let writer_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *writer)
        .await?;
    let t = insert_fact(&mut writer, h.owners[1], SCHEMA, true, false).await?;
    let platform = h.platform.clone();
    let (pid_tx, pid_rx) = tokio::sync::oneshot::channel();
    let dropper = tokio::spawn(async move {
        let mut connection = platform.acquire().await.unwrap();
        let mut tx = begin_migration_transaction(&mut connection).await.unwrap();
        let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        pid_tx.send(pid).unwrap();
        drop_plan().run_direct(None, tx.as_mut(), false).await
    });
    wait_until_blocked(&h.admin, pid_rx.await?, writer_pid).await?;
    writer.commit().await?;
    let result = tokio::time::timeout(PATIENCE, dropper).await??;
    let error = result.expect_err("the committed admission must prevent DROP");
    let sqlx::migrate::MigrateError::ExecuteMigration(cause, _) = error else {
        panic!("expected a refused migration, got {error}");
    };
    assert_sqlstate(&cause, "23503");
    let (memory, sidecar, ledger): (i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM proxiima_core.memory WHERE t=$1),
                (SELECT count(*) FROM proxiima_core.agent_note_v1 WHERE t=$1),
                (SELECT count(*) FROM public._sqlx_migrations_drop_guard)",
    )
    .bind(t)
    .fetch_one(&h.admin)
    .await?;
    assert_eq!((memory, sidecar, ledger), (1, 1, 0));
    Ok(())
}

#[tokio::test]
async fn writers_queued_after_the_guard_cannot_commit_a_dangling_stamp() -> TestResult {
    for repeatable_read in [false, true] {
        let h = Harness::new().await?;
        let mut writer_connection = h.runtime.acquire().await?;
        let mut writer = writer_connection.begin().await?;
        if repeatable_read {
            sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
                .execute(&mut *writer)
                .await?;
        }
        sqlx::query("SELECT set_config('app.proxiima_scope','owner',true), set_config('app.owner',$1,true), set_config('app.write_owner',$1,true)")
            .bind(format!("{{{}}}",h.owners[1].into_inner())).execute(&mut *writer).await?;
        let _: i64 = sqlx::query_scalar("SELECT count(*) FROM proxiima_core.spiice_surface")
            .fetch_one(&mut *writer)
            .await?;
        let mut drop_connection = h.platform.acquire().await?;
        let mut drop_tx = begin_migration_transaction(&mut drop_connection).await?;
        sqlx::query(GUARD)
            .bind(SCHEMA)
            .bind(vec![TABLE])
            .execute(&mut *drop_tx)
            .await?;
        let drop_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *drop_tx)
            .await?;
        let owner = h.owners[1];
        let (pid_tx, pid_rx) = tokio::sync::oneshot::channel();
        let admission = async {
            let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
                .fetch_one(&mut *writer)
                .await?;
            pid_tx.send(pid).unwrap();
            insert_fact(&mut writer, owner, SCHEMA, true, false).await?;
            writer.commit().await
        };
        let dropper = async {
            wait_until_blocked(&h.admin, pid_rx.await?, drop_pid).await?;
            drop_plan()
                .run_direct(None, drop_tx.as_mut(), false)
                .await?;
            drop_tx.commit().await?;
            Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
        };
        let (admitted, migration_result) = tokio::join!(admission, dropper);
        migration_result?;
        let refused = admitted.expect_err("queued writer must not leave a dangling stamp");
        assert_sqlstate(&refused, "23503");
        let leftovers: i64 = sqlx::query_scalar("SELECT count(*) FROM proxiima_core.memory WHERE schema_id=$1 OR sidecar_tables && ARRAY[$2]::text[]")
            .bind(SCHEMA).bind(TABLE).fetch_one(&h.admin).await?;
        assert_eq!(leftovers, 0);
    }
    Ok(())
}

#[tokio::test]
async fn a_fresh_empty_store_can_guard_multiple_tables_and_drop() -> TestResult {
    let h = Harness::new().await?;
    let mut connection = h.platform.acquire().await?;
    let mut tx = begin_migration_transaction(&mut connection).await?;
    sqlx::query(GUARD)
        .bind(SCHEMA)
        .bind(vec![TABLE, "proxiima_core.agent_derivation_v1"])
        .execute(&mut *tx)
        .await?;
    drop_plan().run_direct(None, tx.as_mut(), false).await?;
    tx.commit().await?;
    let ledger: i64 = sqlx::query_scalar("SELECT count(*) FROM public._sqlx_migrations_drop_guard")
        .fetch_one(&h.admin)
        .await?;
    assert_eq!(ledger, 1);
    Ok(())
}
